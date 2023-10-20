use crate::adapters::FeatureCollectionStreamExt;
use crate::processing::raster_vector_join::create_feature_aggregator;
use futures::stream::{once as once_stream, BoxStream};
use futures::{StreamExt, TryStreamExt};
use geoengine_datatypes::primitives::{
    BandSelection, BoundingBox2D, CacheHint, FeatureDataType, Geometry, RasterQueryRectangle,
    VectorQueryRectangle,
};
use geoengine_datatypes::util::arrow::ArrowTyped;
use std::marker::PhantomData;
use std::sync::Arc;

use geoengine_datatypes::raster::{DynamicRasterDataType, GridIndexAccess, RasterTile2D};
use geoengine_datatypes::{
    collections::FeatureCollectionModifications, primitives::TimeInterval, raster::Pixel,
};

use super::util::{CoveredPixels, PixelCoverCreator};
use crate::engine::{
    QueryContext, QueryProcessor, RasterQueryProcessor, TypedRasterQueryProcessor,
    VectorQueryProcessor,
};
use crate::util::Result;
use crate::{adapters::RasterStreamExt, error::Error};
use async_trait::async_trait;
use geoengine_datatypes::collections::GeometryCollection;
use geoengine_datatypes::collections::{FeatureCollection, FeatureCollectionInfos};

use super::aggregator::TypedAggregator;
use super::FeatureAggregationMethod;

pub struct RasterVectorJoinProcessor<G> {
    collection: Box<dyn VectorQueryProcessor<VectorType = FeatureCollection<G>>>,
    raster_processors: Vec<TypedRasterQueryProcessor>,
    column_names: Vec<String>,
    aggregation_method: FeatureAggregationMethod,
    ignore_no_data: bool,
}

impl<G> RasterVectorJoinProcessor<G>
where
    G: Geometry + ArrowTyped + 'static,
    FeatureCollection<G>: GeometryCollection + PixelCoverCreator<G>,
{
    pub fn new(
        collection: Box<dyn VectorQueryProcessor<VectorType = FeatureCollection<G>>>,
        raster_processors: Vec<TypedRasterQueryProcessor>,
        column_names: Vec<String>,
        aggregation_method: FeatureAggregationMethod,
        ignore_no_data: bool,
    ) -> Self {
        Self {
            collection,
            raster_processors,
            column_names,
            aggregation_method,
            ignore_no_data,
        }
    }

    fn process_collections<'a>(
        collection: BoxStream<'a, Result<FeatureCollection<G>>>,
        raster_processor: &'a TypedRasterQueryProcessor,
        new_column_name: &'a str,
        query: VectorQueryRectangle,
        ctx: &'a dyn QueryContext,
        aggregation_method: FeatureAggregationMethod,
        ignore_no_data: bool,
    ) -> BoxStream<'a, Result<FeatureCollection<G>>> {
        let stream = collection.and_then(move |collection| {
            Self::process_collection_chunk(
                collection,
                raster_processor,
                new_column_name,
                query,
                ctx,
                aggregation_method,
                ignore_no_data,
            )
        });

        stream
            .try_flatten()
            .merge_chunks(ctx.chunk_byte_size().into())
            .boxed()
    }

    async fn process_collection_chunk<'a>(
        collection: FeatureCollection<G>,
        raster_processor: &'a TypedRasterQueryProcessor,
        new_column_name: &'a str,
        query: VectorQueryRectangle,
        ctx: &'a dyn QueryContext,
        aggregation_method: FeatureAggregationMethod,
        ignore_no_data: bool,
    ) -> Result<BoxStream<'a, Result<FeatureCollection<G>>>> {
        if collection.is_empty() {
            log::debug!(
                "input collection is empty, returning empty collection, skipping raster query"
            );

            return Self::collection_with_new_null_column(
                &collection,
                new_column_name,
                raster_processor.raster_data_type().into(),
            );
        }

        let bbox = collection
            .bbox()
            .and_then(|bbox| bbox.intersection(&query.spatial_bounds));

        let time = collection
            .time_bounds()
            .and_then(|time| time.intersect(&query.time_interval));

        // TODO: also intersect with raster spatial / time bounds

        let (Some(spatial_bounds), Some(time_interval)) = (bbox, time) else {
            log::debug!(
                "spatial or temporal intersection is empty, returning the same collection, skipping raster query"
            );

            return Self::collection_with_new_null_column(
                &collection,
                new_column_name,
                raster_processor.raster_data_type().into(),
            );
        };

        let query = VectorQueryRectangle {
            spatial_bounds,
            time_interval,
            spatial_resolution: query.spatial_resolution,
            bands: BandSelection::default(), // TODO
        }
        .into();

        call_on_generic_raster_processor!(raster_processor, raster_processor => {
            Self::process_typed_collection_chunk(
                collection,
                raster_processor,
                new_column_name,
                query,
                ctx,
                aggregation_method,
                ignore_no_data,
            )
            .await
        })
    }

    fn collection_with_new_null_column<'a>(
        collection: &FeatureCollection<G>,
        new_column_name: &str,
        feature_data_type: FeatureDataType,
    ) -> Result<BoxStream<'a, Result<FeatureCollection<G>>>> {
        let collection = collection.add_column(
            new_column_name,
            feature_data_type.null_feature_data(collection.len()),
        )?;
        let collection_stream = once_stream(async move { Ok(collection) }).boxed();
        Ok(collection_stream)
    }

    async fn process_typed_collection_chunk<'a, P: Pixel>(
        collection: FeatureCollection<G>,
        raster_processor: &'a dyn RasterQueryProcessor<RasterType = P>,
        new_column_name: &'a str,
        query: RasterQueryRectangle,
        ctx: &'a dyn QueryContext,
        aggregation_method: FeatureAggregationMethod,
        ignore_no_data: bool,
    ) -> Result<BoxStream<'a, Result<FeatureCollection<G>>>> {
        let raster_query = raster_processor.raster_query(query, ctx).await?;

        let collection = Arc::new(collection);

        let collection_stream = raster_query
            .time_multi_fold(
                move || Ok(VectorRasterJoiner::new(aggregation_method, ignore_no_data)),
                move |accum, raster| {
                    let collection = collection.clone();
                    async move {
                        let accum = accum?;
                        let raster = raster?;
                        accum.extract_raster_values(&collection, &raster)
                    }
                },
            )
            .map(move |accum| accum?.into_collection(new_column_name));

        return Ok(collection_stream.boxed());
    }
}

struct JoinerState<G, C> {
    covered_pixels: C,
    aggregator: TypedAggregator,
    g: PhantomData<G>,
}

struct VectorRasterJoiner<G, C> {
    state: Option<JoinerState<G, C>>,
    aggregation_method: FeatureAggregationMethod,
    ignore_no_data: bool,
    cache_hint: CacheHint,
}

impl<G, C> VectorRasterJoiner<G, C>
where
    G: Geometry + ArrowTyped + 'static,
    C: CoveredPixels<G>,
    FeatureCollection<G>: PixelCoverCreator<G, C = C>,
{
    fn new(aggregation_method: FeatureAggregationMethod, ignore_no_data: bool) -> Self {
        // TODO: is it possible to do the initialization here?

        Self {
            state: None,
            aggregation_method,
            ignore_no_data,
            cache_hint: CacheHint::max_duration(),
        }
    }

    fn initialize<P: Pixel>(
        &mut self,
        collection: &FeatureCollection<G>,
        raster_time: &TimeInterval,
    ) -> Result<()> {
        // TODO: could be paralellized

        let (indexes, time_intervals): (Vec<_>, Vec<_>) = collection
            .time_intervals()
            .iter()
            .enumerate()
            .filter_map(|(i, time)| {
                time.intersect(raster_time)
                    .map(|time_intersection| (i, time_intersection))
            })
            .unzip();

        let mut valid = vec![false; collection.len()];
        for i in indexes {
            valid[i] = true;
        }

        let collection = collection.filter(valid)?;
        let collection = collection.replace_time(&time_intervals)?;

        self.state = Some(JoinerState::<G, C> {
            aggregator: create_feature_aggregator::<P>(
                collection.len(),
                self.aggregation_method,
                self.ignore_no_data,
            ),
            covered_pixels: collection.create_covered_pixels(),
            g: Default::default(),
        });

        Ok(())
    }

    fn extract_raster_values<P: Pixel>(
        mut self,
        initial_collection: &FeatureCollection<G>,
        raster: &RasterTile2D<P>,
    ) -> Result<Self> {
        let state = loop {
            if let Some(state) = &mut self.state {
                break state;
            }

            self.initialize::<P>(initial_collection, &raster.time)?;
        };
        let collection = &state.covered_pixels.collection_ref();
        let aggregator = &mut state.aggregator;
        let covered_pixels = &state.covered_pixels;

        for feature_index in 0..collection.len() {
            for grid_idx in covered_pixels.covered_pixels(feature_index, raster) {
                let Ok(value) = raster.get_at_grid_index(grid_idx) else {
                    continue; // not found in this raster tile
                };

                if let Some(data) = value {
                    aggregator.add_value(feature_index, data, 1);
                } else {
                    aggregator.add_null(feature_index);
                }
            }
        }

        self.cache_hint.merge_with(&raster.cache_hint);

        Ok(self)
    }

    fn into_collection(self, new_column_name: &str) -> Result<FeatureCollection<G>> {
        let Some(state) = self.state else {
            return Err(Error::EmptyInput); // TODO: maybe output empty dataset or just nulls
        };
        let mut new_collection = state
            .covered_pixels
            .collection()
            .add_column(new_column_name, state.aggregator.into_data())?;

        new_collection.cache_hint = self.cache_hint;

        Ok(new_collection)
    }
}

#[async_trait]
impl<G> QueryProcessor for RasterVectorJoinProcessor<G>
where
    G: Geometry + ArrowTyped + 'static,
    FeatureCollection<G>: GeometryCollection + PixelCoverCreator<G>,
{
    type Output = FeatureCollection<G>;
    type SpatialBounds = BoundingBox2D;

    async fn _query<'a>(
        &'a self,
        query: VectorQueryRectangle,
        ctx: &'a dyn QueryContext,
    ) -> Result<BoxStream<'a, Result<Self::Output>>> {
        let mut stream = self.collection.query(query, ctx).await?;

        for (raster_processor, new_column_name) in
            self.raster_processors.iter().zip(&self.column_names)
        {
            log::debug!("processing raster for new column {:?}", new_column_name);
            // TODO: spawn task
            stream = Self::process_collections(
                stream,
                raster_processor,
                new_column_name,
                query,
                ctx,
                self.aggregation_method,
                self.ignore_no_data,
            );
        }

        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::engine::{
        ChunkByteSize, MockExecutionContext, MockQueryContext, QueryProcessor, RasterOperator,
        RasterResultDescriptor, VectorOperator, WorkflowOperatorPath,
    };
    use crate::mock::{MockFeatureCollectionSource, MockRasterSource, MockRasterSourceParams};
    use crate::source::{GdalSource, GdalSourceParameters};
    use crate::util::gdal::add_ndvi_dataset;
    use geoengine_datatypes::collections::{
        ChunksEqualIgnoringCacheHint, MultiPointCollection, MultiPolygonCollection,
    };
    use geoengine_datatypes::primitives::CacheHint;
    use geoengine_datatypes::primitives::{BoundingBox2D, DateTime, FeatureData, MultiPolygon};
    use geoengine_datatypes::primitives::{Measurement, SpatialResolution};
    use geoengine_datatypes::primitives::{MultiPoint, TimeInterval};
    use geoengine_datatypes::raster::{
        Grid2D, RasterDataType, TileInformation, TilingSpecification,
    };
    use geoengine_datatypes::spatial_reference::SpatialReference;
    use geoengine_datatypes::util::test::TestDefault;

    #[tokio::test]
    async fn both_instant() {
        let time_instant =
            TimeInterval::new_instant(DateTime::new_utc(2014, 1, 1, 0, 0, 0)).unwrap();

        let points = MockFeatureCollectionSource::single(
            MultiPointCollection::from_data(
                MultiPoint::many(vec![
                    vec![(-13.95, 20.05)],
                    vec![(-14.05, 20.05)],
                    vec![(-13.95, 19.95)],
                    vec![(-14.05, 19.95)],
                    vec![(-13.95, 19.95), (-14.05, 19.95)],
                ])
                .unwrap(),
                vec![time_instant; 5],
                Default::default(),
                CacheHint::default(),
            )
            .unwrap(),
        )
        .boxed();

        let mut execution_context = MockExecutionContext::test_default();

        let raster_source = GdalSource {
            params: GdalSourceParameters {
                data: add_ndvi_dataset(&mut execution_context),
            },
        }
        .boxed();

        let points = points
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_point()
            .unwrap();

        let rasters = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![rasters],
            vec!["ndvi".to_owned()],
            FeatureAggregationMethod::First,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((-180., -90.).into(), (180., 90.).into())
                        .unwrap(),
                    time_interval: time_instant,
                    spatial_resolution: SpatialResolution::new(0.1, 0.1).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPointCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPointCollection::from_slices(
                &MultiPoint::many(vec![
                    vec![(-13.95, 20.05)],
                    vec![(-14.05, 20.05)],
                    vec![(-13.95, 19.95)],
                    vec![(-14.05, 19.95)],
                    vec![(-13.95, 19.95), (-14.05, 19.95)],
                ])
                .unwrap(),
                &[time_instant; 5],
                // these values are taken from loading the tiff in QGIS
                &[("ndvi", FeatureData::Int(vec![54, 55, 51, 55, 51]))],
            )
            .unwrap()
        ));
    }

    #[tokio::test]
    async fn points_instant() {
        let points = MockFeatureCollectionSource::single(
            MultiPointCollection::from_data(
                MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                vec![TimeInterval::new_instant(DateTime::new_utc(2014, 1, 1, 0, 0, 0)).unwrap(); 4],
                Default::default(),
                CacheHint::default(),
            )
            .unwrap(),
        )
        .boxed();

        let mut execution_context = MockExecutionContext::test_default();

        let raster_source = GdalSource {
            params: GdalSourceParameters {
                data: add_ndvi_dataset(&mut execution_context),
            },
        }
        .boxed();

        let points = points
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_point()
            .unwrap();

        let rasters = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![rasters],
            vec!["ndvi".to_owned()],
            FeatureAggregationMethod::First,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((-180., -90.).into(), (180., 90.).into())
                        .unwrap(),
                    time_interval: TimeInterval::new(
                        DateTime::new_utc(2014, 1, 1, 0, 0, 0),
                        DateTime::new_utc(2014, 3, 1, 0, 0, 0),
                    )
                    .unwrap(),
                    spatial_resolution: SpatialResolution::new(0.1, 0.1).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPointCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPointCollection::from_slices(
                &MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                &[TimeInterval::new_instant(DateTime::new_utc(2014, 1, 1, 0, 0, 0)).unwrap(); 4],
                // these values are taken from loading the tiff in QGIS
                &[("ndvi", FeatureData::Int(vec![54, 55, 51, 55]))],
            )
            .unwrap()
        ));
    }

    #[tokio::test]
    async fn raster_instant() {
        let points = MockFeatureCollectionSource::single(
            MultiPointCollection::from_data(
                MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                vec![
                    TimeInterval::new(
                        DateTime::new_utc(2014, 1, 1, 0, 0, 0),
                        DateTime::new_utc(2014, 3, 1, 0, 0, 0),
                    )
                    .unwrap();
                    4
                ],
                Default::default(),
                CacheHint::default(),
            )
            .unwrap(),
        )
        .boxed();

        let mut execution_context = MockExecutionContext::test_default();

        let raster_source = GdalSource {
            params: GdalSourceParameters {
                data: add_ndvi_dataset(&mut execution_context),
            },
        }
        .boxed();

        let points = points
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_point()
            .unwrap();

        let rasters = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![rasters],
            vec!["ndvi".to_owned()],
            FeatureAggregationMethod::First,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((-180., -90.).into(), (180., 90.).into())
                        .unwrap(),
                    time_interval: TimeInterval::new_instant(DateTime::new_utc(
                        2014, 1, 1, 0, 0, 0,
                    ))
                    .unwrap(),
                    spatial_resolution: SpatialResolution::new(0.1, 0.1).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPointCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPointCollection::from_slices(
                &MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                &[TimeInterval::new(
                    DateTime::new_utc(2014, 1, 1, 0, 0, 0),
                    DateTime::new_utc(2014, 2, 1, 0, 0, 0),
                )
                .unwrap(); 4],
                // these values are taken from loading the tiff in QGIS
                &[("ndvi", FeatureData::Int(vec![54, 55, 51, 55]))],
            )
            .unwrap()
        ));
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn both_ranges() {
        let points = MockFeatureCollectionSource::single(
            MultiPointCollection::from_data(
                MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                vec![
                    TimeInterval::new(
                        DateTime::new_utc(2014, 1, 1, 0, 0, 0),
                        DateTime::new_utc(2014, 3, 1, 0, 0, 0),
                    )
                    .unwrap();
                    4
                ],
                Default::default(),
                CacheHint::default(),
            )
            .unwrap(),
        )
        .boxed();

        let mut execution_context = MockExecutionContext::test_default();

        let raster_source = GdalSource {
            params: GdalSourceParameters {
                data: add_ndvi_dataset(&mut execution_context),
            },
        }
        .boxed();

        let points = points
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_point()
            .unwrap();

        let rasters = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![rasters],
            vec!["ndvi".to_owned()],
            FeatureAggregationMethod::First,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((-180., -90.).into(), (180., 90.).into())
                        .unwrap(),
                    time_interval: TimeInterval::new(
                        DateTime::new_utc(2014, 1, 1, 0, 0, 0),
                        DateTime::new_utc(2014, 3, 1, 0, 0, 0),
                    )
                    .unwrap(),
                    spatial_resolution: SpatialResolution::new(0.1, 0.1).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPointCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        let t1 = TimeInterval::new(
            DateTime::new_utc(2014, 1, 1, 0, 0, 0),
            DateTime::new_utc(2014, 2, 1, 0, 0, 0),
        )
        .unwrap();
        let t2 = TimeInterval::new(
            DateTime::new_utc(2014, 2, 1, 0, 0, 0),
            DateTime::new_utc(2014, 3, 1, 0, 0, 0),
        )
        .unwrap();
        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPointCollection::from_slices(
                &MultiPoint::many(vec![
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                    (-13.95, 20.05),
                    (-14.05, 20.05),
                    (-13.95, 19.95),
                    (-14.05, 19.95),
                ])
                .unwrap(),
                &[t1, t1, t1, t1, t2, t2, t2, t2],
                // these values are taken from loading the tiff in QGIS
                &[(
                    "ndvi",
                    FeatureData::Int(vec![54, 55, 51, 55, 52, 55, 50, 53])
                )],
            )
            .unwrap()
        ));
    }

    #[tokio::test]
    #[allow(clippy::float_cmp)]
    #[allow(clippy::too_many_lines)]
    async fn extract_raster_values_two_spatial_tiles_per_time_step_mean() {
        let raster_tile_a_0 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(0, 10).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 0].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![6, 5, 4, 3, 2, 1])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_a_1 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(0, 10).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 1].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![60, 50, 40, 30, 20, 10])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_b_0 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(10, 20).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 0].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![1, 2, 3, 4, 5, 6])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_b_1 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(10, 20).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 1].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![10, 20, 30, 40, 50, 60])
                .unwrap()
                .into(),
            CacheHint::default(),
        );

        let raster_source = MockRasterSource {
            params: MockRasterSourceParams {
                data: vec![
                    raster_tile_a_0,
                    raster_tile_a_1,
                    raster_tile_b_0,
                    raster_tile_b_1,
                ],
                result_descriptor: RasterResultDescriptor {
                    data_type: RasterDataType::U8,
                    spatial_reference: SpatialReference::epsg_4326().into(),
                    measurement: Measurement::Unitless,
                    time: None,
                    bbox: None,
                    resolution: None,
                    bands: 1,
                },
            },
        }
        .boxed();

        let execution_context = MockExecutionContext::new_with_tiling_spec(
            TilingSpecification::new((0., 0.).into(), [3, 2].into()),
        );

        let raster = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let points = MultiPointCollection::from_data(
            MultiPoint::many(vec![
                vec![(0.0, 0.0), (2.0, 0.0)],
                vec![(1.0, 0.0), (3.0, 0.0)],
            ])
            .unwrap(),
            vec![TimeInterval::default(); 2],
            Default::default(),
            CacheHint::default(),
        )
        .unwrap();

        let points = MockFeatureCollectionSource::single(points).boxed();

        let points = points
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_point()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![raster],
            vec!["foo".to_owned()],
            FeatureAggregationMethod::Mean,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((0.0, -3.0).into(), (4.0, 0.0).into())
                        .unwrap(),
                    time_interval: TimeInterval::new_unchecked(0, 20),
                    spatial_resolution: SpatialResolution::new(1., 1.).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPointCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        let t1 = TimeInterval::new(0, 10).unwrap();
        let t2 = TimeInterval::new(10, 20).unwrap();

        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPointCollection::from_slices(
                &MultiPoint::many(vec![
                    vec![(0.0, 0.0), (2.0, 0.0)],
                    vec![(1.0, 0.0), (3.0, 0.0)],
                    vec![(0.0, 0.0), (2.0, 0.0)],
                    vec![(1.0, 0.0), (3.0, 0.0)],
                ])
                .unwrap(),
                &[t1, t1, t2, t2],
                &[(
                    "foo",
                    FeatureData::Float(vec![
                        (6. + 60.) / 2.,
                        (5. + 50.) / 2.,
                        (1. + 10.) / 2.,
                        (2. + 20.) / 2.
                    ])
                )],
            )
            .unwrap()
        ));
    }

    #[tokio::test]
    #[allow(clippy::float_cmp)]
    #[allow(clippy::too_many_lines)]
    async fn polygons() {
        let raster_tile_a_0 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(0, 10).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 0].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![6, 5, 4, 3, 2, 1])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_a_1 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(0, 10).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 1].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![60, 50, 40, 30, 20, 10])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_a_2 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(0, 10).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 2].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![600, 500, 400, 300, 200, 100])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_b_0 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(10, 20).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 0].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![1, 2, 3, 4, 5, 6])
                .unwrap()
                .into(),
            CacheHint::default(),
        );
        let raster_tile_b_1 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(10, 20).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 1].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![10, 20, 30, 40, 50, 60])
                .unwrap()
                .into(),
            CacheHint::default(),
        );

        let raster_tile_b_2 = RasterTile2D::new_with_tile_info(
            TimeInterval::new(10, 20).unwrap(),
            TileInformation {
                global_geo_transform: TestDefault::test_default(),
                global_tile_position: [0, 2].into(),
                tile_size_in_pixels: [3, 2].into(),
            },
            Grid2D::new([3, 2].into(), vec![100, 200, 300, 400, 500, 600])
                .unwrap()
                .into(),
            CacheHint::default(),
        );

        let raster_source = MockRasterSource {
            params: MockRasterSourceParams {
                data: vec![
                    raster_tile_a_0,
                    raster_tile_a_1,
                    raster_tile_a_2,
                    raster_tile_b_0,
                    raster_tile_b_1,
                    raster_tile_b_2,
                ],
                result_descriptor: RasterResultDescriptor {
                    data_type: RasterDataType::U16,
                    spatial_reference: SpatialReference::epsg_4326().into(),
                    measurement: Measurement::Unitless,
                    time: None,
                    bbox: None,
                    resolution: None,
                    bands: 1,
                },
            },
        }
        .boxed();

        let execution_context = MockExecutionContext::new_with_tiling_spec(
            TilingSpecification::new((0., 0.).into(), [3, 2].into()),
        );

        let raster = raster_source
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap();

        let polygons = MultiPolygonCollection::from_data(
            vec![MultiPolygon::new(vec![vec![vec![
                (0.5, -0.5).into(),
                (4., -1.).into(),
                (0.5, -2.5).into(),
                (0.5, -0.5).into(),
            ]]])
            .unwrap()],
            vec![TimeInterval::default(); 1],
            Default::default(),
            CacheHint::default(),
        )
        .unwrap();

        let polygons = MockFeatureCollectionSource::single(polygons).boxed();

        let points = polygons
            .initialize(WorkflowOperatorPath::initialize_root(), &execution_context)
            .await
            .unwrap()
            .query_processor()
            .unwrap()
            .multi_polygon()
            .unwrap();

        let processor = RasterVectorJoinProcessor::new(
            points,
            vec![raster],
            vec!["foo".to_owned()],
            FeatureAggregationMethod::Mean,
            false,
        );

        let mut result = processor
            .query(
                VectorQueryRectangle {
                    spatial_bounds: BoundingBox2D::new((0.0, -3.0).into(), (4.0, 0.0).into())
                        .unwrap(),
                    time_interval: TimeInterval::new_unchecked(0, 20),
                    spatial_resolution: SpatialResolution::new(1., 1.).unwrap(),
                    bands: BandSelection::default(), // TODO
                },
                &MockQueryContext::new(ChunkByteSize::MAX),
            )
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<MultiPolygonCollection>>()
            .await;

        assert_eq!(result.len(), 1);

        let result = result.remove(0);

        let t1 = TimeInterval::new(0, 10).unwrap();
        let t2 = TimeInterval::new(10, 20).unwrap();

        assert!(result.chunks_equal_ignoring_cache_hint(
            &MultiPolygonCollection::from_slices(
                &[
                    MultiPolygon::new(vec![vec![vec![
                        (0.5, -0.5).into(),
                        (4., -1.).into(),
                        (0.5, -2.5).into(),
                        (0.5, -0.5).into(),
                    ]]])
                    .unwrap(),
                    MultiPolygon::new(vec![vec![vec![
                        (0.5, -0.5).into(),
                        (4., -1.).into(),
                        (0.5, -2.5).into(),
                        (0.5, -0.5).into(),
                    ]]])
                    .unwrap()
                ],
                &[t1, t2],
                &[(
                    "foo",
                    FeatureData::Float(vec![
                        (3. + 1. + 40. + 30. + 400.) / 5.,
                        (4. + 6. + 30. + 40. + 300.) / 5.
                    ])
                )],
            )
            .unwrap()
        ));
    }
}

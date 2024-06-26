use super::{
    AxisAlignedRectangle, BoundingBox2D, SpatialPartition2D, SpatialPartitioned, SpatialResolution,
    TimeInterval,
};
use crate::{
    error::{DuplicateBandInQueryBandSelection, QueryBandSelectionMustNotBeEmpty},
    util::Result,
};
use serde::{Deserialize, Serialize};
use snafu::ensure;

/// A spatio-temporal rectangle with a specified resolution and the selected bands
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryRectangle<
    SpatialBounds: AxisAlignedRectangle,
    AttributeSelection: QueryAttributeSelection,
> {
    pub spatial_bounds: SpatialBounds,
    pub time_interval: TimeInterval,
    pub spatial_resolution: SpatialResolution,
    pub attributes: AttributeSelection,
}

pub trait QueryAttributeSelection: Clone + Send + Sync {}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BandSelection(Vec<u32>);

impl BandSelection {
    pub fn new(bands: Vec<u32>) -> Result<Self> {
        fn has_no_duplicates<T: std::hash::Hash + std::cmp::Eq>(vec: &[T]) -> bool {
            let set: std::collections::HashSet<_> = vec.iter().collect();
            set.len() == vec.len()
        }

        ensure!(has_no_duplicates(&bands), DuplicateBandInQueryBandSelection);
        ensure!(!bands.is_empty(), QueryBandSelectionMustNotBeEmpty);

        Ok(Self(bands))
    }

    pub fn new_unchecked(bands: Vec<u32>) -> Self {
        Self(bands)
    }

    pub fn first() -> Self {
        Self(vec![0])
    }

    pub fn first_n(n: u32) -> Self {
        Self((0..n).collect())
    }

    pub fn new_single(band: u32) -> Self {
        Self(vec![band])
    }

    pub fn count(&self) -> u32 {
        self.0.len() as u32
    }

    pub fn as_slice(&self) -> &[u32] {
        &self.0
    }

    pub fn as_vec(&self) -> Vec<u32> {
        self.0.clone()
    }
}

impl From<u32> for BandSelection {
    fn from(value: u32) -> Self {
        Self(vec![value])
    }
}

impl TryFrom<Vec<u32>> for BandSelection {
    type Error = crate::error::Error;

    fn try_from(value: Vec<u32>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const N: usize> TryFrom<[u32; N]> for BandSelection {
    type Error = crate::error::Error;

    fn try_from(value: [u32; N]) -> Result<Self, Self::Error> {
        Self::new(value.to_vec())
    }
}

impl QueryAttributeSelection for BandSelection {}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColumnSelection {}

impl ColumnSelection {
    pub fn all() -> Self {
        Self {}
    }
}

impl QueryAttributeSelection for ColumnSelection {}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlotSeriesSelection {}

impl PlotSeriesSelection {
    pub fn all() -> Self {
        Self {}
    }
}

impl QueryAttributeSelection for PlotSeriesSelection {}

pub type VectorQueryRectangle = QueryRectangle<BoundingBox2D, ColumnSelection>;
pub type RasterQueryRectangle = QueryRectangle<SpatialPartition2D, BandSelection>;
pub type PlotQueryRectangle = QueryRectangle<BoundingBox2D, PlotSeriesSelection>;

impl RasterQueryRectangle {
    pub fn from_qrect_and_bands<A>(
        query: &QueryRectangle<BoundingBox2D, A>,
        bands: BandSelection,
    ) -> Self
    where
        A: QueryAttributeSelection,
        QueryRectangle<BoundingBox2D, A>: SpatialPartitioned,
    {
        Self {
            spatial_bounds: query.spatial_partition(),
            time_interval: query.time_interval,
            spatial_resolution: query.spatial_resolution,
            attributes: bands,
        }
    }

    #[must_use]
    pub fn select_bands(&self, bands: BandSelection) -> Self {
        Self {
            spatial_bounds: self.spatial_bounds,
            time_interval: self.time_interval,
            spatial_resolution: self.spatial_resolution,
            attributes: bands,
        }
    }
}

impl SpatialPartitioned for QueryRectangle<BoundingBox2D, ColumnSelection> {
    fn spatial_partition(&self) -> SpatialPartition2D {
        SpatialPartition2D::with_bbox_and_resolution(self.spatial_bounds, self.spatial_resolution)
    }
}

impl SpatialPartitioned for QueryRectangle<BoundingBox2D, PlotSeriesSelection> {
    fn spatial_partition(&self) -> SpatialPartition2D {
        SpatialPartition2D::with_bbox_and_resolution(self.spatial_bounds, self.spatial_resolution)
    }
}

impl SpatialPartitioned for QueryRectangle<SpatialPartition2D, BandSelection> {
    fn spatial_partition(&self) -> SpatialPartition2D {
        self.spatial_bounds
    }
}

impl From<QueryRectangle<BoundingBox2D, ColumnSelection>>
    for QueryRectangle<BoundingBox2D, PlotSeriesSelection>
{
    fn from(value: QueryRectangle<BoundingBox2D, ColumnSelection>) -> Self {
        Self {
            spatial_bounds: value.spatial_bounds,
            time_interval: value.time_interval,
            spatial_resolution: value.spatial_resolution,
            attributes: value.attributes.into(),
        }
    }
}

impl From<QueryRectangle<BoundingBox2D, PlotSeriesSelection>>
    for QueryRectangle<BoundingBox2D, ColumnSelection>
{
    fn from(value: QueryRectangle<BoundingBox2D, PlotSeriesSelection>) -> Self {
        Self {
            spatial_bounds: value.spatial_bounds,
            time_interval: value.time_interval,
            spatial_resolution: value.spatial_resolution,
            attributes: value.attributes.into(),
        }
    }
}

impl From<ColumnSelection> for PlotSeriesSelection {
    fn from(_: ColumnSelection) -> Self {
        Self {}
    }
}

impl From<PlotSeriesSelection> for ColumnSelection {
    fn from(_: PlotSeriesSelection) -> Self {
        Self {}
    }
}

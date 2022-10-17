use crate::engine::{
    CreateSpan, InitializedRasterOperator, InitializedVectorOperator, QueryContext, QueryProcessor,
    RasterResultDescriptor, TypedRasterQueryProcessor, TypedVectorQueryProcessor,
    VectorResultDescriptor,
};
use crate::pro::adapters::stream_statistics_adapter::StreamStatisticsAdapter;
use crate::util::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use geoengine_datatypes::primitives::{AxisAlignedRectangle, QueryRectangle};

pub struct InitializedProcessorStatistics<S> {
    source: S,
    id: String,
    span: Box<dyn CreateSpan>,
}

impl<S> InitializedProcessorStatistics<S> {
    pub fn statistics_with_id(source: S, id: String, span: Box<dyn CreateSpan>) -> Self {
        Self { source, id, span }
    }
}

impl InitializedRasterOperator
    for InitializedProcessorStatistics<Box<dyn InitializedRasterOperator>>
{
    fn result_descriptor(&self) -> &RasterResultDescriptor {
        tracing::debug!(event = "raster result descriptor", id = self.id);
        self.source.result_descriptor()
    }

    fn query_processor(&self) -> Result<TypedRasterQueryProcessor> {
        tracing::debug!(event = "query processor", id = self.id);
        let processor_result = self.source.query_processor();
        match processor_result {
            Ok(p) => {
                let res_processor = match p {
                    TypedRasterQueryProcessor::U8(p) => TypedRasterQueryProcessor::U8(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::U16(p) => TypedRasterQueryProcessor::U16(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::U32(p) => TypedRasterQueryProcessor::U32(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::U64(p) => TypedRasterQueryProcessor::U64(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::I8(p) => TypedRasterQueryProcessor::I8(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::I16(p) => TypedRasterQueryProcessor::I16(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::I32(p) => TypedRasterQueryProcessor::I32(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::I64(p) => TypedRasterQueryProcessor::I64(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::F32(p) => TypedRasterQueryProcessor::F32(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                    TypedRasterQueryProcessor::F64(p) => TypedRasterQueryProcessor::F64(Box::new(
                        ProcessorStatisticsProcessor::statistics_with_id(
                            p,
                            self.id.clone(),
                            self.span.clone(),
                        ),
                    )),
                };
                tracing::debug!(event = "query processor created", id = self.id);
                Ok(res_processor)
            }
            Err(err) => {
                tracing::debug!(event = "query processor failed", id = self.id);
                Err(err)
            }
        }
    }
}

impl InitializedVectorOperator
    for InitializedProcessorStatistics<Box<dyn InitializedVectorOperator>>
{
    fn result_descriptor(&self) -> &VectorResultDescriptor {
        tracing::debug!(event = "vector result descriptor", id = self.id);
        self.source.result_descriptor()
    }

    fn query_processor(&self) -> Result<TypedVectorQueryProcessor> {
        tracing::debug!(event = "query processor", id = self.id);
        let processor_result = self.source.query_processor();
        match processor_result {
            Ok(p) => {
                let result = map_typed_query_processor!(
                    p,
                    p => Box::new(ProcessorStatisticsProcessor::statistics_with_id(p, self.id.clone(),
                    self.span.clone()))
                );
                tracing::debug!(event = "query processor created", id = self.id);
                Ok(result)
            }
            Err(err) => {
                tracing::debug!(event = "query processor failed", id = self.id);
                Err(err)
            }
        }
    }
}

struct ProcessorStatisticsProcessor<Q, T>
where
    Q: QueryProcessor<Output = T>,
{
    processor: Q,
    id: String,
    span: Box<dyn CreateSpan>,
}

impl<Q, T> ProcessorStatisticsProcessor<Q, T>
where
    Q: QueryProcessor<Output = T> + Sized,
{
    pub fn statistics_with_id(processor: Q, id: String, span: Box<dyn CreateSpan>) -> Self {
        ProcessorStatisticsProcessor {
            processor,
            id,
            span,
        }
    }
}

#[async_trait]
impl<Q, T, S> QueryProcessor for ProcessorStatisticsProcessor<Q, T>
where
    Q: QueryProcessor<Output = T, SpatialBounds = S>,
    S: AxisAlignedRectangle + Send + Sync + 'static,
    T: Send,
{
    type Output = T;
    type SpatialBounds = S;

    async fn query<'a>(
        &'a self,
        query: QueryRectangle<Self::SpatialBounds>,
        ctx: &'a dyn QueryContext,
    ) -> Result<BoxStream<'a, Result<Self::Output>>> {
        tracing::trace!(event = "query", id = self.id);
        let stream_result = self.processor.query(query, ctx).await;
        tracing::debug!(event = "query ready", id = self.id);
        match stream_result {
            Ok(stream) => {
                tracing::debug!(event = "query ok", id = self.id);
                Ok(StreamStatisticsAdapter::statistics_with_id(
                    stream,
                    self.id.clone(),
                    self.span.create_span(),
                )
                .boxed())
            }
            Err(err) => {
                tracing::debug!(event = "query error", id = self.id);
                Err(err)
            }
        }
    }
}

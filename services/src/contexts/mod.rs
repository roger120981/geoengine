use crate::datasets::upload::Volume;
use crate::error::Result;
use crate::layers::listing::{DatasetLayerCollectionProvider, LayerCollectionProvider};
use crate::layers::storage::{LayerDb, LayerProviderDb};
use crate::machine_learning::ml_model::{MlModel, MlModelDb};
use crate::tasks::{TaskContext, TaskManager};
use crate::{projects::ProjectDb, workflows::registry::WorkflowRegistry};
use async_trait::async_trait;
use geoengine_datatypes::ml_model::MlModelId;
use geoengine_datatypes::primitives::{RasterQueryRectangle, VectorQueryRectangle};
use rayon::ThreadPool;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

mod in_memory;
mod postgres;
mod session;
mod simple_context;

use crate::datasets::storage::DatasetDb;

use geoengine_datatypes::dataset::{DataId, DataProviderId, ExternalDataId, LayerId, NamedData};

use geoengine_datatypes::raster::TilingSpecification;
use geoengine_operators::engine::{
    ChunkByteSize, CreateSpan, ExecutionContext, InitializedPlotOperator,
    InitializedVectorOperator, MetaData, MetaDataProvider, QueryAbortRegistration,
    QueryAbortTrigger, QueryContext, QueryContextExtensions, RasterResultDescriptor,
    VectorResultDescriptor, WorkflowOperatorPath,
};
use geoengine_operators::mock::MockDatasetDataSourceLoadingInfo;
use geoengine_operators::source::{GdalLoadingInfo, OgrSourceDataset};

pub use in_memory::{InMemoryContext, InMemoryDb, InMemorySessionContext};
pub use postgres::{PostgresContext, PostgresDb, PostgresSessionContext};
pub use session::{MockableSession, Session, SessionId, SimpleSession};
pub use simple_context::SimpleApplicationContext;

pub type Db<T> = Arc<RwLock<T>>;

/// The application context bundles shared resources.
/// It is passed to API handlers and allows creating a session context that provides access to resources.
#[async_trait]
pub trait ApplicationContext: 'static + Send + Sync + Clone {
    type SessionContext: SessionContext;
    type Session: Session + Clone;

    /// Create a new session context for the given session.
    fn session_context(&self, session: Self::Session) -> Self::SessionContext;

    /// Load a session by its id
    async fn session_by_id(&self, session_id: SessionId) -> Result<Self::Session>;
}

/// The session context bundles resources that are specific to a session.
#[async_trait]
pub trait SessionContext: 'static + Send + Sync + Clone {
    type Session: Session + Clone;
    type GeoEngineDB: GeoEngineDb;
    type QueryContext: QueryContext;
    type ExecutionContext: ExecutionContext;
    type TaskContext: TaskContext;
    type TaskManager: TaskManager<Self::TaskContext>;

    /// Get the db for accessing resources
    fn db(&self) -> Self::GeoEngineDB;

    /// Get the task manager for accessing tasks
    fn tasks(&self) -> Self::TaskManager;

    /// Create a new query context for executing queries on processors
    fn query_context(&self) -> Result<Self::QueryContext>;

    /// Create a new execution context initializing operators
    fn execution_context(&self) -> Result<Self::ExecutionContext>;

    /// Get the list of available data volumes
    fn volumes(&self) -> Result<Vec<Volume>>;

    /// Get the current session
    fn session(&self) -> &Self::Session;
}

/// The trait for accessing all resources
pub trait GeoEngineDb:
    DatasetDb
    + LayerDb
    + LayerProviderDb
    + LayerCollectionProvider
    + DatasetLayerCollectionProvider
    + ProjectDb
    + WorkflowRegistry
    + MlModelDb
{
}

pub struct QueryContextImpl {
    chunk_byte_size: ChunkByteSize,
    thread_pool: Arc<ThreadPool>,
    extensions: QueryContextExtensions,
    abort_registration: QueryAbortRegistration,
    abort_trigger: Option<QueryAbortTrigger>,
}

impl QueryContextImpl {
    pub fn new(chunk_byte_size: ChunkByteSize, thread_pool: Arc<ThreadPool>) -> Self {
        let (abort_registration, abort_trigger) = QueryAbortRegistration::new();
        QueryContextImpl {
            chunk_byte_size,
            thread_pool,
            extensions: Default::default(),
            abort_registration,
            abort_trigger: Some(abort_trigger),
        }
    }

    pub fn new_with_extensions(
        chunk_byte_size: ChunkByteSize,
        thread_pool: Arc<ThreadPool>,
        extensions: QueryContextExtensions,
    ) -> Self {
        let (abort_registration, abort_trigger) = QueryAbortRegistration::new();
        QueryContextImpl {
            chunk_byte_size,
            thread_pool,
            extensions,
            abort_registration,
            abort_trigger: Some(abort_trigger),
        }
    }
}

impl QueryContext for QueryContextImpl {
    fn chunk_byte_size(&self) -> ChunkByteSize {
        self.chunk_byte_size
    }

    fn thread_pool(&self) -> &Arc<ThreadPool> {
        &self.thread_pool
    }

    fn extensions(&self) -> &QueryContextExtensions {
        &self.extensions
    }

    fn abort_registration(&self) -> &QueryAbortRegistration {
        &self.abort_registration
    }

    fn abort_trigger(&mut self) -> geoengine_operators::util::Result<QueryAbortTrigger> {
        self.abort_trigger
            .take()
            .ok_or(geoengine_operators::error::Error::AbortTriggerAlreadyUsed)
    }
}

pub struct ExecutionContextImpl<D>
where
    D: DatasetDb + LayerProviderDb,
{
    db: D,
    thread_pool: Arc<ThreadPool>,
    tiling_specification: TilingSpecification,
}

impl<D> ExecutionContextImpl<D>
where
    D: DatasetDb + LayerProviderDb,
{
    pub fn new(
        db: D,
        thread_pool: Arc<ThreadPool>,
        tiling_specification: TilingSpecification,
    ) -> Self {
        Self {
            db,
            thread_pool,
            tiling_specification,
        }
    }
}

#[async_trait::async_trait]
impl<D> ExecutionContext for ExecutionContextImpl<D>
where
    D: DatasetDb
        + MetaDataProvider<
            MockDatasetDataSourceLoadingInfo,
            VectorResultDescriptor,
            VectorQueryRectangle,
        > + MetaDataProvider<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>
        + MetaDataProvider<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>
        + LayerProviderDb
        + MlModelDb,
{
    fn thread_pool(&self) -> &Arc<ThreadPool> {
        &self.thread_pool
    }

    fn tiling_specification(&self) -> TilingSpecification {
        self.tiling_specification
    }

    fn wrap_initialized_raster_operator(
        &self,
        op: Box<dyn geoengine_operators::engine::InitializedRasterOperator>,
        _span: CreateSpan,
        _path: WorkflowOperatorPath,
    ) -> Box<dyn geoengine_operators::engine::InitializedRasterOperator> {
        op
    }

    fn wrap_initialized_vector_operator(
        &self,
        op: Box<dyn InitializedVectorOperator>,
        _span: CreateSpan,
        _path: WorkflowOperatorPath,
    ) -> Box<dyn InitializedVectorOperator> {
        op
    }

    fn wrap_initialized_plot_operator(
        &self,
        op: Box<dyn InitializedPlotOperator>,
        _span: CreateSpan,
        _path: WorkflowOperatorPath,
    ) -> Box<dyn InitializedPlotOperator> {
        op
    }

    /// Loads a machine learning model with the specified `model_id` from the database.
    ///
    /// # Arguments
    ///
    /// * `model_id` - The ID of the machine learning model to load.
    ///
    /// # Returns
    ///
    /// Returns a `Result` containing the loaded machine learning model content as a `String`.
    /// If the model ID is not found in the database, an `UnknownModelId` error is returned.
    ///
    async fn load_ml_model(
        &self,
        model_id: MlModelId,
    ) -> geoengine_operators::util::Result<String> {
        let db = &self.db;

        let ml_model_from_db = db.load_ml_model(model_id).await.map_err(|_| {
            geoengine_operators::error::Error::UnknownModelId {
                id: model_id.to_string(),
            }
        })?;

        Ok(ml_model_from_db.model_content)
    }

    /// This method is meant to write a ml model to disk.
    /// The provided path for the model has to exist.
    async fn store_ml_model_in_db(
        &mut self,
        model_id: MlModelId,
        model_content: String,
    ) -> geoengine_operators::util::Result<()> {
        let model = MlModel {
            model_id,
            model_content,
        };

        // TODO: add routine or error, if a given id would overwrite an existing model
        self.db
            .store_ml_model(model)
            .await
            .map_err(|_| geoengine_operators::error::Error::CouldNotStoreMlModelInDb)
    }

    async fn resolve_named_data(
        &self,
        data: &NamedData,
    ) -> Result<DataId, geoengine_operators::error::Error> {
        if let Some(provider) = &data.provider {
            // TODO: resolve provider name to provider id
            let provider_id = DataProviderId::from_str(provider)?;

            let data_id = ExternalDataId {
                provider_id,
                layer_id: LayerId(data.name.clone()),
            };

            return Ok(data_id.into());
        }

        let dataset_id = self
            .db
            .resolve_dataset_name_to_id(&data.into())
            .await
            .map_err(
                |source| geoengine_operators::error::Error::CannotResolveDatasetName {
                    name: data.clone(),
                    source: Box::new(source),
                },
            )?;

        Ok(dataset_id.into())
    }
}

// TODO: use macro(?) for delegating meta_data function to DatasetDB to avoid redundant code
#[async_trait]
impl<D>
    MetaDataProvider<MockDatasetDataSourceLoadingInfo, VectorResultDescriptor, VectorQueryRectangle>
    for ExecutionContextImpl<D>
where
    D: DatasetDb
        + MetaDataProvider<
            MockDatasetDataSourceLoadingInfo,
            VectorResultDescriptor,
            VectorQueryRectangle,
        > + LayerProviderDb,
{
    async fn meta_data(
        &self,
        data_id: &DataId,
    ) -> Result<
        Box<
            dyn MetaData<
                MockDatasetDataSourceLoadingInfo,
                VectorResultDescriptor,
                VectorQueryRectangle,
            >,
        >,
        geoengine_operators::error::Error,
    > {
        match data_id {
            DataId::Internal { dataset_id: _ } => {
                self.db.meta_data(&data_id.clone()).await.map_err(|e| {
                    geoengine_operators::error::Error::LoadingInfo {
                        source: Box::new(e),
                    }
                })
            }
            DataId::External(external) => {
                self.db
                    .load_layer_provider(external.provider_id.into())
                    .await
                    .map_err(|e| geoengine_operators::error::Error::DatasetMetaData {
                        source: Box::new(e),
                    })?
                    .meta_data(data_id)
                    .await
            }
        }
    }
}

// TODO: use macro(?) for delegating meta_data function to DatasetDB to avoid redundant code
#[async_trait]
impl<D> MetaDataProvider<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>
    for ExecutionContextImpl<D>
where
    D: DatasetDb
        + MetaDataProvider<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>
        + LayerProviderDb,
{
    async fn meta_data(
        &self,
        data_id: &DataId,
    ) -> Result<
        Box<dyn MetaData<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>>,
        geoengine_operators::error::Error,
    > {
        match data_id {
            DataId::Internal { dataset_id: _ } => {
                self.db.meta_data(&data_id.clone()).await.map_err(|e| {
                    geoengine_operators::error::Error::LoadingInfo {
                        source: Box::new(e),
                    }
                })
            }
            DataId::External(external) => {
                self.db
                    .load_layer_provider(external.provider_id.into())
                    .await
                    .map_err(|e| geoengine_operators::error::Error::DatasetMetaData {
                        source: Box::new(e),
                    })?
                    .meta_data(data_id)
                    .await
            }
        }
    }
}

// TODO: use macro(?) for delegating meta_data function to DatasetDB to avoid redundant code
#[async_trait]
impl<D> MetaDataProvider<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>
    for ExecutionContextImpl<D>
where
    D: DatasetDb
        + MetaDataProvider<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>
        + LayerProviderDb,
{
    async fn meta_data(
        &self,
        data_id: &DataId,
    ) -> Result<
        Box<dyn MetaData<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>>,
        geoengine_operators::error::Error,
    > {
        match data_id {
            DataId::Internal { dataset_id: _ } => {
                self.db.meta_data(&data_id.clone()).await.map_err(|e| {
                    geoengine_operators::error::Error::LoadingInfo {
                        source: Box::new(e),
                    }
                })
            }
            DataId::External(external) => {
                self.db
                    .load_layer_provider(external.provider_id.into())
                    .await
                    .map_err(|e| geoengine_operators::error::Error::DatasetMetaData {
                        source: Box::new(e),
                    })?
                    .meta_data(data_id)
                    .await
            }
        }
    }
}

#[cfg(test)]

mod tests {
    use super::*;
    use std::str::FromStr;

    use geoengine_datatypes::{test_data, util::test::TestDefault};
    use serial_test::serial;

    use crate::contexts::{InMemoryContext, SessionContext};

    #[cfg(feature = "xgboost")]
    use crate::machine_learning::ml_model::MlModel;

    /// Loads a pretrained mock model from disk
    async fn load_mock_model_from_disk() -> String {
        let path = test_data!("pro/ml/")
            .join("b764bf81-e21d-4eb8-bf01-fac9af13faee")
            .join("mock_model.json");

        tokio::fs::read_to_string(path).await.unwrap()
    }

    #[tokio::test]
    #[serial]
    /// Verify, that a stored model can be read from the `InMemoryDb` backend.
    async fn load_ml_model_from_db_test() {
        let model_content = load_mock_model_from_disk().await;

        let model = MlModel {
            model_id: MlModelId::from_str("b764bf81-e21d-4eb8-bf01-fac9af13faee")
                .expect("Could not create a new ModelId."),
            model_content,
        };

        let ctx = InMemoryContext::test_default()
            .default_session_context()
            .await
            .unwrap();

        ctx.db()
            .store_ml_model(model)
            .await
            .expect("Could not store the model in the db.");

        let exe_ctx = ctx.execution_context().unwrap();

        let model_id = MlModelId::from_str("b764bf81-e21d-4eb8-bf01-fac9af13faee")
            .expect("Should have generated a ModelId from the given uuid string.");

        let mut model = exe_ctx
            .load_ml_model(model_id)
            .await
            .expect("Could not load ml model from backend db.");

        let actual: String = model.drain(0..350).collect();

        let expected = "{\"learner\":{\"attributes\":{},\"feature_names\":[],\"feature_types\":[],\"gradient_booster\":{\"model\":{\"gbtree_model_param\":{\"num_parallel_tree\":\"1\",\"num_trees\":\"48\",\"size_leaf_vector\":\"0\"},\"tree_info\":[0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2,0,1,2],\"trees\":[{\"base_weights\":[0.2127139,-0.5411393,1.0690608],";

        assert_eq!(actual, expected);
    }

    #[tokio::test]
    #[serial]
    async fn store_model_in_db_test() {
        // get a mock model, to store in the database
        let expected = load_mock_model_from_disk().await;

        let model = MlModel {
            model_id: MlModelId::from_str("b764bf81-e21d-4eb8-bf01-fac9af13faee")
                .expect("Could not create a new ModelId."),
            model_content: expected.clone(),
        };

        let ctx = InMemoryContext::test_default()
            .default_session_context()
            .await
            .unwrap();

        let mut exe_ctx = ctx.execution_context().unwrap();

        exe_ctx
            .store_ml_model_in_db(model.model_id, model.model_content)
            .await
            .expect("Could not store ml model in backend db.");

        let actual = exe_ctx
            .load_ml_model(model.model_id)
            .await
            .expect("Could not load ml model from backend db.");

        assert_eq!(actual, expected);
    }
}

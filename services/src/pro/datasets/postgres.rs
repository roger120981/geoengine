use std::collections::HashMap;
use std::str::FromStr;

use crate::api::model::datatypes::{DatasetId, LayerId};
use crate::api::model::services::AddDataset;
use crate::datasets::listing::ProvenanceOutput;
use crate::datasets::storage::DATASET_DB_LAYER_PROVIDER_ID;
use crate::datasets::storage::DATASET_DB_ROOT_COLLECTION_ID;
use crate::datasets::storage::{
    Dataset, DatasetDb, DatasetStore, DatasetStorer, MetaDataDefinition,
};
use crate::datasets::upload::FileId;
use crate::datasets::upload::{Upload, UploadDb, UploadId};
use crate::error::{self, Error, Result};
use crate::layers::layer::CollectionItem;
use crate::layers::layer::Layer;
use crate::layers::layer::LayerCollection;
use crate::layers::layer::LayerCollectionListOptions;
use crate::layers::layer::LayerListing;
use crate::layers::layer::ProviderLayerCollectionId;
use crate::layers::layer::ProviderLayerId;
use crate::layers::listing::LayerCollectionProvider;
use crate::layers::listing::{DatasetLayerCollectionProvider, LayerCollectionId};
use crate::layers::storage::INTERNAL_PROVIDER_ID;
use crate::pro::contexts::PostgresDb;
use crate::pro::permissions::{Permission, PermissionDb, RoleId};
use crate::projects::Symbology;
use crate::util::operators::source_operator_from_dataset;
use crate::util::user_input::Validated;
use crate::workflows::workflow::Workflow;
use crate::{
    datasets::listing::{DatasetListOptions, DatasetListing, DatasetProvider},
    pro::users::UserSession,
};
use async_trait::async_trait;
use bb8_postgres::bb8::Pool;
use bb8_postgres::tokio_postgres::tls::{MakeTlsConnect, TlsConnect};
use bb8_postgres::tokio_postgres::Socket;
use bb8_postgres::PostgresConnectionManager;
use geoengine_datatypes::dataset::DataId;
use geoengine_datatypes::primitives::RasterQueryRectangle;
use geoengine_datatypes::primitives::VectorQueryRectangle;
use geoengine_datatypes::util::Identifier;
use geoengine_operators::engine::{
    MetaData, MetaDataProvider, RasterResultDescriptor, StaticMetaData, TypedResultDescriptor,
    VectorResultDescriptor,
};

use geoengine_operators::mock::MockDatasetDataSourceLoadingInfo;

use geoengine_operators::source::{GdalLoadingInfo, OgrSourceDataset};
use log::info;
use postgres_types::{FromSql, ToSql};
use snafu::{ensure, ResultExt};
use uuid::Uuid;

impl<Tls> DatasetDb for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
}

#[async_trait]
impl<Tls> DatasetProvider for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn list_datasets(
        &self,
        _options: Validated<DatasetListOptions>,
    ) -> Result<Vec<DatasetListing>> {
        // TODO: use options

        let conn = self.conn_pool.get().await?;
        let stmt = conn
            .prepare(
                "
            SELECT 
                d.id, 
                d.name, 
                d.description,
                d.tags,
                d.source_operator,
                d.result_descriptor,
                d.symbology
            FROM 
                user_permitted_datasets p JOIN datasets d 
                    ON (p.dataset_id = d.id)
            WHERE 
                p.user_id = $1",
            )
            .await?;

        let rows = conn.query(&stmt, &[&self.session.user.id]).await?;

        Ok(rows
            .iter()
            .map(|row| {
                Result::<DatasetListing>::Ok(DatasetListing {
                    id: row.get(0),
                    name: row.get(1),
                    description: row.get(2),
                    tags: row.get::<_, Option<_>>(3).unwrap_or_default(),
                    source_operator: row.get(4),
                    result_descriptor: serde_json::from_value(row.get(5))?,
                    symbology: serde_json::from_value(row.get(6))?,
                })
            })
            .filter_map(Result::ok)
            .collect())
    }

    async fn load_dataset(&self, dataset: &DatasetId) -> Result<Dataset> {
        let conn = self.conn_pool.get().await?;
        let stmt = conn
            .prepare(
                "
            SELECT
                d.id,
                d.name,
                d.description,
                d.result_descriptor,
                d.source_operator,
                d.symbology,
                d.provenance
            FROM 
                user_permitted_datasets p JOIN datasets d 
                    ON (p.dataset_id = d.id)
            WHERE 
                p.user_id = $1 AND d.id = $2
            LIMIT 
                1",
            )
            .await?;

        // TODO: throw proper dataset does not exist/no permission error
        let row = conn
            .query_one(&stmt, &[&self.session.user.id, dataset])
            .await?;

        Ok(Dataset {
            id: row.get(0),
            name: row.get(1),
            description: row.get(2),
            result_descriptor: serde_json::from_value(row.get(3))?,
            source_operator: row.get(4),
            symbology: serde_json::from_value(row.get(5))?,
            provenance: serde_json::from_value(row.get(6))?,
        })
    }

    async fn load_provenance(&self, dataset: &DatasetId) -> Result<ProvenanceOutput> {
        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare(
                "
            SELECT 
                d.provenance 
            FROM 
                user_permitted_datasets p JOIN datasets d
                    ON(p.dataset_id = d.id)
            WHERE 
                p.user_id = $1 AND d.id = $2",
            )
            .await?;

        let row = conn
            .query_one(&stmt, &[&self.session.user.id, dataset])
            .await?;

        Ok(ProvenanceOutput {
            data: (*dataset).into(),
            provenance: serde_json::from_value(row.get(0)).context(error::SerdeJson)?,
        })
    }
}

#[async_trait]
impl<Tls>
    MetaDataProvider<MockDatasetDataSourceLoadingInfo, VectorResultDescriptor, VectorQueryRectangle>
    for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn meta_data(
        &self,
        _id: &DataId,
    ) -> geoengine_operators::util::Result<
        Box<
            dyn MetaData<
                MockDatasetDataSourceLoadingInfo,
                VectorResultDescriptor,
                VectorQueryRectangle,
            >,
        >,
    > {
        Err(geoengine_operators::error::Error::NotYetImplemented)
    }
}

#[async_trait]
impl<Tls> MetaDataProvider<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>
    for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn meta_data(
        &self,
        id: &DataId,
    ) -> geoengine_operators::util::Result<
        Box<dyn MetaData<OgrSourceDataset, VectorResultDescriptor, VectorQueryRectangle>>,
    > {
        let id: DatasetId = id
            .internal()
            .ok_or(geoengine_operators::error::Error::DataIdTypeMissMatch)?
            .into();

        let conn = self.conn_pool.get().await.unwrap(); // TODO
        let stmt = conn
            .prepare(
                "
        SELECT 
            d.meta_data
        FROM 
            user_permitted_datasets p JOIN datasets d 
                ON (p.dataset_id = d.id)
        WHERE 
            d.id = $1 AND p.user_id = $2",
            )
            .await
            .unwrap(); // TODO

        let row = conn
            .query_one(&stmt, &[&id, &self.session.user.id])
            .await
            .unwrap(); // TODO

        let meta_data: StaticMetaData<
            OgrSourceDataset,
            VectorResultDescriptor,
            VectorQueryRectangle,
        > = serde_json::from_value(row.get(0))?;

        Ok(Box::new(meta_data))
    }
}

#[async_trait]
impl<Tls> MetaDataProvider<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>
    for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn meta_data(
        &self,
        id: &DataId,
    ) -> geoengine_operators::util::Result<
        Box<dyn MetaData<GdalLoadingInfo, RasterResultDescriptor, RasterQueryRectangle>>,
    > {
        let id: DatasetId = id
            .internal()
            .ok_or(geoengine_operators::error::Error::DataIdTypeMissMatch)?
            .into();

        let conn = self.conn_pool.get().await.unwrap(); // TODO
        let stmt = conn
            .prepare(
                "
        SELECT 
            d.meta_data
        FROM 
            user_permitted_datasets p JOIN datasets d 
                ON (p.dataset_id = d.id)
        WHERE 
            d.id = $1 AND p.user_id = $2",
            )
            .await
            .unwrap(); // TODO

        let row = conn
            .query_one(&stmt, &[&id, &self.session.user.id])
            .await
            .unwrap(); // TODO

        let meta_data: MetaDataDefinition = serde_json::from_value(row.get(0))?;

        Ok(match meta_data {
            MetaDataDefinition::GdalMetaDataRegular(m) => Box::new(m),
            MetaDataDefinition::GdalStatic(m) => Box::new(m),
            _ => return Err(geoengine_operators::error::Error::DataIdTypeMissMatch),
        })
    }
}

#[async_trait]
pub trait PostgresStorable<Tls>: Send + Sync
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    fn to_json(&self) -> Result<DatasetMetaDataJson>;
}

pub struct DatasetMetaDataJson {
    meta_data: serde_json::Value,
    result_descriptor: serde_json::Value,
}

impl<Tls> DatasetStorer for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    type StorageType = Box<dyn PostgresStorable<Tls>>;
}

impl<Tls> PostgresStorable<Tls> for MetaDataDefinition
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    fn to_json(&self) -> Result<DatasetMetaDataJson> {
        match self {
            MetaDataDefinition::MockMetaData(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
            MetaDataDefinition::OgrMetaData(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
            MetaDataDefinition::GdalMetaDataRegular(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
            MetaDataDefinition::GdalStatic(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
            MetaDataDefinition::GdalMetadataNetCdfCf(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
            MetaDataDefinition::GdalMetaDataList(d) => Ok(DatasetMetaDataJson {
                meta_data: serde_json::to_value(self)?,
                result_descriptor: serde_json::to_value(TypedResultDescriptor::from(
                    d.result_descriptor.clone(),
                ))?,
            }),
        }
    }
}

#[async_trait]
impl<Tls> DatasetStore for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn add_dataset(
        &self,
        dataset: Validated<AddDataset>,
        meta_data: Box<dyn PostgresStorable<Tls>>,
    ) -> Result<DatasetId> {
        let dataset = dataset.user_input;
        let id = dataset.id.unwrap_or_else(DatasetId::new);

        let meta_data_json = meta_data.to_json()?;

        let mut conn = self.conn_pool.get().await?;

        let tx = conn.build_transaction().start().await?;

        let stmt = tx
            .prepare(
                "
                INSERT INTO datasets (
                    id,
                    name,
                    description,
                    source_operator,
                    result_descriptor,
                    meta_data,
                    symbology,
                    provenance
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .await?;

        tx.execute(
            &stmt,
            &[
                &id,
                &dataset.name,
                &dataset.description,
                &dataset.source_operator,
                &meta_data_json.result_descriptor,
                &meta_data_json.meta_data,
                &serde_json::to_value(&dataset.symbology)?,
                &serde_json::to_value(&dataset.provenance)?,
            ],
        )
        .await?;

        let stmt = tx
            .prepare(
                "
            INSERT INTO permissions (
                role_id,
                dataset_id,
                permission
            )
            VALUES ($1, $2, $3)",
            )
            .await?;

        tx.execute(
            &stmt,
            &[&RoleId::from(self.session.user.id), &id, &Permission::Owner],
        )
        .await?;

        tx.commit().await?;

        Ok(id)
    }

    async fn delete_dataset(&self, dataset_id: DatasetId) -> Result<()> {
        ensure!(
            self.has_permission(dataset_id, Permission::Owner).await?,
            error::PermissionDenied
        );

        let mut conn = self.conn_pool.get().await?;

        let tx = conn.build_transaction().start().await?;

        let stmt = tx
            .prepare(
                "
        SELECT 
            TRUE
        FROM 
            user_permitted_datasets p JOIN datasets d 
                ON (p.dataset_id = d.id)
        WHERE 
            d.id = $1 AND p.user_id = $2 AND p.permission = 'Owner';",
            )
            .await?;

        let rows = tx
            .query(&stmt, &[&dataset_id, &self.session.user.id])
            .await?;

        if rows.is_empty() {
            return Err(Error::OperationRequiresOwnerPermission);
        }

        let stmt = tx.prepare("DELETE FROM datasets WHERE id = $1;").await?;

        tx.execute(&stmt, &[&dataset_id]).await?;

        tx.commit().await?;

        Ok(())
    }

    fn wrap_meta_data(&self, meta: MetaDataDefinition) -> Self::StorageType {
        Box::new(meta)
    }
}

#[async_trait]
impl<Tls> UploadDb for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn load_upload(&self, upload: UploadId) -> Result<Upload> {
        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare("SELECT id, files FROM uploads WHERE id = $1 AND user_id = $2")
            .await?;

        let row = conn
            .query_one(&stmt, &[&upload, &self.session.user.id])
            .await?;

        Ok(Upload {
            id: row.get(0),
            files: row
                .get::<_, Vec<FileUpload>>(1)
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    async fn create_upload(&self, upload: Upload) -> Result<()> {
        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare("INSERT INTO uploads (id, user_id, files) VALUES ($1, $2, $3)")
            .await?;

        conn.execute(
            &stmt,
            &[
                &upload.id,
                &self.session.user.id,
                &upload
                    .files
                    .iter()
                    .map(FileUpload::from)
                    .collect::<Vec<_>>(),
            ],
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl<Tls> DatasetLayerCollectionProvider for PostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn load_dataset_layer_collection(
        &self,
        collection: &LayerCollectionId,
        options: Validated<LayerCollectionListOptions>,
    ) -> Result<LayerCollection> {
        ensure!(
            self.has_permission(collection.clone(), Permission::Owner)
                .await?,
            error::PermissionDenied
        );

        let conn = self.conn_pool.get().await?;

        let options = options.user_input;

        // TODO: only list datasets that are accessible to the user as layer
        // for now they are listed, but cannot be accessed
        let stmt = conn
            .prepare(
                "
                SELECT 
                    concat(d.id, ''), 
                    d.name, 
                    d.description
                FROM 
                    datasets d
                ORDER BY d.name ASC
                LIMIT $1
                OFFSET $2;",
            )
            .await?;

        let rows = conn
            .query(
                &stmt,
                &[&i64::from(options.limit), &i64::from(options.offset)],
            )
            .await?;

        let items = rows
            .iter()
            .map(|row| {
                Result::<CollectionItem>::Ok(CollectionItem::Layer(LayerListing {
                    id: ProviderLayerId {
                        provider_id: DATASET_DB_LAYER_PROVIDER_ID,
                        layer_id: LayerId(row.get(0)),
                    },
                    name: row.get(1),
                    description: row.get(2),
                    properties: vec![],
                }))
            })
            .filter_map(Result::ok)
            .collect();

        Ok(LayerCollection {
            id: ProviderLayerCollectionId {
                provider_id: INTERNAL_PROVIDER_ID,
                collection_id: collection.clone(),
            },
            name: "Datasets".to_string(),
            description: "Basic Layers for all Datasets".to_string(),
            items,
            entry_label: None,
            properties: vec![],
        })
    }

    async fn get_dataset_root_layer_collection_id(&self) -> Result<LayerCollectionId> {
        Ok(LayerCollectionId(DATASET_DB_ROOT_COLLECTION_ID.to_string()))
    }

    async fn load_dataset_layer(&self, id: &LayerId) -> Result<Layer> {
        let dataset_id = DatasetId::from_str(&id.0)?;

        ensure!(
            self.has_permission(dataset_id, Permission::Owner).await?,
            error::PermissionDenied
        );

        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare(
                "
                SELECT 
                    d.name, 
                    d.description,
                    d.source_operator,
                    d.symbology
                FROM 
                    datasets d
                WHERE id = $1;",
            )
            .await?;

        let row = conn
            .query_one(
                &stmt,
                &[
                    &Uuid::from_str(&id.0).map_err(|_| error::Error::IdStringMustBeUuid {
                        found: id.0.clone(),
                    })?,
                ],
            )
            .await?;

        let name: String = row.get(0);
        let description: String = row.get(1);
        let source_operator: String = row.get(2);
        let symbology: Option<Symbology> = serde_json::from_value(row.get(3))?;

        let operator = source_operator_from_dataset(&source_operator, &dataset_id.into())?;

        Ok(Layer {
            id: ProviderLayerId {
                provider_id: DATASET_DB_LAYER_PROVIDER_ID,
                layer_id: id.clone(),
            },
            name,
            description,
            workflow: Workflow { operator },
            symbology,
            properties: vec![],
            metadata: HashMap::new(),
        })
    }
}

#[derive(Debug, Clone, ToSql, FromSql)]
pub struct FileUpload {
    pub id: FileId,
    pub name: String,
    pub byte_size: i64,
}

impl From<crate::datasets::upload::FileUpload> for FileUpload {
    fn from(upload: crate::datasets::upload::FileUpload) -> Self {
        Self {
            id: upload.id,
            name: upload.name,
            byte_size: upload.byte_size as i64,
        }
    }
}

impl From<&crate::datasets::upload::FileUpload> for FileUpload {
    fn from(upload: &crate::datasets::upload::FileUpload) -> Self {
        Self {
            id: upload.id,
            name: upload.name.clone(),
            byte_size: upload.byte_size as i64,
        }
    }
}

impl From<FileUpload> for crate::datasets::upload::FileUpload {
    fn from(upload: FileUpload) -> Self {
        Self {
            id: upload.id,
            name: upload.name,
            byte_size: upload.byte_size as u64,
        }
    }
}

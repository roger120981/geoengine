use crate::error;
use crate::layers::external::TypedDataProviderDefinition;
use crate::layers::layer::Property;
use crate::layers::listing::{
    ProviderCapabilities, SearchCapabilities, SearchParameters, SearchType, SearchTypes,
};
use crate::layers::postgres_layer_db::{
    delete_layer_collection, delete_layer_collection_from_parent, delete_layer_from_collection,
    insert_collection_parent, insert_layer, insert_layer_collection_with_id,
};
use crate::pro::contexts::ProPostgresDb;
use crate::pro::datasets::TypedProDataProviderDefinition;
use crate::pro::permissions::postgres_permissiondb::TxPermissionDb;
use crate::pro::permissions::{Permission, RoleId};
use crate::{
    error::Result,
    layers::{
        external::{DataProvider, DataProviderDefinition},
        layer::{
            AddLayer, AddLayerCollection, CollectionItem, Layer, LayerCollection,
            LayerCollectionListOptions, LayerCollectionListing, LayerListing,
            ProviderLayerCollectionId, ProviderLayerId,
        },
        listing::{LayerCollectionId, LayerCollectionProvider},
        storage::{
            LayerDb, LayerProviderDb, LayerProviderListing, LayerProviderListingOptions,
            INTERNAL_LAYER_DB_ROOT_COLLECTION_ID, INTERNAL_PROVIDER_ID,
        },
        LayerDbError,
    },
};
use async_trait::async_trait;
use bb8_postgres::tokio_postgres::{
    tls::{MakeTlsConnect, TlsConnect},
    Socket,
};
use geoengine_datatypes::dataset::{DataProviderId, LayerId};
use geoengine_datatypes::util::HashMapTextTextDbType;
use snafu::{ensure, ResultExt};
use std::str::FromStr;
use uuid::Uuid;

#[async_trait]
impl<Tls> LayerDb for ProPostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn add_layer(&self, layer: AddLayer, collection: &LayerCollectionId) -> Result<LayerId> {
        let layer_id = Uuid::new_v4();
        let layer_id = LayerId(layer_id.to_string());

        self.add_layer_with_id(&layer_id, layer, collection).await?;

        Ok(layer_id)
    }

    async fn add_layer_with_id(
        &self,
        id: &LayerId,
        layer: AddLayer,
        collection: &LayerCollectionId,
    ) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let trans = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection.clone().into(), Permission::Owner, &trans)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let layer_id = insert_layer(&trans, id, layer, collection).await?;

        // TODO: `ON CONFLICT DO NOTHING` means, we do not get an error if the permission already exists.
        //       Do we want that, or should we report an error and let the caller decide whether to ignore it?
        //       We should decide that and adjust all places where `ON CONFLICT DO NOTHING` is used.
        let stmt = trans
            .prepare(
                "
            INSERT INTO permissions (role_id, permission, layer_id)
            VALUES ($1, $2, $3) ON CONFLICT DO NOTHING;",
            )
            .await?;

        trans
            .execute(
                &stmt,
                &[
                    &RoleId::from(self.session.user.id),
                    &Permission::Owner,
                    &layer_id,
                ],
            )
            .await?;

        trans.commit().await?;

        Ok(())
    }

    async fn add_layer_to_collection(
        &self,
        layer: &LayerId,
        collection: &LayerCollectionId,
    ) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let tx = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection.clone().into(), Permission::Owner, &tx)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let layer_id =
            Uuid::from_str(&layer.0).map_err(|_| crate::error::Error::IdStringMustBeUuid {
                found: layer.0.clone(),
            })?;

        let collection_id =
            Uuid::from_str(&collection.0).map_err(|_| crate::error::Error::IdStringMustBeUuid {
                found: collection.0.clone(),
            })?;

        let stmt = tx
            .prepare(
                "
            INSERT INTO collection_layers (collection, layer)
            VALUES ($1, $2) ON CONFLICT DO NOTHING;",
            )
            .await?;

        tx.execute(&stmt, &[&collection_id, &layer_id]).await?;

        tx.commit().await?;

        Ok(())
    }

    async fn add_layer_collection(
        &self,
        collection: AddLayerCollection,
        parent: &LayerCollectionId,
    ) -> Result<LayerCollectionId> {
        let collection_id = Uuid::new_v4();
        let collection_id = LayerCollectionId(collection_id.to_string());

        self.add_layer_collection_with_id(&collection_id, collection, parent)
            .await?;

        Ok(collection_id)
    }

    async fn add_layer_collection_with_id(
        &self,
        id: &LayerCollectionId,
        collection: AddLayerCollection,
        parent: &LayerCollectionId,
    ) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let trans = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(parent.clone().into(), Permission::Owner, &trans)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let collection_id = insert_layer_collection_with_id(&trans, id, collection, parent).await?;

        let stmt = trans
            .prepare(
                "
            INSERT INTO permissions (role_id, permission, layer_collection_id)
            VALUES ($1, $2, $3) ON CONFLICT DO NOTHING;",
            )
            .await?;

        trans
            .execute(
                &stmt,
                &[
                    &RoleId::from(self.session.user.id),
                    &Permission::Owner,
                    &collection_id,
                ],
            )
            .await?;

        trans.commit().await?;

        Ok(())
    }

    async fn add_collection_to_parent(
        &self,
        collection: &LayerCollectionId,
        parent: &LayerCollectionId,
    ) -> Result<()> {
        let conn = self.conn_pool.get().await?;
        insert_collection_parent(&conn, collection, parent).await
    }

    async fn remove_layer_collection(&self, collection: &LayerCollectionId) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let transaction = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection.clone().into(), Permission::Owner, &transaction)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        delete_layer_collection(&transaction, collection).await?;

        transaction.commit().await.map_err(Into::into)
    }

    async fn remove_layer_from_collection(
        &self,
        layer: &LayerId,
        collection: &LayerCollectionId,
    ) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let transaction = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection.clone().into(), Permission::Owner, &transaction)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        delete_layer_from_collection(&transaction, layer, collection).await?;

        transaction.commit().await.map_err(Into::into)
    }

    async fn remove_layer_collection_from_parent(
        &self,
        collection: &LayerCollectionId,
        parent: &LayerCollectionId,
    ) -> Result<()> {
        let mut conn = self.conn_pool.get().await?;
        let transaction = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection.clone().into(), Permission::Owner, &transaction)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        delete_layer_collection_from_parent(&transaction, collection, parent).await?;

        transaction.commit().await.map_err(Into::into)
    }
}

fn create_search_query(full_info: bool) -> String {
    format!("
        WITH RECURSIVE parents AS (
            SELECT $1::uuid as id
            UNION ALL SELECT DISTINCT child FROM collection_children JOIN parents ON (id = parent)
        )
        SELECT DISTINCT *
        FROM (
            SELECT 
                {}
            FROM user_permitted_layer_collections u
                JOIN layer_collections lc ON (u.layer_collection_id = lc.id)
                JOIN (SELECT DISTINCT child FROM collection_children JOIN parents ON (id = parent)) cc ON (id = cc.child)
            WHERE u.user_id = $4 AND name ILIKE $5
        ) u UNION (
            SELECT 
                {}
            FROM user_permitted_layers ul
                JOIN layers uc ON (ul.layer_id = uc.id)
                JOIN (SELECT DISTINCT layer FROM collection_layers JOIN parents ON (collection = id)) cl ON (id = cl.layer)
            WHERE ul.user_id = $4 AND name ILIKE $5
        )
        ORDER BY {}name ASC
        LIMIT $2 
        OFFSET $3;",
        if full_info {
            "concat(id, '') AS id,
        name,
        description,
        properties,
        FALSE AS is_layer"
        } else { "name" },
        if full_info {
            "concat(id, '') AS id,
        name,
        description,
        properties,
        TRUE AS is_layer"
        } else { "name" },
        if full_info { "is_layer ASC," } else { "" })
}

#[async_trait]
impl<Tls> LayerCollectionProvider for ProPostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            listing: true,
            search: SearchCapabilities {
                search_types: SearchTypes {
                    fulltext: true,
                    prefix: true,
                },
                autocomplete: true,
                filters: None,
            },
        }
    }

    fn name(&self) -> &str {
        "Postgres Layer Collection Provider (Pro)"
    }

    fn description(&self) -> &str {
        "Layer collection provider for Postgres (Pro)"
    }

    #[allow(clippy::too_many_lines)]
    async fn load_layer_collection(
        &self,
        collection_id: &LayerCollectionId,
        options: LayerCollectionListOptions,
    ) -> Result<LayerCollection> {
        let mut conn = self.conn_pool.get().await?;
        let tx = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection_id.clone().into(), Permission::Read, &tx)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let collection = Uuid::from_str(&collection_id.0).map_err(|_| {
            crate::error::Error::IdStringMustBeUuid {
                found: collection_id.0.clone(),
            }
        })?;

        let stmt = tx
            .prepare(
                "
        SELECT name, description, properties
        FROM user_permitted_layer_collections p 
            JOIN layer_collections c ON (p.layer_collection_id = c.id) 
        WHERE p.user_id = $1 AND layer_collection_id = $2;",
            )
            .await?;

        let row = tx
            .query_one(&stmt, &[&self.session.user.id, &collection])
            .await?;

        let name: String = row.get(0);
        let description: String = row.get(1);
        let properties: Vec<Property> = row.get(2);

        let stmt = tx
            .prepare(
                "
        SELECT DISTINCT id, name, description, properties, is_layer
        FROM (
            SELECT 
                concat(id, '') AS id, 
                name, 
                description, 
                properties, 
                FALSE AS is_layer
            FROM user_permitted_layer_collections u 
                JOIN layer_collections lc ON (u.layer_collection_id = lc.id)
                JOIN collection_children cc ON (layer_collection_id = cc.child)
            WHERE u.user_id = $4 AND cc.parent = $1
        ) u UNION (
            SELECT 
                concat(id, '') AS id, 
                name, 
                description, 
                properties, 
                TRUE AS is_layer
            FROM user_permitted_layers ul
                JOIN layers uc ON (ul.layer_id = uc.id) 
                JOIN collection_layers cl ON (layer_id = cl.layer)
            WHERE ul.user_id = $4 AND cl.collection = $1
        )
        ORDER BY is_layer ASC, name ASC
        LIMIT $2 
        OFFSET $3;            
        ",
            )
            .await?;

        let rows = tx
            .query(
                &stmt,
                &[
                    &collection,
                    &i64::from(options.limit),
                    &i64::from(options.offset),
                    &self.session.user.id,
                ],
            )
            .await?;

        let items = rows
            .into_iter()
            .map(|row| {
                let is_layer: bool = row.get(4);

                if is_layer {
                    Ok(CollectionItem::Layer(LayerListing {
                        id: ProviderLayerId {
                            provider_id: INTERNAL_PROVIDER_ID,
                            layer_id: LayerId(row.get(0)),
                        },
                        name: row.get(1),
                        description: row.get(2),
                        properties: row.get(3),
                    }))
                } else {
                    Ok(CollectionItem::Collection(LayerCollectionListing {
                        id: ProviderLayerCollectionId {
                            provider_id: INTERNAL_PROVIDER_ID,
                            collection_id: LayerCollectionId(row.get(0)),
                        },
                        name: row.get(1),
                        description: row.get(2),
                        properties: row.get(3),
                    }))
                }
            })
            .collect::<Result<Vec<CollectionItem>>>()?;

        tx.commit().await?;

        Ok(LayerCollection {
            id: ProviderLayerCollectionId {
                provider_id: INTERNAL_PROVIDER_ID,
                collection_id: collection_id.clone(),
            },
            name,
            description,
            items,
            entry_label: None,
            properties,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn search(
        &self,
        collection_id: &LayerCollectionId,
        search: SearchParameters,
    ) -> Result<LayerCollection> {
        let mut conn = self.conn_pool.get().await?;
        let tx = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection_id.clone().into(), Permission::Read, &tx)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let collection = Uuid::from_str(&collection_id.0).map_err(|_| {
            crate::error::Error::IdStringMustBeUuid {
                found: collection_id.0.clone(),
            }
        })?;

        let stmt = tx
            .prepare(
                "
        SELECT name, description, properties
        FROM user_permitted_layer_collections p 
            JOIN layer_collections c ON (p.layer_collection_id = c.id) 
        WHERE p.user_id = $1 AND layer_collection_id = $2;",
            )
            .await?;

        let row = tx
            .query_one(&stmt, &[&self.session.user.id, &collection])
            .await?;

        let name: String = row.get(0);
        let description: String = row.get(1);
        let properties: Vec<Property> = row.get(2);

        let pattern = match search.search_type {
            SearchType::Fulltext => {
                format!("%{}%", search.search_string)
            }
            SearchType::Prefix => {
                format!("{}%", search.search_string)
            }
        };

        let stmt = tx.prepare(&create_search_query(true)).await?;

        let rows = tx
            .query(
                &stmt,
                &[
                    &collection,
                    &i64::from(search.limit),
                    &i64::from(search.offset),
                    &self.session.user.id,
                    &pattern,
                ],
            )
            .await?;

        let items = rows
            .into_iter()
            .map(|row| {
                let is_layer: bool = row.get(4);

                if is_layer {
                    Ok(CollectionItem::Layer(LayerListing {
                        id: ProviderLayerId {
                            provider_id: INTERNAL_PROVIDER_ID,
                            layer_id: LayerId(row.get(0)),
                        },
                        name: row.get(1),
                        description: row.get(2),
                        properties: row.get(3),
                    }))
                } else {
                    Ok(CollectionItem::Collection(LayerCollectionListing {
                        id: ProviderLayerCollectionId {
                            provider_id: INTERNAL_PROVIDER_ID,
                            collection_id: LayerCollectionId(row.get(0)),
                        },
                        name: row.get(1),
                        description: row.get(2),
                        properties: row.get(3),
                    }))
                }
            })
            .collect::<Result<Vec<CollectionItem>>>()?;

        tx.commit().await?;

        Ok(LayerCollection {
            id: ProviderLayerCollectionId {
                provider_id: INTERNAL_PROVIDER_ID,
                collection_id: collection_id.clone(),
            },
            name,
            description,
            items,
            entry_label: None,
            properties,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn autocomplete_search(
        &self,
        collection_id: &LayerCollectionId,
        search: SearchParameters,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn_pool.get().await?;
        let tx = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(collection_id.clone().into(), Permission::Read, &tx)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let collection = Uuid::from_str(&collection_id.0).map_err(|_| {
            crate::error::Error::IdStringMustBeUuid {
                found: collection_id.0.clone(),
            }
        })?;

        let pattern = match search.search_type {
            SearchType::Fulltext => {
                format!("%{}%", search.search_string)
            }
            SearchType::Prefix => {
                format!("{}%", search.search_string)
            }
        };

        let stmt = tx.prepare(&create_search_query(false)).await?;

        let rows = tx
            .query(
                &stmt,
                &[
                    &collection,
                    &i64::from(search.limit),
                    &i64::from(search.offset),
                    &self.session.user.id,
                    &pattern,
                ],
            )
            .await?;

        let items = rows
            .into_iter()
            .map(|row| Ok(row.get::<usize, &str>(0).to_string()))
            .collect::<Result<Vec<String>>>()?;

        tx.commit().await?;

        Ok(items)
    }

    async fn get_root_layer_collection_id(&self) -> Result<LayerCollectionId> {
        Ok(LayerCollectionId(
            INTERNAL_LAYER_DB_ROOT_COLLECTION_ID.to_string(),
        ))
    }

    async fn load_layer(&self, id: &LayerId) -> Result<Layer> {
        let mut conn = self.conn_pool.get().await?;
        let tx = conn.build_transaction().start().await?;

        self.ensure_permission_in_tx(id.clone().into(), Permission::Read, &tx)
            .await
            .map_err(Into::into)
            .context(crate::error::PermissionDb)?;

        let layer_id =
            Uuid::from_str(&id.0).map_err(|_| crate::error::Error::IdStringMustBeUuid {
                found: id.0.clone(),
            })?;

        let stmt = tx
            .prepare(
                "
            SELECT 
                l.name,
                l.description,
                w.workflow,
                l.symbology,
                l.properties,
                l.metadata
            FROM 
                layers l JOIN workflows w ON (l.workflow_id = w.id)
            WHERE 
                l.id = $1;",
            )
            .await?;

        let row = tx
            .query_one(&stmt, &[&layer_id])
            .await
            .map_err(|_error| LayerDbError::NoLayerForGivenId { id: id.clone() })?;

        tx.commit().await?;

        Ok(Layer {
            id: ProviderLayerId {
                provider_id: INTERNAL_PROVIDER_ID,
                layer_id: id.clone(),
            },
            name: row.get(0),
            description: row.get(1),
            workflow: serde_json::from_value(row.get(2)).context(crate::error::SerdeJson)?,
            symbology: row.get(3),
            properties: row.get(4),
            metadata: row.get::<_, HashMapTextTextDbType>(5).into(),
        })
    }
}

#[async_trait]
impl<Tls> LayerProviderDb for ProPostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn add_layer_provider(
        &self,
        provider: TypedDataProviderDefinition,
    ) -> Result<DataProviderId> {
        ensure!(self.session.is_admin(), error::PermissionDenied);

        let conn = self.conn_pool.get().await?;

        let prio = DataProviderDefinition::<Self>::priority(&provider);
        let clamp_prio = prio.clamp(-1000, 1000);

        if prio != clamp_prio {
            log::warn!(
                "The priority of the provider {} is out of range! --> clamped {} to {}",
                DataProviderDefinition::<Self>::name(&provider),
                prio,
                clamp_prio
            );
        }

        let stmt = conn
            .prepare(
                "
              INSERT INTO layer_providers (
                  id, 
                  type_name, 
                  name,
                  definition,
                  priority
              )
              VALUES ($1, $2, $3, $4, $5)",
            )
            .await?;

        let id = DataProviderDefinition::<Self>::id(&provider);
        conn.execute(
            &stmt,
            &[
                &id,
                &DataProviderDefinition::<Self>::type_name(&provider),
                &DataProviderDefinition::<Self>::name(&provider),
                &provider,
                &clamp_prio,
            ],
        )
        .await?;
        Ok(id)
    }

    async fn list_layer_providers(
        &self,
        options: LayerProviderListingOptions,
    ) -> Result<Vec<LayerProviderListing>> {
        // TODO: permission
        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare(
                "(
                    SELECT 
                        id, 
                        name,
                        type_name,
                        priority
                    FROM 
                        layer_providers
                    WHERE
                        priority > -1000
                    UNION ALL
                    SELECT 
                        id, 
                        name,
                        type_name,
                        priority
                    FROM 
                        pro_layer_providers
                    WHERE
                        priority > -1000
                )
                ORDER BY priority desc, name ASC
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

        Ok(rows
            .iter()
            .map(|row| LayerProviderListing {
                id: row.get(0),
                name: row.get(1),
                priority: row.get(3),
            })
            .collect())
    }

    async fn load_layer_provider(&self, id: DataProviderId) -> Result<Box<dyn DataProvider>> {
        // TODO: permissions
        let conn = self.conn_pool.get().await?;

        let stmt = conn
            .prepare(
                "SELECT
                    definition, NULL AS pro_definition
                FROM
                    layer_providers
                WHERE
                    id = $1
                UNION ALL
                SELECT
                    NULL AS definition, definition AS pro_definition
                FROM
                    pro_layer_providers
                WHERE
                    id = $1",
            )
            .await?;

        let row = conn.query_one(&stmt, &[&id]).await?;

        if let Some(definition) = row.get::<_, Option<TypedDataProviderDefinition>>(0) {
            return Box::new(definition)
                .initialize(ProPostgresDb {
                    conn_pool: self.conn_pool.clone(),
                    session: self.session.clone(),
                })
                .await;
        }

        let pro_definition: TypedProDataProviderDefinition = row.get(1);
        Box::new(pro_definition)
            .initialize(ProPostgresDb {
                conn_pool: self.conn_pool.clone(),
                session: self.session.clone(),
            })
            .await
    }
}

#[async_trait]
pub trait ProLayerProviderDb: Send + Sync + 'static {
    async fn add_pro_layer_provider(
        &self,
        provider: TypedProDataProviderDefinition,
    ) -> Result<DataProviderId>;
}

#[async_trait]
impl<Tls> ProLayerProviderDb for ProPostgresDb<Tls>
where
    Tls: MakeTlsConnect<Socket> + Clone + Send + Sync + 'static,
    <Tls as MakeTlsConnect<Socket>>::Stream: Send + Sync,
    <Tls as MakeTlsConnect<Socket>>::TlsConnect: Send,
    <<Tls as MakeTlsConnect<Socket>>::TlsConnect as TlsConnect<Socket>>::Future: Send,
{
    async fn add_pro_layer_provider(
        &self,
        provider: TypedProDataProviderDefinition,
    ) -> Result<DataProviderId> {
        ensure!(self.session.is_admin(), error::PermissionDenied);

        let conn = self.conn_pool.get().await?;

        let prio = DataProviderDefinition::<Self>::priority(&provider);
        let clamp_prio = prio.clamp(-1000, 1000);

        if prio != clamp_prio {
            log::warn!(
                "The priority of the provider {} is out of range! --> clamped {} to {}",
                DataProviderDefinition::<Self>::name(&provider),
                prio,
                clamp_prio
            );
        }

        let stmt = conn
            .prepare(
                "
              INSERT INTO pro_layer_providers (
                  id, 
                  type_name, 
                  name,
                  definition,
                  priority
              )
              VALUES ($1, $2, $3, $4, $5)",
            )
            .await?;

        let id = DataProviderDefinition::<Self>::id(&provider);
        conn.execute(
            &stmt,
            &[
                &id,
                &DataProviderDefinition::<Self>::type_name(&provider),
                &DataProviderDefinition::<Self>::name(&provider),
                &provider,
                &clamp_prio,
            ],
        )
        .await?;
        Ok(id)
    }
}

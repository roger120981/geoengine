use self::migration_0000_initial::Migration0000Initial;
pub use crate::contexts::migrations::{
    migration_0001_raster_stacks::Migration0001RasterStacks,
    migration_0002_dataset_listing_provider::Migration0002DatasetListingProvider,
    migration_0003_gbif_config::Migration0003GbifConfig,
    migration_0004_dataset_listing_provider_prio::Migration0004DatasetListingProviderPrio,
    migration_0005_gbif_column_selection::Migration0005GbifColumnSelection,
    migration_0006_ebv_provider::Migration0006EbvProvider,
    migration_0007_owner_role::Migration0007OwnerRole,
};
pub use database_migration::{migrate_database, DatabaseVersion, Migration, MigrationResult};

mod database_migration;
pub mod migration_0000_initial;
pub mod migration_0001_raster_stacks;
pub mod migration_0002_dataset_listing_provider;
pub mod migration_0003_gbif_config;
pub mod migration_0004_dataset_listing_provider_prio;
pub mod migration_0005_gbif_column_selection;
mod migration_0006_ebv_provider;
pub mod migration_0007_owner_role;
#[cfg(test)]
mod schema_info;

/// All migrations that are available. The migrations are applied in the order they are defined here, starting from the current version of the database.
///
/// NEW MIGRATIONS HAVE TO BE REGISTERED HERE!
///
pub fn all_migrations() -> Vec<Box<dyn Migration>> {
    vec![
        Box::new(Migration0000Initial),
        Box::new(Migration0001RasterStacks),
        Box::new(Migration0002DatasetListingProvider),
        Box::new(Migration0003GbifConfig),
        Box::new(Migration0004DatasetListingProviderPrio),
        Box::new(Migration0005GbifColumnSelection),
        Box::new(Migration0006EbvProvider),
        Box::new(Migration0007OwnerRole),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::config::get_config_element;
    use bb8_postgres::{
        bb8::{Pool, PooledConnection},
        PostgresConnectionManager,
    };
    use tests::schema_info::{schema_info_from_information_schema, SchemaInfo};
    use tokio_postgres::NoTls;

    #[tokio::test]
    async fn migrations_lead_to_ground_truth_schema() {
        async fn get_schema(
            connection: &mut PooledConnection<'_, PostgresConnectionManager<NoTls>>,
        ) -> SchemaInfo {
            let transaction = connection
                .build_transaction()
                .read_only(true)
                .start()
                .await
                .unwrap();
            let schema = transaction
                .query_one("SELECT current_schema", &[])
                .await
                .unwrap()
                .get::<_, String>("current_schema");

            schema_info_from_information_schema(transaction, &schema)
                .await
                .unwrap()
        }

        let schema_after_migrations = {
            let postgres_config = get_config_element::<crate::util::config::Postgres>().unwrap();
            let pg_mgr = PostgresConnectionManager::new(postgres_config.try_into().unwrap(), NoTls);

            let pool = Pool::builder().max_size(1).build(pg_mgr).await.unwrap();

            let mut connection = pool.get().await.unwrap();

            // initial schema
            migrate_database(&mut connection, &all_migrations(), None)
                .await
                .unwrap();

            get_schema(&mut connection).await
        };

        let ground_truth_schema = {
            let postgres_config = get_config_element::<crate::util::config::Postgres>().unwrap();
            let pg_mgr = PostgresConnectionManager::new(postgres_config.try_into().unwrap(), NoTls);

            let pool = Pool::builder().max_size(1).build(pg_mgr).await.unwrap();

            let mut connection = pool.get().await.unwrap();

            connection
                .batch_execute(include_str!("current_schema.sql"))
                .await
                .unwrap();

            get_schema(&mut connection).await
        };

        pretty_assertions::assert_eq!(schema_after_migrations, ground_truth_schema);
    }
}

use self::migration_0000_initial::Migration0000Initial;
use crate::contexts::migrations::{
    migration_0001_raster_stacks::Migration0001RasterStacks,
    migration_0002_dataset_listing_provider::Migration0002DatasetListingProvider,
    migration_0003_gbif_config::Migration0003GbifConfig,
    migration_0004_dataset_listing_provider_prio::Migration0004DatasetListingProviderPrio,
};
pub use database_migration::{migrate_database, DatabaseVersion, Migration, MigrationResult};

mod database_migration;
pub mod migration_0000_initial;
pub mod migration_0001_raster_stacks;
pub mod migration_0002_dataset_listing_provider;
pub mod migration_0003_gbif_config;
pub mod migration_0004_dataset_listing_provider_prio;

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
    ]
}
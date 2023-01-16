use crate::api::model::datatypes::{
    BoundingBox2D, Breakpoint, ClassificationMeasurement, Colorizer, ContinuousMeasurement,
    Coordinate2D, DataId, DataProviderId, DatasetId, DateTimeParseFormat, ExternalDataId,
    FeatureDataType, LayerId, Measurement, MultiLineString, MultiPoint, MultiPolygon, NoGeometry,
    Palette, PlotOutputFormat, RasterDataType, RasterPropertiesEntryType, RasterPropertiesKey,
    RasterQueryRectangle, RgbaColor, SpatialPartition2D, SpatialReference,
    SpatialReferenceAuthority, SpatialReferenceOption, SpatialResolution, TimeGranularity,
    TimeInstance, TimeInterval, TimeStep, VectorDataType,
};
use crate::api::model::operators::{
    CsvHeader, FileNotFoundHandling, FormatSpecifics, GdalConfigOption, GdalDatasetGeoTransform,
    GdalDatasetParameters, GdalLoadingInfoTemporalSlice, GdalMetaDataList, GdalMetaDataRegular,
    GdalMetaDataStatic, GdalMetadataMapping, GdalMetadataNetCdfCf, GdalSourceTimePlaceholder,
    MockDatasetDataSourceLoadingInfo, MockMetaData, OgrMetaData, OgrSourceColumnSpec,
    OgrSourceDataset, OgrSourceDatasetTimeType, OgrSourceDurationSpec, OgrSourceErrorSpec,
    OgrSourceTimeFormat, PlotResultDescriptor, RasterResultDescriptor, TimeReference,
    TypedGeometry, TypedOperator, TypedResultDescriptor, UnixTimeStampType, VectorColumnInfo,
    VectorResultDescriptor,
};
use crate::api::model::services::{
    AddDataset, CreateDataset, DataPath, DatasetDefinition, MetaDataDefinition, MetaDataSuggestion,
};
use crate::contexts::{SessionId, SimpleSession};
use crate::datasets::listing::{DatasetListing, OrderBy, Provenance, ProvenanceOutput};
use crate::datasets::storage::{AutoCreateDataset, Dataset};
use crate::datasets::upload::{UploadId, Volume, VolumeName};
use crate::handlers;
use crate::handlers::plots::WrappedPlotOutput;
use crate::handlers::tasks::TaskAbortOptions;
use crate::handlers::wcs::CoverageResponse;
use crate::handlers::wfs::{CollectionType, Coordinates, Feature, FeatureType, GeoJson};
use crate::handlers::wms::MapResponse;
use crate::handlers::workflows::{RasterDatasetFromWorkflow, RasterDatasetFromWorkflowResult};
use crate::layers::layer::{
    AddLayer, AddLayerCollection, CollectionItem, Layer, LayerCollection, LayerCollectionListing,
    LayerListing, Property, ProviderLayerCollectionId, ProviderLayerId,
};
use crate::layers::listing::LayerCollectionId;
use crate::ogc::util::OgcBoundingBox;
use crate::ogc::{wcs, wfs, wms};
use crate::projects::{
    ColorParam, DerivedColor, DerivedNumber, LineSymbology, NumberParam, PointSymbology,
    PolygonSymbology, ProjectId, RasterSymbology, STRectangle, StrokeParam, Symbology,
    TextSymbology,
};
use crate::tasks::{TaskFilter, TaskId, TaskListOptions, TaskStatus};
use crate::util::{apidoc::OpenApiServerInfo, server::ServerInfo, IdResponse};
use crate::workflows::workflow::{Workflow, WorkflowId};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

#[derive(OpenApi)]
#[openapi(
    paths(
        crate::util::server::available_handler,
        crate::util::server::server_info_handler,
        handlers::layers::layer_handler,
        handlers::layers::layer_to_workflow_id_handler,
        handlers::layers::list_collection_handler,
        handlers::layers::list_root_collections_handler,
        handlers::layers::add_layer,
        handlers::layers::add_collection,
        handlers::layers::remove_collection,
        handlers::layers::remove_layer_from_collection,
        handlers::layers::add_existing_layer_to_collection,
        handlers::layers::add_existing_collection_to_collection,
        handlers::layers::remove_collection_from_collection,
        handlers::session::anonymous_handler,
        handlers::session::session_handler,
        handlers::session::session_project_handler,
        handlers::session::session_view_handler,
        handlers::tasks::abort_handler,
        handlers::tasks::list_handler,
        handlers::tasks::status_handler,
        handlers::wcs::wcs_capabilities_handler,
        handlers::wcs::wcs_describe_coverage_handler,
        handlers::wcs::wcs_get_coverage_handler,
        handlers::wfs::wfs_capabilities_handler,
        handlers::wfs::wfs_feature_handler,
        handlers::wms::wms_capabilities_handler,
        handlers::wms::wms_legend_graphic_handler,
        handlers::wms::wms_map_handler,
        handlers::workflows::dataset_from_workflow_handler,
        handlers::workflows::get_workflow_metadata_handler,
        handlers::workflows::get_workflow_provenance_handler,
        handlers::workflows::load_workflow_handler,
        handlers::workflows::register_workflow_handler,
        handlers::datasets::list_datasets_handler,
        handlers::datasets::list_volumes_handler,
        handlers::datasets::get_dataset_handler,
        handlers::datasets::create_dataset_handler,
        handlers::datasets::auto_create_dataset_handler,
        handlers::datasets::suggest_meta_data_handler,
        handlers::plots::get_plot_handler,
    ),
    components(
        schemas(
            SimpleSession,

            DataId,
            DataProviderId,
            DatasetId,
            ExternalDataId,
            IdResponse<WorkflowId>,
            LayerId,
            ProjectId,
            SessionId,
            TaskId,
            UploadId,
            WorkflowId,
            ProviderLayerId,
            ProviderLayerCollectionId,
            LayerCollectionId,

            TimeInstance,
            TimeInterval,

            Coordinate2D,
            BoundingBox2D,
            SpatialPartition2D,
            SpatialResolution,
            SpatialReference,
            SpatialReferenceOption,
            SpatialReferenceAuthority,
            Measurement,
            ContinuousMeasurement,
            ClassificationMeasurement,
            STRectangle,

            ProvenanceOutput,
            Provenance,

            VectorDataType,
            FeatureDataType,
            RasterDataType,

            ServerInfo,

            Workflow,
            TypedOperator,
            TypedResultDescriptor,
            PlotResultDescriptor,
            RasterResultDescriptor,
            VectorResultDescriptor,
            VectorColumnInfo,
            RasterDatasetFromWorkflow,
            RasterDatasetFromWorkflowResult,
            RasterQueryRectangle,
            // VectorQueryRectangle,
            // PlotQueryRectangle,

            TaskAbortOptions,
            TaskFilter,
            TaskListOptions,
            TaskStatus,

            Layer,
            LayerListing,
            LayerCollection,
            LayerCollectionListing,
            Property,
            CollectionItem,
            AddLayer,
            AddLayerCollection,

            Breakpoint,
            ColorParam,
            Colorizer,
            DerivedColor,
            DerivedNumber,
            LineSymbology,
            NumberParam,
            Palette,
            PointSymbology,
            PolygonSymbology,
            RasterSymbology,
            RgbaColor,
            StrokeParam,
            Symbology,
            TextSymbology,

            OgcBoundingBox,
            MapResponse,
            CoverageResponse,

            wcs::request::WcsService,
            wcs::request::WcsVersion,
            wcs::request::GetCapabilitiesRequest,
            wcs::request::DescribeCoverageRequest,
            wcs::request::GetCoverageRequest,
            wcs::request::GetCoverageFormat,
            wcs::request::WcsBoundingbox,

            wms::request::WmsService,
            wms::request::WmsVersion,
            wms::request::GetCapabilitiesFormat,
            wms::request::GetCapabilitiesRequest,
            wms::request::GetMapRequest,
            wms::request::GetMapExceptionFormat,
            wms::request::GetMapFormat,
            wms::request::GetLegendGraphicRequest,

            wfs::request::WfsService,
            wfs::request::WfsVersion,
            wfs::request::GetCapabilitiesRequest,
            wfs::request::WfsResolution,
            wfs::request::GetFeatureRequest,
            wfs::request::TypeNames,

            GeoJson,
            CollectionType,
            Feature,
            FeatureType,
            Coordinates,

            CreateDataset,
            AutoCreateDataset,
            OrderBy,
            DatasetListing,
            MetaDataSuggestion,
            MetaDataDefinition,
            MockMetaData,
            GdalMetaDataRegular,
            GdalMetaDataStatic,
            GdalMetadataNetCdfCf,
            GdalMetaDataList,
            GdalDatasetParameters,
            TimeStep,
            GdalSourceTimePlaceholder,
            GdalDatasetParameters,
            GdalLoadingInfoTemporalSlice,
            FileNotFoundHandling,
            GdalDatasetGeoTransform,
            GdalMetadataMapping,
            TimeGranularity,
            DateTimeParseFormat,
            TimeReference,
            RasterPropertiesKey,
            RasterPropertiesEntryType,
            OgrMetaData,
            GdalConfigOption,
            MockDatasetDataSourceLoadingInfo,
            OgrSourceDataset,
            OgrSourceColumnSpec,
            TypedGeometry,
            OgrSourceErrorSpec,
            OgrSourceDatasetTimeType,
            OgrSourceDurationSpec,
            OgrSourceTimeFormat,
            NoGeometry,
            MultiPoint,
            MultiLineString,
            MultiPolygon,
            FormatSpecifics,
            CsvHeader,
            UnixTimeStampType,
            Dataset,
            DatasetDefinition,
            Volume,
            VolumeName,
            DataPath,

            PlotOutputFormat,
            WrappedPlotOutput
        ),
    ),
    modifiers(&SecurityAddon, &ApiDocInfo, &OpenApiServerInfo),
    external_docs(url = "https://docs.geoengine.io", description = "Geo Engine Docs")
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.as_mut().unwrap();
        components.add_security_scheme(
            "session_token",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("UUID")
                    .description(Some("A valid session token can be obtained via the /anonymous or /login (pro only) endpoints. Alternatively, it can be defined as a fixed value in the Settings.toml file."))
                    .build(),
            ),
        );
    }
}

struct ApiDocInfo;

impl Modify for ApiDocInfo {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.info.title = "Geo Engine API".to_string();

        openapi.info.contact = Some(
            utoipa::openapi::ContactBuilder::new()
                .name(Some("Geo Engine Developers"))
                .email(Some("dev@geoengine.de"))
                .build(),
        );

        openapi.info.license = Some(
            utoipa::openapi::LicenseBuilder::new()
                .name("Apache 2.0 (pro features excluded)")
                .url(Some(
                    "https://github.com/geo-engine/geoengine/blob/master/LICENSE",
                ))
                .build(),
        );
    }
}

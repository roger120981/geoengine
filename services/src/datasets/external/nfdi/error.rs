use snafu::prelude::*;
use tonic::metadata::errors::InvalidMetadataValue;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
#[snafu(context(suffix(false)))] // disables default `Snafu` suffix
pub enum NFDIProviderError {
    InvalidAPIToken { source: InvalidMetadataValue },
    InvalidDataId,
    InvalidUri { uri_string: String },
    MissingCollection,
    MissingDataObject,
    MissingMetaObject,
    MissingNFDIMetaData,
    MissingObjectGroup,
    MissingURL,
    Reqwest { source: reqwest::Error },
    UnexpectedObjectHierarchy,
    TonicStatus { source: tonic::Status },
    TonicTransport { source: tonic::transport::Error },
}

impl From<tonic::Status> for NFDIProviderError {
    fn from(source: tonic::Status) -> Self {
        Self::TonicStatus { source }
    }
}

impl From<tonic::transport::Error> for NFDIProviderError {
    fn from(source: tonic::transport::Error) -> Self {
        Self::TonicTransport { source }
    }
}

impl From<reqwest::Error> for NFDIProviderError {
    fn from(source: reqwest::Error) -> Self {
        Self::Reqwest { source }
    }
}

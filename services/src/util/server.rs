use crate::error::Result;
use crate::handlers::ErrorResponse;
use crate::util::config::get_config_element;

use actix_http::body::{BoxBody, EitherBody, MessageBody};
use actix_http::uri::PathAndQuery;
use actix_http::HttpMessage;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::error::{InternalError, JsonPayloadError, QueryPayloadError};
use actix_web::{http, middleware, web, HttpResponse};
use log::debug;
use std::num::NonZeroUsize;
use tracing::log::info;
use tracing::Span;
use tracing_actix_web::{RequestId, RootSpanBuilder};
use url::Url;
use utoipa::{openapi::OpenApi, ToSchema};

/// Custom root span for web requests that paste a request id to all logs.
pub struct CustomRootSpanBuilder;

impl RootSpanBuilder for CustomRootSpanBuilder {
    fn on_request_start(request: &ServiceRequest) -> Span {
        let request_id = request.extensions().get::<RequestId>().copied().unwrap();

        let span = tracing::info_span!("Request", request_id = %request_id);

        // Emit HTTP request at the beginng of the span.
        {
            let _entered = span.enter();

            let head = request.head();
            let http_method = head.method.as_str();

            let http_route: std::borrow::Cow<'static, str> = request
                .match_pattern()
                .map_or_else(|| "default".into(), Into::into);

            let http_target = request
                .uri()
                .path_and_query()
                .map_or("", PathAndQuery::as_str);

            tracing::info!(
                target: "HTTP request",
                method = %http_method,
                route = %http_route,
                target = %http_target,
            );
        }

        span
    }

    fn on_request_end<B>(_span: Span, _outcome: &Result<ServiceResponse<B>, actix_web::Error>) {}
}

/// Calculate maximum number of blocking threads **per worker**.
///
/// By default set to 512 / workers.
///
/// TODO: use blocking threads globally instead of per worker.
///
pub(crate) fn calculate_max_blocking_threads_per_worker() -> usize {
    const MIN_BLOCKING_THREADS_PER_WORKER: usize = 32;

    // Taken from `actix_server::ServerBuilder`.
    // By default, server uses number of available logical CPU as thread count.
    let number_of_workers = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1);

    // Taken from `actix_server::ServerWorkerConfig`.
    let max_blocking_threads = std::cmp::max(512 / number_of_workers, 1);

    std::cmp::max(max_blocking_threads, MIN_BLOCKING_THREADS_PER_WORKER)
}

pub(crate) fn configure_extractors(cfg: &mut web::ServiceConfig) {
    cfg.app_data(web::JsonConfig::default().error_handler(|err, _req| {
        match err {
            JsonPayloadError::ContentType => InternalError::from_response(
                err,
                HttpResponse::UnsupportedMediaType().json(ErrorResponse {
                    error: "UnsupportedMediaType".to_string(),
                    message: "Unsupported content type header.".to_string(),
                }),
            )
            .into(),
            JsonPayloadError::Overflow { limit } => InternalError::from_response(
                err,
                HttpResponse::PayloadTooLarge().json(ErrorResponse {
                    error: "Overflow".to_string(),
                    message: format!("JSON payload has exceeded limit ({} bytes).", limit),
                }),
            )
            .into(),
            JsonPayloadError::OverflowKnownLength { length, limit } => {
                InternalError::from_response(
                    err,
                    HttpResponse::PayloadTooLarge().json(ErrorResponse {
                        error: "Overflow".to_string(),
                        message: format!(
                            "JSON payload ({} bytes) is larger than allowed (limit: {} bytes).",
                            length, limit
                        ),
                    }),
                )
                .into()
            }
            JsonPayloadError::Payload(err) => ErrorResponse {
                error: "Payload".to_string(),
                message: err.to_string(),
            }
            .into(),
            JsonPayloadError::Deserialize(err) => ErrorResponse {
                error: "BodyDeserializeError".to_string(),
                message: err.to_string(),
            }
            .into(),
            JsonPayloadError::Serialize(err) => ErrorResponse {
                error: "BodySerializeError".to_string(),
                message: err.to_string(),
            }
            .into(),
            _ => {
                debug!("Unknown JsonPayloadError variant");
                ErrorResponse {
                    error: "UnknownError".to_string(),
                    message: "Unknown Error".to_string(),
                }
                .into()
            }
        }
    }));
    cfg.app_data(web::QueryConfig::default().error_handler(|err, _req| {
        match err {
            QueryPayloadError::Deserialize(err) => ErrorResponse {
                error: "UnableToParseQueryString".to_string(),
                message: format!("Unable to parse query string: {}", err),
            }
            .into(),
            _ => {
                debug!("Unknown QueryPayloadError variant");
                ErrorResponse {
                    error: "UnknownError".to_string(),
                    message: "Unknown Error".to_string(),
                }
                .into()
            }
        }
    }));
}

#[derive(serde::Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServerInfo {
    pub(crate) build_date: &'static str,
    pub(crate) commit_hash: &'static str,
    pub(crate) version: &'static str,
    pub(crate) features: &'static str,
}

/// Shows information about the server software version.
#[utoipa::path(
    tag = "General",
    get,
    path = "/info",
    responses(
        (status = 200, description = "Server software information", body = ServerInfo,
            example = json!({
                "buildDate": "2022-09-29",
                "commitHash": "555dc6d84d3682c37490a145d53c5097d0b81b27",
                "version": "0.7.0",
                "features": "default"
              }))
    )
)]
#[allow(clippy::unused_async)] // the function signature of request handlers requires it
pub(crate) async fn server_info_handler() -> impl actix_web::Responder {
    web::Json(server_info())
}

pub(crate) fn server_info() -> ServerInfo {
    ServerInfo {
        build_date: env!("VERGEN_BUILD_DATE"),
        commit_hash: env!("VERGEN_GIT_SHA"),
        version: env!("CARGO_PKG_VERSION"),
        features: env!("VERGEN_CARGO_FEATURES"),
    }
}

#[allow(clippy::unnecessary_wraps)]
pub(crate) fn render_404(
    mut response: ServiceResponse,
) -> actix_web::Result<middleware::ErrorHandlerResponse<BoxBody>> {
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::header::HeaderValue::from_static("application/json"),
    );

    let response_json_string = serde_json::to_string(&ErrorResponse {
        error: "NotFound".to_string(),
        message: "Not Found".to_string(),
    })
    .expect("Serialization of fixed ErrorResponse must not fail");

    let response = response.map_body(|_, _| EitherBody::new(response_json_string.boxed()));

    Ok(middleware::ErrorHandlerResponse::Response(response))
}

#[allow(clippy::unnecessary_wraps)]
pub(crate) fn render_405(
    mut response: ServiceResponse,
) -> actix_web::Result<middleware::ErrorHandlerResponse<BoxBody>> {
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        http::header::HeaderValue::from_static("application/json"),
    );

    let response_json_string = serde_json::to_string(&ErrorResponse {
        error: "MethodNotAllowed".to_string(),
        message: "HTTP method not allowed.".to_string(),
    })
    .expect("Serialization of fixed ErrorResponse must not fail");

    let response = response.map_body(|_, _| EitherBody::new(response_json_string.boxed()));

    Ok(middleware::ErrorHandlerResponse::Response(response))
}

// this is a workaround to be able to serve swagger UI and the openapi.json behind a proxy (/api)
// TODO: remove this when utoipa allows configuring the paths to serve the openapi.json and to include it in the swagger UI separately
pub fn serve_openapi_json<
    T: ServiceFactory<ServiceRequest, Config = (), Error = actix_web::Error, InitError = ()>,
>(
    app: actix_web::App<T>,
    api_urls: &mut Vec<(utoipa_swagger_ui::Url, OpenApi)>,
    name: &'static str,
    ui_url: &'static str,
    serve_url: &str,
    openapi: OpenApi,
) -> actix_web::App<T> {
    api_urls.push((utoipa_swagger_ui::Url::new(name, ui_url), openapi.clone()));
    app.route(
        serve_url,
        web::get().to(move || {
            let openapi = openapi.clone();
            async move { web::Json(openapi) }
        }),
    )
}

pub(crate) fn log_server_info() -> Result<()> {
    let web_config: crate::util::config::Web = get_config_element()?;

    let external_address = web_config.external_address()?;

    info!("Starting server…");

    let version = server_info();
    info!(
        "Version: {} (commit: {}, build date: {})",
        version.version, version.commit_hash, version.build_date
    );
    info!("Features: {}", version.features);

    info!(
        "Local Address: {} ",
        Url::parse(&format!("http://{}/", web_config.bind_address))?,
    );

    info!("External Address: {} ", external_address);

    info!(
        "API Documentation: {}",
        external_address.join("swagger-ui/")?
    );

    let session_config: crate::util::config::Session = get_config_element()?;

    if session_config.anonymous_access {
        info!("Anonymous Access: enabled");
    } else {
        info!("Anonymous Access: disabled");
    }

    Ok(())
}

#[allow(clippy::unused_async)]
// async is required for the request handler signature
pub async fn not_implemented_handler() -> HttpResponse {
    HttpResponse::NotImplemented().finish()
}
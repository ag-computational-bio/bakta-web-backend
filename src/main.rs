use axum::{
    Router,
    response::Redirect,
    routing::{delete, get, post},
};
use bakta_handler::BaktaHandler;
use std::{net::SocketAddr, sync::Arc};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

mod api_paths;
#[allow(clippy::upper_case_acronyms)]
mod api_structs;
mod argo;
mod bakta_handler;
mod s3_handler;
mod v2;
mod workflow_catalog;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::from_filename(".env")?;

    let socket_address: SocketAddr = dotenvy::var("SOCKET_ADDR")
        .unwrap_or("127.0.0.1:8080".to_string())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(socket_address).await.unwrap();
    let swagger = SwaggerUi::new("/swagger-ui")
        .url(
            "/api-docs/openapi-v1.json",
            api_structs::BaktaApi::openapi(),
        )
        .url(
            "/api-docs/openapi-v2.json",
            v2::api_structs::BaktaApiV2::openapi(),
        );

    let bakta_handler = Arc::new(
        BaktaHandler::new(
            dotenvy::var("ARGO_TOKEN")?,
            dotenvy::var("ARGO_URL")?,
            dotenvy::var("ARGO_NAMESPACE")?,
            dotenvy::var("S3_ACCESS_KEY")?,
            dotenvy::var("S3_SECRET_KEY")?,
            dotenvy::var("S3_BUCKET")?,
            dotenvy::var("S3_ENDPOINT")?,
            dotenvy::var("BAKTA_VERSION")?,
            dotenvy::var("DATABASE_VERSION")?,
            dotenvy::var("BAKTFOLD_VERSION").unwrap_or_default(),
            dotenvy::var("BAKTFOLD_DATABASE_VERSION").unwrap_or_default(),
            dotenvy::var("BACKEND_VERSION")?,
        )
        .await,
    );

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or("none".into())
        .add_directive("tower_http=debug".parse()?)
        .add_directive("bakta_web_backend=trace".parse()?);

    tracing_subscriber::fmt().with_env_filter(filter).init();

    tracing::info!(?socket_address, "Starting bakta web backend");

    let app = Router::new()
        .merge(swagger)
        .route("/", get(|| async { Redirect::permanent("/swagger-ui") }))
        .route("/api/v1/job/delete", delete(api_paths::delete_job))
        .route("/api/v1/job/logs", get(api_paths::job_logs))
        .route("/api/v1/job/init", post(api_paths::init_job))
        .route("/api/v1/job/list", post(api_paths::list_jobs))
        .route("/api/v1/job/result", post(api_paths::query_result))
        .route("/api/v1/job/start", post(api_paths::start_job))
        .route("/api/v1/version", get(api_paths::version))
        .route("/api/v2/workflows", get(v2::api_paths::workflows))
        .route("/api/v2/job/delete", delete(v2::api_paths::delete_job))
        .route("/api/v2/job/logs", get(v2::api_paths::job_logs))
        .route("/api/v2/job/init", post(v2::api_paths::init_job))
        .route("/api/v2/job/list", post(v2::api_paths::list_jobs))
        .route("/api/v2/job/result", post(v2::api_paths::query_result))
        .route("/api/v2/job/start", post(v2::api_paths::start_job))
        .route("/api/v2/version", get(v2::api_paths::version))
        .with_state(bakta_handler)
        .layer(CorsLayer::very_permissive())
        .layer(
            TraceLayer::new_for_http()
                .on_response(())
                .on_body_chunk(())
                .on_eos(()),
        );
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

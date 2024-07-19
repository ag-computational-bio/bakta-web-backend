use axum::{
    response::Redirect,
    routing::{delete, get, post},
    Router,
};
use bakta_handler::BaktaHandler;
use std::{net::SocketAddr, sync::Arc};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

mod api_paths;
mod api_structs;
mod argo;
mod bakta_handler;
mod s3_handler;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::from_filename(".env")?;

    let socket_address: SocketAddr = dotenvy::var("SOCKET_ADDR")
        .unwrap_or("127.0.0.1:8080".to_string())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(socket_address).await.unwrap();
    let swagger = SwaggerUi::new("/swagger-ui")
        .url("/api-docs/openapi.json", api_structs::BaktaApi::openapi());

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
            dotenvy::var("BACKEND_VERSION")?,
        )
        .await,
    );


    let filter = EnvFilter::try_from_default_env()
        .unwrap_or("none".into())
        .add_directive("tower_http=debug".parse()?)
        .add_directive("bakta_web_backend=trace".parse()?);

    tracing_subscriber::fmt()
        .with_env_filter(
            filter
        )
        .init();

    tracing::info!(?socket_address, "Starting bakta web backend");

    let app = Router::new()
        .merge(swagger)
        .route("/", get(|| async { Redirect::permanent("/swagger-ui") }))
        .route("/api/v1/delete", delete(api_paths::delete_job))
        .route("/api/v1/job/init", post(api_paths::init_job))
        .route("/api/v1/job/list", post(api_paths::list_jobs))
        .route("/api/v1/job/result", post(api_paths::query_result))
        .route("/api/v1/job/start", post(api_paths::start_job))
        .route("/api/v1/version", get(api_paths::version))
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

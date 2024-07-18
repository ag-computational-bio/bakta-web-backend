use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use reqwest::StatusCode;

use crate::{
    api_structs::{InitRequest, InitResponse, Job, ListRequest, StartRequest},
    bakta_handler::BaktaHandler,
};

/// Delete an existing Job
#[utoipa::path(
    delete,
    path = "/api/v1/delete",
    params(
        Job,
    ),
    responses(
        (status = 200, body = ()),
        (status = 400, body = String)
    )
)]
pub async fn delete_job(
    State(state): State<Arc<BaktaHandler>>,
    Query(job): Query<Job>,
) -> impl IntoResponse {
    state
        .state_handler
        .delete_job((job.id, job.secret))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// Create a new BaktaJob
#[utoipa::path(
    post,
    path = "/api/v1/job/init",
    request_body = InitRequest,
    responses(
        (status = 200, body = InitResponse),
        (status = 400, body = String)
    )
)]
pub async fn init_job(
    State(state): State<Arc<BaktaHandler>>,
    Json(init_request): Json<InitRequest>,
) -> impl IntoResponse {
    let (id, secret) = state.state_handler.init_job(init_request.name).await;

    let Ok(fasta_url) = state
        .s3_handler
        .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Fasta)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json("Failed to sign URL".to_string()),
        )
            .into_response();
    };

    let Ok(prodigal_url) = state
        .s3_handler
        .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Prodigal)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json("Failed to sign URL".to_string()),
        )
            .into_response();
    };
    let Ok(replicon_url) = state
        .s3_handler
        .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::RepliconsTSV)
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json("Failed to sign URL".to_string()),
        )
            .into_response();
    };

    (
        StatusCode::OK,
        Json(InitResponse {
            job: Job { id, secret },
            fasta_url,
            prodigal_url,
            replicon_url,
        }),
    )
        .into_response()
}

/// List status of jobs
#[utoipa::path(
    post,
    path = "/api/v1/job/list",
    request_body = ListRequest,
    responses(
        (status = 200, body = ListResponse)
    )
)]
pub async fn list_jobs(Json(ListRequest): Json<ListRequest>) -> impl IntoResponse {
    format!("")
}

/// Query the result of a job
#[utoipa::path(
    post,
    path = "/api/v1/job/result",
    request_body = Job,
    responses(
        (status = 200, body = ResultResponse)
    )
)]
pub async fn query_result(Json(job): Json<Job>) -> impl IntoResponse {
    format!("")
}

/// Start a job
#[utoipa::path(
    post,
    path = "/api/v1/job/start",
    request_body = StartRequest,
    responses(
        (status = 200, body = ())
    )
)]
pub async fn start_job(Json(StartRequest): Json<StartRequest>) -> impl IntoResponse {
    format!("")
}

/// Get the current version
#[utoipa::path(
    get,
    path = "/api/v1/version",
    responses(
        (status = 200, body = VersionResponse)
    )
)]
pub async fn version() -> impl IntoResponse {
    format!("")
}

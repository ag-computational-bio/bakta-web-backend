use axum::{extract::Query, response::IntoResponse, Json};

use crate::api_structs::{InitRequest, Job, ListRequest, StartRequest};

/// Delete an existing Job
#[utoipa::path(
    delete,
    path = "/api/v1/delete",
    params(
        Job,
    ),
    responses(
        (status = 200, body = ())
    )
)]
pub async fn delete_job(query: Query<Job>) -> impl IntoResponse {
    format!("Job {} deleted", query.id)
}

/// Create a new BaktaJob
#[utoipa::path(
    post,
    path = "/api/v1/job/init",
    request_body = InitRequest,
    responses(
        (status = 200, body = InitResponse)
    )
)]
pub async fn init_job(Json(InitRequest): Json<InitRequest>) -> impl IntoResponse {
    format!("")
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

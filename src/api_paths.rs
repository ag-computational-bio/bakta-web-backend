use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
    Json,
};
use reqwest::StatusCode;

use crate::{
    api_structs::{
        InitRequest, InitResponse, Job, ListRequest, ListResponse, ResultResponse, StartRequest,
        VersionResponse,
    },
    bakta_handler::{BaktaHandler, REGEX},
};

/// Delete an existing Job
#[utoipa::path(
    delete,
    path = "/api/v1/job/delete",
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

/// Get stdout / stderr logs of a job
#[utoipa::path(
    post,
    path = "/api/v1/job/logs",
    params(
        Job,
    ),
    responses(
        (status = 200, body = ()),
        (status = 400, body = String)
    )
)]
pub async fn job_logs(
    State(state): State<Arc<BaktaHandler>>,
    Query(job): Query<Job>,
) -> impl IntoResponse {
    state
        .state_handler
        .get_logs((job.id, job.secret))
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

    let Ok((fasta_url, prodigal_url, replicon_url)) =
        || -> anyhow::Result<(String, String, String)> {
            Ok((
                state
                    .s3_handler
                    .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Fasta)?,
                state
                    .s3_handler
                    .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Prodigal)?,
                state
                    .s3_handler
                    .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::RepliconsTSV)?,
            ))
        }()
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
pub async fn list_jobs(
    State(state): State<Arc<BaktaHandler>>,
    Json(list_request): Json<ListRequest>,
) -> impl IntoResponse {
    Json(state.state_handler.get_job_states(list_request.jobs).await)
}

/// Query the result of a job
#[utoipa::path(
    post,
    path = "/api/v1/job/result",
    request_body = Job,
    responses(
        (status = 200, body = ResultResponse),
        (status = 400, body = String)
    )
)]
pub async fn query_result(
    State(state): State<Arc<BaktaHandler>>,
    Json(job): Json<Job>,
) -> impl IntoResponse {
    match state
        .state_handler
        .get_results(job, &state.s3_handler)
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
    }
}

/// Start a job
#[utoipa::path(
    post,
    path = "/api/v1/job/start",
    request_body = StartRequest,
    responses(
        (status = 200, body = ()),
        (status = 400, body = String)
    )
)]
pub async fn start_job(
    State(state): State<Arc<BaktaHandler>>,
    headers: HeaderMap,
    Json(start_request): Json<StartRequest>,
) -> impl IntoResponse {
    let tool_version = state.version.tool.clone();

    let origin = headers.get("origin").and_then(|o| {
        o.to_str().ok().map(|e| {
            REGEX
                .replace_all(e.strip_prefix("https://").unwrap_or(e), "_")
                .to_string()
        })
    });

    match state
        .state_handler
        .start_job(start_request, tool_version, origin)
        .await
    {
        Ok(_) => (StatusCode::OK, Json(())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
    }
}

/// Get the current version
#[utoipa::path(
    get,
    path = "/api/v1/version",
    responses(
        (status = 200, body = VersionResponse)
    )
)]
pub async fn version(State(state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    Json(state.version.clone())
}

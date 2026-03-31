use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use reqwest::StatusCode;

use crate::{
    api_structs::{
        InitRequest, InitResponse, Job, ListRequest, ListResponse, ResultResponse, StartRequest,
        VersionResponse,
    },
    bakta_handler::{BaktaHandler, REGEX},
};

/// Delete an existing V1 Bakta job.
#[utoipa::path(
    delete,
    path = "/api/v1/job/delete",
    operation_id = "deleteV1Job",
    summary = "Delete V1 job",
    description = "Deletes a V1 Bakta job and removes it from the in-memory job state if the provided `jobID` and `secret` match.",
    params(Job),
    responses(
        (status = 200, description = "Job deleted successfully."),
        (status = 400, body = String, description = "Job not found, unauthorized, or deletion failed.")
    ),
    tag = "v1",
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

/// Get raw stdout and stderr logs for a V1 job.
#[utoipa::path(
    get,
    path = "/api/v1/job/logs",
    operation_id = "getV1JobLogs",
    summary = "Fetch V1 job logs",
    description = "Returns the plain-text Bakta log stream for a V1 job. Combined or stage-aware logs are only available in V2.",
    params(Job),
    responses(
        (status = 200, body = String, description = "Plain-text workflow logs."),
        (status = 400, body = String, description = "Job not found, unauthorized, or logs unavailable.")
    ),
    tag = "v1",
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

/// Create a new V1 Bakta job and return upload URLs.
#[utoipa::path(
    post,
    path = "/api/v1/job/init",
    operation_id = "initV1Job",
    summary = "Initialize V1 job",
    description = "Creates a new V1 Bakta job, returns a `jobID` plus `secret`, and presigns the fixed upload slots for genome FASTA, Prodigal training data, and replicons.",
    request_body = InitRequest,
    responses(
        (status = 200, body = InitResponse, description = "Job credentials and upload URLs."),
        (status = 400, body = String, description = "Failed to create the job or sign upload URLs.")
    ),
    tag = "v1",
)]
pub async fn init_job(
    State(state): State<Arc<BaktaHandler>>,
    Json(init_request): Json<InitRequest>,
) -> impl IntoResponse {
    let (id, secret) = state.state_handler.init_job(init_request.name).await;

    let (Ok(fasta_url), Ok(prodigal_url), Ok(replicon_url)) = (
        state
            .s3_handler
            .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Fasta)
            .await,
        state
            .s3_handler
            .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::Prodigal)
            .await,
        state
            .s3_handler
            .sign_upload_url(&id.to_string(), crate::s3_handler::InputType::RepliconsTSV)
            .await,
    ) else {
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

/// List the status of multiple V1 jobs.
#[utoipa::path(
    post,
    path = "/api/v1/job/list",
    operation_id = "listV1Jobs",
    summary = "List V1 jobs",
    description = "Returns the current state for multiple V1 jobs in a single request. Jobs that cannot be resolved are returned in the `failedJobs` list.",
    request_body = ListRequest,
    responses(
        (status = 200, body = ListResponse, description = "Current state of the requested jobs.")
    ),
    tag = "v1",
)]
pub async fn list_jobs(
    State(state): State<Arc<BaktaHandler>>,
    Json(list_request): Json<ListRequest>,
) -> impl IntoResponse {
    Json(state.state_handler.get_job_states(list_request.jobs).await)
}

/// Fetch the signed result artifact URLs for a finished V1 job.
#[utoipa::path(
    post,
    path = "/api/v1/job/result",
    operation_id = "getV1JobResult",
    summary = "Fetch V1 job result",
    description = "Returns presigned download URLs for the fixed Bakta result artifact set once the job has finished successfully.",
    request_body = Job,
    responses(
        (status = 200, body = ResultResponse, description = "Signed result artifact URLs for the finished job."),
        (status = 400, body = String, description = "Job not found, unauthorized, or not finished yet.")
    ),
    tag = "v1",
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

/// Start a V1 Bakta job after uploads are complete.
#[utoipa::path(
    post,
    path = "/api/v1/job/start",
    operation_id = "startV1Job",
    summary = "Start V1 job",
    description = "Submits the original Bakta genome annotation workflow to Argo using the uploaded inputs and the V1 parameter set.",
    request_body = StartRequest,
    responses(
        (status = 200, description = "Workflow submitted successfully."),
        (status = 400, body = String, description = "Job not found, unauthorized, or workflow submission failed.")
    ),
    tag = "v1",
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

/// Get V1 tool and backend version information.
#[utoipa::path(
    get,
    path = "/api/v1/version",
    operation_id = "getV1Version",
    summary = "Get V1 version information",
    description = "Returns the Bakta tool version, Bakta database version, and backend version exposed through the V1 compatibility API.",
    responses(
        (status = 200, body = VersionResponse, description = "Current V1 version information.")
    ),
    tag = "v1",
)]
pub async fn version(State(state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    Json(state.version.clone())
}

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::IntoResponse,
};
use reqwest::StatusCode;

use crate::{
    bakta_handler::{BaktaHandler, REGEX},
    v2::api_structs::{
        JobReference, UploadLink, V2InitRequest, V2InitResponse, V2ListRequest, V2ListResponse,
        V2LogsResponse, V2ResultResponse, V2StartRequest, V2VersionResponse,
        WorkflowDescriptorResponse,
    },
    workflow_catalog::{workflow_descriptor, workflow_descriptors},
};

/// List supported V2 workflows.
#[utoipa::path(
    get,
    path = "/api/v2/workflows",
    operation_id = "listV2Workflows",
    summary = "List V2 workflows",
    description = "Returns the static V2 workflow catalog, including required uploads, result families, and logical log stages for each supported workflow kind.",
    responses(
        (status = 200, body = [WorkflowDescriptorResponse], description = "Supported workflow definitions.")
    ),
    tag = "v2",
)]
pub async fn workflows() -> impl IntoResponse {
    Json(
        workflow_descriptors()
            .iter()
            .map(|descriptor| WorkflowDescriptorResponse {
                workflow_kind: descriptor.workflow_kind,
                result_kind: descriptor.result_kind,
                uploads: descriptor
                    .uploads
                    .iter()
                    .map(|upload| crate::v2::api_structs::UploadDescriptor {
                        upload_kind: upload.kind,
                        required: upload.required,
                    })
                    .collect(),
                stages: descriptor
                    .stages
                    .iter()
                    .map(|stage| stage.to_string())
                    .collect(),
            })
            .collect::<Vec<_>>(),
    )
}

/// Initialize a V2 job.
#[utoipa::path(
    post,
    path = "/api/v2/job/init",
    operation_id = "initV2Job",
    summary = "Initialize V2 job",
    description = "Creates a V2 job for a specific workflow kind and returns presigned upload URLs for all required and optional input slots of that workflow.",
    request_body = V2InitRequest,
    responses(
        (status = 200, body = V2InitResponse),
        (status = 400, body = String)
    ),
    tag = "v2",
)]
pub async fn init_job(
    State(state): State<Arc<BaktaHandler>>,
    Json(init_request): Json<V2InitRequest>,
) -> impl IntoResponse {
    let descriptor = workflow_descriptor(init_request.workflow_kind);
    let (job_id, secret) = state
        .state_handler
        .init_job_v2(init_request.name, init_request.workflow_kind)
        .await;
    let job_id_string = job_id.to_string();

    let mut uploads = Vec::with_capacity(descriptor.uploads.len());
    for upload in descriptor.uploads {
        let Ok(url) = state
            .s3_handler
            .sign_upload_url_v2(job_id_string.as_str(), upload.kind)
            .await
        else {
            return (
                StatusCode::BAD_REQUEST,
                Json("Failed to sign V2 upload URL".to_string()),
            )
                .into_response();
        };

        uploads.push(UploadLink {
            upload_kind: upload.kind,
            required: upload.required,
            url,
        });
    }

    (
        StatusCode::OK,
        Json(V2InitResponse {
            job: JobReference { secret, job_id },
            workflow_kind: init_request.workflow_kind,
            uploads,
        }),
    )
        .into_response()
}

/// List V2 jobs.
#[utoipa::path(
    post,
    path = "/api/v2/job/list",
    operation_id = "listV2Jobs",
    summary = "List V2 jobs",
    description = "Returns workflow-aware status information for multiple V2 jobs in a single request.",
    request_body = V2ListRequest,
    responses(
        (status = 200, body = V2ListResponse)
    ),
    tag = "v2",
)]
pub async fn list_jobs(
    State(state): State<Arc<BaktaHandler>>,
    Json(list_request): Json<V2ListRequest>,
) -> impl IntoResponse {
    Json(
        state
            .state_handler
            .get_job_states_v2(list_request.jobs)
            .await,
    )
}

/// Query the result of a V2 job.
#[utoipa::path(
    post,
    path = "/api/v2/job/result",
    operation_id = "getV2JobResult",
    summary = "Fetch V2 job result",
    description = "Returns signed download URLs for the finished V2 workflow. The `json` artifact is always required; other artifact URLs are omitted when the workflow did not produce the file.",
    request_body = JobReference,
    responses(
        (status = 200, body = V2ResultResponse, description = "Signed result artifact URLs. Optional artifacts are omitted when the corresponding result file is absent."),
        (status = 400, body = String)
    ),
    tag = "v2",
)]
pub async fn query_result(
    State(state): State<Arc<BaktaHandler>>,
    Json(job): Json<JobReference>,
) -> impl IntoResponse {
    match state
        .state_handler
        .get_results_v2(job, &state.s3_handler)
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
    }
}

/// Start a V2 job.
#[utoipa::path(
    post,
    path = "/api/v2/job/start",
    operation_id = "startV2Job",
    summary = "Start V2 workflow",
    description = "Submits the selected V2 workflow template to Argo using the uploaded inputs and the workflow-specific configuration payload.",
    request_body = V2StartRequest,
    responses(
        (status = 200, body = ()),
        (status = 400, body = String)
    ),
    tag = "v2",
)]
pub async fn start_job(
    State(state): State<Arc<BaktaHandler>>,
    headers: HeaderMap,
    Json(start_request): Json<V2StartRequest>,
) -> impl IntoResponse {
    let origin = headers.get("origin").and_then(|o| {
        o.to_str().ok().map(|e| {
            REGEX
                .replace_all(e.strip_prefix("https://").unwrap_or(e), "_")
                .to_string()
        })
    });

    match state
        .state_handler
        .start_job_v2(start_request, origin)
        .await
    {
        Ok(_) => (StatusCode::OK, Json(())).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
    }
}

/// Get logs of a V2 job.
#[utoipa::path(
    get,
    path = "/api/v2/job/logs",
    operation_id = "getV2JobLogs",
    summary = "Fetch V2 structured logs",
    description = "Returns structured logs grouped by logical workflow stage. Combined workflows, such as Bakta plus Baktfold, expose one log section per stage.",
    params(JobReference),
    responses(
        (status = 200, body = V2LogsResponse),
        (status = 400, body = String)
    ),
    tag = "v2",
)]
pub async fn job_logs(
    State(state): State<Arc<BaktaHandler>>,
    Query(job): Query<JobReference>,
) -> impl IntoResponse {
    match state.state_handler.get_logs_v2(job).await {
        Ok(logs) => (StatusCode::OK, Json(logs)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(e.to_string())).into_response(),
    }
}

/// Delete a V2 job.
#[utoipa::path(
    delete,
    path = "/api/v2/job/delete",
    operation_id = "deleteV2Job",
    summary = "Delete V2 job",
    description = "Deletes a V2 workflow and removes its internal job state if the provided `job_id` and `secret` match.",
    params(JobReference),
    responses(
        (status = 200, body = ()),
        (status = 400, body = String)
    ),
    tag = "v2",
)]
pub async fn delete_job(
    State(state): State<Arc<BaktaHandler>>,
    Query(job): Query<JobReference>,
) -> impl IntoResponse {
    state
        .state_handler
        .delete_job((job.job_id, job.secret))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

/// Get V2 version information.
#[utoipa::path(
    get,
    path = "/api/v2/version",
    operation_id = "getV2Version",
    summary = "Get V2 version information",
    description = "Returns backend, Bakta, Bakta database, Baktfold, and Baktfold database versions used by the V2 API.",
    responses(
        (status = 200, body = V2VersionResponse)
    ),
    tag = "v2",
)]
pub async fn version(State(state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    Json(state.version_v2.clone())
}

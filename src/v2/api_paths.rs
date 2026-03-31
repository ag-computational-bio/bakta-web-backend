use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use reqwest::StatusCode;

use crate::{
    bakta_handler::BaktaHandler,
    v2::api_structs::{
        JobReference, V2InitRequest, V2ListRequest, V2VersionResponse, WorkflowDescriptorResponse,
    },
    workflow_catalog::workflow_descriptors,
};

fn not_implemented() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json("V2 route is registered but not implemented yet".to_string()),
    )
        .into_response()
}

/// List supported V2 workflows.
#[utoipa::path(
    get,
    path = "/api/v2/workflows",
    responses(
        (status = 200, body = [WorkflowDescriptorResponse])
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
    request_body = V2InitRequest,
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn init_job(
    State(_state): State<Arc<BaktaHandler>>,
    Json(_init_request): Json<V2InitRequest>,
) -> impl IntoResponse {
    not_implemented()
}

/// List V2 jobs.
#[utoipa::path(
    post,
    path = "/api/v2/job/list",
    request_body = V2ListRequest,
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn list_jobs(
    State(_state): State<Arc<BaktaHandler>>,
    Json(_list_request): Json<V2ListRequest>,
) -> impl IntoResponse {
    not_implemented()
}

/// Query the result of a V2 job.
#[utoipa::path(
    post,
    path = "/api/v2/job/result",
    request_body = JobReference,
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn query_result(
    State(_state): State<Arc<BaktaHandler>>,
    Json(_job): Json<JobReference>,
) -> impl IntoResponse {
    not_implemented()
}

/// Start a V2 job.
#[utoipa::path(
    post,
    path = "/api/v2/job/start",
    request_body = crate::v2::api_structs::V2StartRequest,
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn start_job(
    State(_state): State<Arc<BaktaHandler>>,
    Json(_start_request): Json<crate::v2::api_structs::V2StartRequest>,
) -> impl IntoResponse {
    not_implemented()
}

/// Get logs of a V2 job.
#[utoipa::path(
    get,
    path = "/api/v2/job/logs",
    params(JobReference),
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn job_logs(
    State(_state): State<Arc<BaktaHandler>>,
    Query(_job): Query<JobReference>,
) -> impl IntoResponse {
    not_implemented()
}

/// Delete a V2 job.
#[utoipa::path(
    delete,
    path = "/api/v2/job/delete",
    params(JobReference),
    responses(
        (status = 501, body = String)
    ),
    tag = "v2",
)]
pub async fn delete_job(
    State(_state): State<Arc<BaktaHandler>>,
    Query(_job): Query<JobReference>,
) -> impl IntoResponse {
    not_implemented()
}

/// Get V2 version information.
#[utoipa::path(
    get,
    path = "/api/v2/version",
    responses(
        (status = 501, body = String),
        (status = 200, body = V2VersionResponse)
    ),
    tag = "v2",
)]
pub async fn version(State(_state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    not_implemented()
}

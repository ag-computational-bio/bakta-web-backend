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
        JobReference, UploadLink, V2InitRequest, V2InitResponse, V2ListRequest, V2ListResponse,
        V2ResultResponse, V2VersionResponse, WorkflowDescriptorResponse,
    },
    workflow_catalog::{workflow_descriptor, workflow_descriptors},
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
    request_body = JobReference,
    responses(
        (status = 200, body = V2ResultResponse),
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
    responses(
        (status = 200, body = V2VersionResponse)
    ),
    tag = "v2",
)]
pub async fn version(State(state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    Json(state.version_v2.clone())
}

//! Admin handlers that change Argo workflows: retry, resubmit, stop, delete and cleanup.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::Utc;
use uuid::Uuid;

use super::{Admin, AdminError, structs::*};
use crate::{
    api_structs::ArgoStatus,
    argo::structs::ArgoWorkflow,
    bakta_handler::{BaktaHandler, FullJobState},
    stats::StatsEvent,
};

/// What the backend knows about a job: the in-memory state and/or the workflow
/// name from the statistics database.
struct JobTarget {
    in_memory: Option<FullJobState>,
    workflow_name: Option<String>,
}

async fn find_job(state: &BaktaHandler, id: Uuid) -> Result<JobTarget, AdminError> {
    let handler = &state.state_handler;
    if let Some(job) = handler.job_state.read().await.get(&id).cloned() {
        return Ok(JobTarget {
            workflow_name: job.workflowname.clone(),
            in_memory: Some(job),
        });
    }
    let record = match handler.stats.db() {
        Some(db) => db.get_job(id).await.map_err(AdminError::database)?,
        None => None,
    }
    .ok_or_else(|| AdminError::NotFound("Job not found".to_string()))?;
    Ok(JobTarget {
        in_memory: None,
        workflow_name: record.workflow_name,
    })
}

fn require_workflow(target: &JobTarget) -> Result<String, AdminError> {
    target
        .workflow_name
        .clone()
        .ok_or_else(|| AdminError::Conflict("Job has no workflow".to_string()))
}

async fn current_phase(state: &BaktaHandler, workflow_name: &str) -> Result<String, AdminError> {
    Ok(state
        .state_handler
        .argo_client
        .get_workflow(workflow_name)
        .await
        .map_err(AdminError::argo)?
        .status
        .phase)
}

fn response_phase(workflow: &ArgoWorkflow, fallback: &str) -> String {
    if workflow.status.phase.is_empty() {
        fallback.to_string()
    } else {
        workflow.status.phase.clone()
    }
}

/// Argo retry of a failed or errored workflow.
#[utoipa::path(
    post,
    path = "/api/admin/jobs/{id}/retry",
    operation_id = "retryAdminJob",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 409, body = AdminErrorResponse, description = "Workflow is not in Failed or Error."),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn retry_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    let workflow_name = require_workflow(&find_job(&state, id).await?)?;
    let phase = current_phase(&state, &workflow_name).await?;
    if !matches!(phase.as_str(), "Failed" | "Error") {
        return Err(AdminError::Conflict(format!(
            "Workflow is {phase}, only Failed or Error workflows can be retried"
        )));
    }
    let handler = &state.state_handler;
    let workflow = handler
        .argo_client
        .retry_workflow(&workflow_name)
        .await
        .map_err(AdminError::argo)?;
    tracing::info!(user = %user.username, job_id = %id, workflow_name = %workflow_name, "admin_retry");
    if let Some(job) = handler.job_state.write().await.get_mut(&id)
        && job.workflowname.as_deref() == Some(workflow_name.as_str())
    {
        job.status = Some(ArgoStatus::Running);
        job.updated = Some(Utc::now());
    }
    handler.stats.record(StatsEvent::Retried { id });
    Ok(Json(WorkflowActionResponse {
        phase: response_phase(&workflow, "Running"),
        workflow_name,
    }))
}

/// Argo resubmit: a new workflow with the same parameters.
#[utoipa::path(
    post,
    path = "/api/admin/jobs/{id}/resubmit",
    operation_id = "resubmitAdminJob",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 409, body = AdminErrorResponse),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn resubmit_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    let old_name = require_workflow(&find_job(&state, id).await?)?;
    let handler = &state.state_handler;
    let workflow = handler
        .argo_client
        .resubmit_workflow(&old_name)
        .await
        .map_err(AdminError::argo)?;
    let new_name = workflow.metadata.name.clone();
    let now = Utc::now();
    tracing::info!(
        user = %user.username, job_id = %id, workflow_name = %old_name,
        new_workflow_name = %new_name, "admin_resubmit"
    );
    if let Some(job) = handler.job_state.write().await.get_mut(&id) {
        job.workflowname = Some(new_name.clone());
        job.status = Some(ArgoStatus::Pending);
        job.started = Some(workflow.metadata.creation_timestamp.unwrap_or(now));
        job.updated = Some(now);
        job.archived = false;
        job.argo_uid = None;
    }
    handler.stats.record(StatsEvent::Resubmitted {
        id,
        workflow_name: new_name.clone(),
        submitted_at: now,
    });
    Ok(Json(WorkflowActionResponse {
        phase: response_phase(&workflow, "Pending"),
        workflow_name: new_name,
    }))
}

/// Argo terminate of a running workflow.
#[utoipa::path(
    post,
    path = "/api/admin/jobs/{id}/stop",
    operation_id = "stopAdminJob",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 409, body = AdminErrorResponse, description = "Workflow is not running."),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stop_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    let workflow_name = require_workflow(&find_job(&state, id).await?)?;
    let phase = current_phase(&state, &workflow_name).await?;
    if !matches!(phase.as_str(), "Running" | "Pending") {
        return Err(AdminError::Conflict(format!(
            "Workflow is {phase}, only running workflows can be stopped"
        )));
    }
    let workflow = state
        .state_handler
        .argo_client
        .terminate_workflow(&workflow_name)
        .await
        .map_err(AdminError::argo)?;
    tracing::info!(user = %user.username, job_id = %id, workflow_name = %workflow_name, "admin_stop");
    Ok(Json(WorkflowActionResponse {
        phase: response_phase(&workflow, "Failed"),
        workflow_name,
    }))
}

/// Deletes the job's workflow and marks the job as deleted.
#[utoipa::path(
    delete,
    path = "/api/admin/jobs/{id}",
    operation_id = "deleteAdminJob",
    params(("id" = Uuid, Path, description = "Job ID"), DeleteJobParams),
    responses(
        (status = 200, body = DeleteJobResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 409, body = AdminErrorResponse, description = "The job is being started."),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn delete_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
    Query(params): Query<DeleteJobParams>,
) -> Result<Json<DeleteJobResponse>, AdminError> {
    let target = find_job(&state, id).await?;
    let handler = &state.state_handler;
    if handler.is_starting(&id) {
        return Err(AdminError::Conflict(
            "Job is starting, try again later".to_string(),
        ));
    }
    let deleted_workflow = match (&target.in_memory, &target.workflow_name) {
        (Some(job), _) if job.archived => {
            handler
                .argo_client
                .delete_workflow(job)
                .await
                .map_err(AdminError::argo)?;
            true
        }
        (_, Some(name)) => handler
            .argo_client
            .delete_workflow_by_name(name)
            .await
            .map_err(AdminError::argo)?,
        (_, None) => false,
    };
    let purged_objects = if params.purge_data.unwrap_or(false) {
        state
            .s3_handler
            .purge_job_objects(&id.to_string())
            .await
            .map_err(|e| AdminError::BadGateway(format!("S3 purge failed: {e}")))?
    } else {
        0
    };
    handler.job_state.write().await.remove(&id);
    handler.stats.record(StatsEvent::Deleted { id });
    tracing::info!(
        user = %user.username, job_id = %id,
        workflow_name = target.workflow_name.as_deref().unwrap_or(""),
        deleted_workflow, purged_objects, "admin_delete"
    );
    Ok(Json(DeleteJobResponse {
        deleted_workflow,
        purged_objects,
    }))
}

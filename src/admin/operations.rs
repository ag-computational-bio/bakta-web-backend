//! Admin handlers that change Argo workflows: retry, resubmit, stop, delete and cleanup.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use chrono::Utc;
use uuid::Uuid;

use super::{Admin, AdminError, parse_environment, stats_db, structs::*};
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

async fn environment_workflows(
    state: &BaktaHandler,
    environment: Option<&str>,
) -> Result<Vec<ArgoWorkflow>, AdminError> {
    let environment = parse_environment(environment, &state.state_handler.environment)?;
    let selector = environment.map(|environment| format!("bakta-environment={environment}"));
    state
        .state_handler
        .argo_client
        .list_workflows(selector.as_deref())
        .await
        .map_err(AdminError::argo)
}

/// Live Argo workflows of this backend.
#[utoipa::path(
    get,
    path = "/api/admin/workflows",
    operation_id = "listAdminWorkflows",
    params(WorkflowListParams),
    responses(
        (status = 200, body = WorkflowListResponse),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn list_workflows(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(params): Query<WorkflowListParams>,
) -> Result<Json<WorkflowListResponse>, AdminError> {
    let phases: Vec<String> = params
        .phase
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|phase| !phase.is_empty())
        .map(str::to_string)
        .collect();
    let mut workflows: Vec<_> = environment_workflows(&state, params.environment.as_deref())
        .await?
        .into_iter()
        .filter(|workflow| phases.is_empty() || phases.contains(&workflow.status.phase))
        .collect();
    if params.orphans.unwrap_or(false) {
        let job_id = |workflow: &ArgoWorkflow| {
            workflow
                .metadata
                .labels
                .get("jobid")
                .and_then(|value| value.parse::<Uuid>().ok())
        };
        let ids: Vec<Uuid> = workflows.iter().filter_map(job_id).collect();
        let known = stats_db(&state)?
            .existing_job_ids(&ids)
            .await
            .map_err(AdminError::database)?;
        workflows.retain(|workflow| job_id(workflow).is_none_or(|id| !known.contains(&id)));
    }
    let items = workflows
        .into_iter()
        .map(|workflow| {
            let labels = &workflow.metadata.labels;
            WorkflowSummary {
                job_id: labels.get("jobid").cloned(),
                workflow_kind: labels.get("workflow-kind").cloned(),
                environment: labels.get("bakta-environment").cloned(),
                template: workflow
                    .spec
                    .workflow_template_ref
                    .map(|template| template.name),
                name: workflow.metadata.name,
                phase: workflow.status.phase,
                started_at: workflow.status.started_at,
                finished_at: workflow.status.finished_at,
                message: workflow.status.message,
            }
        })
        .collect();
    Ok(Json(WorkflowListResponse { items }))
}

/// Deletes old workflows in the given phases. Lists them only with `dry_run`.
#[utoipa::path(
    post,
    path = "/api/admin/cleanup",
    operation_id = "cleanupAdminWorkflows",
    request_body = CleanupRequest,
    responses(
        (status = 200, body = CleanupResponse),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn cleanup(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Json(request): Json<CleanupRequest>,
) -> Result<Json<CleanupResponse>, AdminError> {
    let phases = request
        .phases
        .unwrap_or_else(|| vec!["Failed".to_string(), "Error".to_string()]);
    let older_than_hours = request.older_than_hours.unwrap_or(24.0);
    if !older_than_hours.is_finite() || older_than_hours < 0.0 {
        return Err(AdminError::BadRequest(
            "older_than_hours must be zero or positive".to_string(),
        ));
    }
    let dry_run = request.dry_run.unwrap_or(true);
    let cutoff = Utc::now() - chrono::Duration::seconds((older_than_hours * 3600.0) as i64);

    let matching: Vec<_> = environment_workflows(&state, request.environment.as_deref())
        .await?
        .into_iter()
        .filter(|workflow| phases.contains(&workflow.status.phase))
        .filter(|workflow| {
            workflow
                .status
                .finished_at
                .is_some_and(|finished_at| finished_at < cutoff)
        })
        .filter(|workflow| {
            request
                .workflow_names
                .as_ref()
                .is_none_or(|names| names.contains(&workflow.metadata.name))
        })
        .collect();

    let mut workflows = Vec::new();
    for workflow in matching {
        if !dry_run {
            match state
                .state_handler
                .argo_client
                .delete_workflow_by_name(&workflow.metadata.name)
                .await
            {
                Ok(_) => tracing::info!(
                    user = %user.username, workflow_name = %workflow.metadata.name,
                    job_id = workflow.metadata.labels.get("jobid").map(String::as_str).unwrap_or(""),
                    "admin_cleanup_delete"
                ),
                Err(error) => {
                    tracing::warn!(%error, workflow_name = %workflow.metadata.name, "admin_cleanup_delete_failed");
                    continue;
                }
            }
        }
        workflows.push(CleanupItem {
            job_id: workflow.metadata.labels.get("jobid").cloned(),
            name: workflow.metadata.name,
            phase: workflow.status.phase,
            finished_at: workflow.status.finished_at,
        });
    }
    Ok(Json(CleanupResponse { dry_run, workflows }))
}

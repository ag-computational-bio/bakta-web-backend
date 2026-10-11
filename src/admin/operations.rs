//! Admin handlers that change Argo workflows: retry, resubmit, stop, resume, delete and bulk actions.

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
    workflow_catalog::is_bakta_web_template,
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

/// Updates the job state and record after an Argo retry.
async fn after_retry(state: &BaktaHandler, id: Uuid, workflow_name: &str) {
    let handler = &state.state_handler;
    if let Some(job) = handler.job_state.write().await.get_mut(&id)
        && job.workflowname.as_deref() == Some(workflow_name)
    {
        job.status = Some(ArgoStatus::Running);
        job.updated = Some(Utc::now());
    }
    handler.stats.record(StatsEvent::Retried {
        id,
        workflow_name: workflow_name.to_string(),
    });
}

/// Points the job state and record to the new workflow after an Argo resubmit
/// of `old_name`.
async fn after_resubmit(state: &BaktaHandler, id: Uuid, old_name: &str, workflow: &ArgoWorkflow) {
    let handler = &state.state_handler;
    let now = Utc::now();
    if let Some(job) = handler.job_state.write().await.get_mut(&id)
        && job
            .workflowname
            .as_deref()
            .is_none_or(|name| name == old_name)
    {
        job.workflowname = Some(workflow.metadata.name.clone());
        job.status = Some(ArgoStatus::Pending);
        job.started = Some(workflow.metadata.creation_timestamp.unwrap_or(now));
        job.updated = Some(now);
        job.archived = false;
        job.argo_uid = None;
    }
    handler.stats.record(StatsEvent::Resubmitted {
        id,
        previous_workflow_name: old_name.to_string(),
        workflow_name: workflow.metadata.name.clone(),
        submitted_at: now,
    });
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
    after_retry(&state, id, &workflow_name).await;
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
    tracing::info!(
        user = %user.username, job_id = %id, workflow_name = %old_name,
        new_workflow_name = %new_name, "admin_resubmit"
    );
    after_resubmit(&state, id, &old_name, &workflow).await;
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

/// Argo resume of a suspended workflow.
#[utoipa::path(
    post,
    path = "/api/admin/jobs/{id}/resume",
    operation_id = "resumeAdminJob",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = WorkflowActionResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 409, body = AdminErrorResponse, description = "Workflow is not suspended."),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn resume_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    let workflow_name = require_workflow(&find_job(&state, id).await?)?;
    let argo = &state.state_handler.argo_client;
    let current = argo
        .get_workflow(&workflow_name)
        .await
        .map_err(AdminError::argo)?;
    if !current.is_suspended() {
        return Err(AdminError::Conflict(
            "Workflow is not suspended".to_string(),
        ));
    }
    let workflow = argo
        .resume_workflow(&workflow_name)
        .await
        .map_err(AdminError::argo)?;
    tracing::info!(user = %user.username, job_id = %id, workflow_name = %workflow_name, "admin_resume");
    Ok(Json(WorkflowActionResponse {
        phase: response_phase(&workflow, "Running"),
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
    let handler = &state.state_handler;
    // Same reservation as a start: refuse while the job is starting, and keep
    // a start from beginning until the delete is done.
    let _reservation = handler
        .reserve_start(id)
        .map_err(|_| AdminError::Conflict("Job is starting, try again later".to_string()))?;
    let target = find_job(&state, id).await?;
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

/// True for workflows created by Bakta Web: they have a `bakta-environment`
/// label, or (created before that label existed) a `jobid` label and a
/// Bakta Web workflow template.
fn is_bakta_web_workflow(workflow: &ArgoWorkflow) -> bool {
    let labels = &workflow.metadata.labels;
    labels.contains_key("bakta-environment")
        || (labels.contains_key("jobid")
            && workflow
                .spec
                .workflow_template_ref
                .as_ref()
                .is_some_and(|template| is_bakta_web_template(&template.name)))
}

/// Live Bakta Web workflows of one environment, or of all with `all`.
/// Other workflows in the shared namespace are never returned.
async fn environment_workflows(
    state: &BaktaHandler,
    environment: Option<&str>,
) -> Result<Vec<ArgoWorkflow>, AdminError> {
    let environment = parse_environment(environment, &state.state_handler.environment)?;
    let selector = environment.map(|environment| format!("bakta-environment={environment}"));
    let mut workflows = state
        .state_handler
        .argo_client
        .list_workflows(selector.as_deref())
        .await
        .map_err(AdminError::argo)?;
    workflows.retain(is_bakta_web_workflow);
    Ok(workflows)
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
            let suspended = workflow.is_suspended();
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
                suspended,
                message: workflow.status.message,
            }
        })
        .collect();
    Ok(Json(WorkflowListResponse { items }))
}

fn bulk_applicable(action: BulkAction, workflow: &ArgoWorkflow) -> bool {
    let phase = workflow.status.phase.as_str();
    match action {
        BulkAction::Delete => true,
        BulkAction::Stop => matches!(phase, "Pending" | "Running"),
        BulkAction::Retry => matches!(phase, "Failed" | "Error"),
        BulkAction::Resume => workflow.is_suspended(),
        BulkAction::Resubmit => matches!(phase, "Succeeded" | "Failed" | "Error"),
    }
}

fn bulk_matches(
    filter: &BulkFilter,
    cutoff: Option<chrono::DateTime<Utc>>,
    workflow: &ArgoWorkflow,
) -> bool {
    let labels = &workflow.metadata.labels;
    let q_matches = filter
        .q
        .as_deref()
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty())
        .is_none_or(|q| {
            workflow.metadata.name.to_lowercase().contains(&q)
                || labels
                    .get("jobid")
                    .is_some_and(|id| id.to_lowercase().contains(&q))
        });
    let phase_matches = filter
        .phases
        .as_ref()
        .is_none_or(|phases| phases.is_empty() || phases.contains(&workflow.status.phase));
    let kind_matches = filter.workflow_kinds.as_ref().is_none_or(|kinds| {
        kinds.is_empty()
            || labels
                .get("workflow-kind")
                .is_some_and(|kind| kinds.contains(kind))
    });
    let age_matches = cutoff.is_none_or(|cutoff| {
        workflow
            .status
            .finished_at
            .is_some_and(|finished_at| finished_at < cutoff)
    });
    q_matches && phase_matches && kind_matches && age_matches
}

/// The job's current workflow when `workflow` belongs to a job that has
/// another current workflow (after a resubmit).
async fn superseded_by(state: &BaktaHandler, workflow: &ArgoWorkflow) -> Option<String> {
    let id = workflow
        .metadata
        .labels
        .get("jobid")?
        .parse::<Uuid>()
        .ok()?;
    let current = find_job(state, id).await.ok()?.workflow_name?;
    (current != workflow.metadata.name).then_some(current)
}

/// Runs one bulk action on one workflow.
async fn run_bulk_action(
    state: &BaktaHandler,
    action: BulkAction,
    workflow: &ArgoWorkflow,
) -> anyhow::Result<()> {
    let argo = &state.state_handler.argo_client;
    let name = &workflow.metadata.name;
    let job_id = workflow
        .metadata
        .labels
        .get("jobid")
        .and_then(|value| value.parse::<Uuid>().ok());
    match action {
        BulkAction::Delete => {
            argo.delete_workflow_by_name(name).await?;
        }
        BulkAction::Stop => {
            argo.terminate_workflow(name).await?;
        }
        BulkAction::Resume => {
            argo.resume_workflow(name).await?;
        }
        BulkAction::Retry => {
            argo.retry_workflow(name).await?;
            if let Some(id) = job_id {
                after_retry(state, id, name).await;
            }
        }
        BulkAction::Resubmit => {
            let new_workflow = argo.resubmit_workflow(name).await?;
            if let Some(id) = job_id {
                after_resubmit(state, id, name, &new_workflow).await;
            }
        }
    }
    Ok(())
}

/// Runs one action on all live workflows that match a filter. Lists them only with `dry_run`.
#[utoipa::path(
    post,
    path = "/api/admin/workflows/bulk",
    operation_id = "bulkAdminWorkflows",
    request_body = BulkRequest,
    responses(
        (status = 200, body = BulkResponse),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn bulk_workflows(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Json(request): Json<BulkRequest>,
) -> Result<Json<BulkResponse>, AdminError> {
    let cutoff = match request.filter.finished_before_hours {
        Some(hours) if !hours.is_finite() || hours < 0.0 => {
            return Err(AdminError::BadRequest(
                "finished_before_hours must be zero or positive".to_string(),
            ));
        }
        Some(hours) => Some(Utc::now() - chrono::Duration::seconds((hours * 3600.0) as i64)),
        None => None,
    };
    let dry_run = request.dry_run.unwrap_or(true);
    let action = request.action;

    let workflows: Vec<_> = environment_workflows(&state, request.filter.environment.as_deref())
        .await?
        .into_iter()
        .filter(|workflow| bulk_matches(&request.filter, cutoff, workflow))
        .filter(|workflow| {
            request
                .workflow_names
                .as_ref()
                .is_none_or(|names| names.contains(&workflow.metadata.name))
        })
        .collect();

    let mut items = Vec::with_capacity(workflows.len());
    for workflow in workflows {
        let mut reason = (!bulk_applicable(action, &workflow)).then(|| {
            format!(
                "Action does not apply to a {} workflow",
                workflow.status.phase
            )
        });
        // Retry and resubmit only act on the job's current workflow.
        if reason.is_none()
            && matches!(action, BulkAction::Retry | BulkAction::Resubmit)
            && let Some(current) = superseded_by(&state, &workflow).await
        {
            reason = Some(format!("Workflow was superseded by {current}"));
        }
        let applicable = reason.is_none();
        let (result, error) = if dry_run {
            (None, None)
        } else if !applicable {
            (Some(BulkResult::Skipped), None)
        } else {
            match run_bulk_action(&state, action, &workflow).await {
                Ok(()) => {
                    tracing::info!(
                        user = %user.username, ?action, workflow_name = %workflow.metadata.name,
                        job_id = workflow.metadata.labels.get("jobid").map(String::as_str).unwrap_or(""),
                        "admin_bulk_action"
                    );
                    (Some(BulkResult::Done), None)
                }
                Err(error) => {
                    tracing::warn!(
                        user = %user.username, ?action, workflow_name = %workflow.metadata.name,
                        %error, "admin_bulk_action_failed"
                    );
                    (Some(BulkResult::Failed), Some(error.to_string()))
                }
            }
        };
        let labels = &workflow.metadata.labels;
        items.push(BulkItem {
            job_id: labels.get("jobid").cloned(),
            workflow_kind: labels.get("workflow-kind").cloned(),
            name: workflow.metadata.name,
            phase: workflow.status.phase,
            finished_at: workflow.status.finished_at,
            applicable,
            result,
            error,
            reason,
        });
    }
    Ok(Json(BulkResponse {
        dry_run,
        action,
        items,
    }))
}

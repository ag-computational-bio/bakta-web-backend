//! Admin handlers that change Argo workflows: retry, resubmit, stop, resume, delete and bulk actions.

use std::{collections::hash_map::Entry, sync::Arc};

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    Admin, AdminError,
    extract::{self, Path, Query},
    parse_environment, stats_db,
    structs::*,
};
use crate::{
    api_structs::ArgoStatus,
    argo::structs::ArgoWorkflow,
    bakta_handler::{
        BaktaHandler, FullJobState, SubmissionUnknown, WorkflowLookupFailed, resubmitted_job_state,
        state_reference_time, workflow_environment,
    },
    retention::{data_expires_at, is_data_available, reference_time},
    stats::StatsEvent,
    workflow_catalog::is_bakta_web_template,
};

/// What the backend knows about a job: the in-memory state and/or the workflow
/// name from the statistics database.
struct JobTarget {
    in_memory: Option<FullJobState>,
    workflow_name: Option<String>,
    /// The job record has status `deleted`.
    deleted: bool,
}

async fn find_job(state: &BaktaHandler, id: Uuid) -> Result<JobTarget, AdminError> {
    let handler = &state.state_handler;
    if let Some(job) = handler.job_state.read().await.get(&id).cloned() {
        return Ok(JobTarget {
            workflow_name: job.workflowname.clone(),
            in_memory: Some(job),
            deleted: false,
        });
    }
    let record = match handler.stats.db() {
        Some(db) => db.get_job(id).await.map_err(AdminError::database)?,
        None => None,
    }
    .ok_or_else(|| AdminError::NotFound("Job not found".to_string()))?;
    Ok(JobTarget {
        in_memory: None,
        deleted: record.status == "deleted",
        workflow_name: record.workflow_name,
    })
}

/// True when the job was deleted: it is in the backend's list of deleted
/// jobs, or it is not in memory and its record has status `deleted`. A
/// failed database lookup counts as not deleted.
async fn job_deleted(state: &BaktaHandler, id: Uuid) -> bool {
    let handler = &state.state_handler;
    if handler.is_deleted(&id) {
        return true;
    }
    if handler.job_state.read().await.contains_key(&id) {
        return false;
    }
    match handler.stats.db() {
        Some(db) => db
            .get_job(id)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, job_id = %id, "admin_deleted_lookup_failed");
                None
            })
            .is_some_and(|record| record.status == "deleted"),
        None => false,
    }
}

const INPUT_DATA_DELETED: &str = "Input data was deleted after the retention period";
/// A start, delete or resubmit of the job is in progress.
const JOB_BUSY: &str = "Job is busy, try again later";
/// The job was deleted. A resubmit would add it back.
const JOB_DELETED: &str = "Job was deleted";

/// True while the job's S3 data exists. The reference time comes from the
/// job record, or from the in-memory state when there is no record.
pub(super) async fn job_data_available(
    state: &BaktaHandler,
    id: Uuid,
    in_memory: Option<&FullJobState>,
) -> bool {
    let record = match state.state_handler.stats.db() {
        Some(db) => db.get_job(id).await.unwrap_or_else(|error| {
            tracing::warn!(%error, job_id = %id, "admin_retention_lookup_failed");
            None
        }),
        None => None,
    };
    let reference = match (&record, in_memory) {
        (Some(record), _) => Some(reference_time(record.finished_at, record.created_at)),
        (None, Some(job)) => state_reference_time(job),
        (None, None) => None,
    };
    reference.is_none_or(|reference| {
        is_data_available(
            data_expires_at(reference, state.data_retention_days),
            Utc::now(),
        )
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

/// Updates the job state and record after an Argo retry requested at
/// `retried_at`. The event is recorded while the job state lock is held, so
/// it is ordered with the status updates of the polling loop. A finish seen
/// after `retried_at` belongs to the new attempt and is kept.
async fn after_retry(
    state: &BaktaHandler,
    id: Uuid,
    workflow_name: &str,
    retried_at: DateTime<Utc>,
) {
    let handler = &state.state_handler;
    let mut job_state = handler.job_state.write().await;
    if let Some(job) = job_state.get_mut(&id)
        && job.workflowname.as_deref() == Some(workflow_name)
        && job.updated.is_none_or(|updated| updated < retried_at)
    {
        job.status = Some(ArgoStatus::Running);
        job.updated = Some(Utc::now());
    }
    handler.stats.record(StatsEvent::Retried {
        id,
        workflow_name: workflow_name.to_string(),
        retried_at,
    });
    drop(job_state);
}

/// Points the job state and record to the new workflow after an Argo resubmit
/// of `old_name`. A job without job state gets one, so the next poll takes
/// the new workflow as current even when the database still names the old one.
async fn after_resubmit(state: &BaktaHandler, id: Uuid, old_name: &str, workflow: &ArgoWorkflow) {
    let handler = &state.state_handler;
    let now = Utc::now();
    let created_at = workflow.metadata.creation_timestamp.unwrap_or(now);
    // Record while holding the lock, so the event is ordered with the poll.
    let mut job_state = handler.job_state.write().await;
    match job_state.entry(id) {
        Entry::Occupied(mut entry) => {
            let job = entry.get_mut();
            if job
                .workflowname
                .as_deref()
                .is_none_or(|name| name == old_name)
            {
                job.workflowname = Some(workflow.metadata.name.clone());
                job.status = Some(ArgoStatus::Pending);
                job.started = Some(created_at);
                job.updated = Some(now);
                job.archived = false;
                job.argo_uid = None;
            }
        }
        Entry::Vacant(entry) => {
            entry.insert(resubmitted_job_state(
                id,
                &workflow.metadata.name,
                &workflow.metadata.labels,
                created_at,
                now,
            ));
        }
    }
    handler.stats.record(StatsEvent::Resubmitted {
        id,
        previous_workflow_name: old_name.to_string(),
        workflow_name: workflow.metadata.name.clone(),
        submitted_at: now,
    });
    drop(job_state);
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
        (status = 502, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse, description = "The statistics database failed.")
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn retry_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    // A resubmit may have created a newer workflow that is not known yet.
    if state.state_handler.is_submission_unknown(&id) {
        return Err(AdminError::Conflict(SubmissionUnknown.to_string()));
    }
    let target = find_job(&state, id).await?;
    let workflow_name = require_workflow(&target)?;
    if !job_data_available(&state, id, target.in_memory.as_ref()).await {
        return Err(AdminError::Conflict(INPUT_DATA_DELETED.to_string()));
    }
    let phase = current_phase(&state, &workflow_name).await?;
    if !matches!(phase.as_str(), "Failed" | "Error") {
        return Err(AdminError::Conflict(format!(
            "Workflow is {phase}, only Failed or Error workflows can be retried"
        )));
    }
    let handler = &state.state_handler;
    let retried_at = Utc::now();
    let workflow = handler
        .argo_client
        .retry_workflow(&workflow_name)
        .await
        .map_err(AdminError::argo)?;
    tracing::info!(user = %user.username, job_id = %id, workflow_name = %workflow_name, "admin_retry");
    after_retry(&state, id, &workflow_name, retried_at).await;
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
        (status = 502, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse, description = "The statistics database failed.")
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn resubmit_job(
    Admin(user): Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowActionResponse>, AdminError> {
    // Same reservation as a start or delete: a delete that runs at the same
    // time could otherwise remove the job while the new workflow runs.
    let _reservation = state
        .state_handler
        .reserve_start(id)
        .map_err(|_| AdminError::Conflict(JOB_BUSY.to_string()))?;
    let handler = &state.state_handler;
    // An earlier start or resubmit may have created a workflow that is not
    // known yet.
    if handler.is_submission_unknown(&id) {
        return Err(AdminError::Conflict(SubmissionUnknown.to_string()));
    }
    if handler.is_deleted(&id) {
        return Err(AdminError::Conflict(JOB_DELETED.to_string()));
    }
    let target = find_job(&state, id).await?;
    if target.deleted {
        return Err(AdminError::Conflict(JOB_DELETED.to_string()));
    }
    let old_name = require_workflow(&target)?;
    if !job_data_available(&state, id, target.in_memory.as_ref()).await {
        return Err(AdminError::Conflict(INPUT_DATA_DELETED.to_string()));
    }
    let requested_at = Utc::now();
    let workflow = handler
        .argo_client
        .resubmit_workflow(&old_name)
        .await
        .map_err(|error| {
            AdminError::argo(handler.recover_failed_resubmit(id, &old_name, requested_at, error))
        })?;
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
        (status = 409, body = AdminErrorResponse, description = "The job is being started, or the result of an earlier start request is not known yet."),
        (status = 502, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse, description = "The job has no known workflow and its workflows could not be looked up in Argo, or the statistics database failed.")
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
    // A workflow may exist that the backend does not know yet.
    if handler.is_submission_unknown(&id) {
        return Err(AdminError::Conflict(SubmissionUnknown.to_string()));
    }
    let target = find_job(&state, id).await?;
    let deleted_workflow = match (&target.in_memory, &target.workflow_name) {
        // Deletes the live workflow and the archive record.
        (Some(job), _) if job.archived => handler
            .argo_client
            .delete_workflow(job)
            .await
            .map_err(AdminError::argo)?,
        (_, Some(name)) => handler
            .argo_client
            .delete_workflow_by_name(name)
            .await
            .map_err(AdminError::argo)?,
        // A start may have created a workflow that the backend does not know.
        (_, None) => handler.delete_job_workflows(id).await.map_err(|error| {
            if error.is::<WorkflowLookupFailed>() {
                AdminError::Unavailable(error.to_string())
            } else {
                AdminError::argo(error)
            }
        })?,
    };
    // Without S3 data there is nothing to list or purge.
    let purge = params.purge_data.unwrap_or(false)
        && job_data_available(&state, id, target.in_memory.as_ref()).await;
    let purged_objects = if purge {
        state
            .s3_handler
            .purge_job_objects(&id.to_string())
            .await
            .map_err(|e| AdminError::BadGateway(format!("S3 purge failed: {e}")))?
    } else {
        0
    };
    let mut job_state = handler.job_state.write().await;
    handler.record_deleted(id);
    job_state.remove(&id);
    handler.stats.record(StatsEvent::Deleted { id });
    drop(job_state);
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
/// Other workflows in the shared namespace are never returned. The
/// environment is derived like in the poll, so workflows from before the
/// `bakta-environment` label are found too.
async fn environment_workflows(
    state: &BaktaHandler,
    environment: Option<&str>,
) -> Result<Vec<ArgoWorkflow>, AdminError> {
    let environment = parse_environment(environment, &state.state_handler.environment)?;
    let mut workflows = state
        .state_handler
        .argo_client
        .list_workflows(None)
        .await
        .map_err(AdminError::argo)?;
    workflows.retain(|workflow| {
        is_bakta_web_workflow(workflow)
            && environment.as_deref().is_none_or(|environment| {
                workflow_environment(&workflow.metadata.labels) == environment
            })
    });
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
                environment: Some(workflow_environment(labels).to_string()),
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

/// Why `action` does not apply to `workflow` because of its phase, or `None`.
fn bulk_phase_reason(action: BulkAction, workflow: &ArgoWorkflow) -> Option<String> {
    let phase = workflow.status.phase.as_str();
    if action == BulkAction::Delete && matches!(phase, "Pending" | "Running") {
        return Some("Still running, stop it first".to_string());
    }
    (!bulk_applicable(action, workflow))
        .then(|| format!("Action does not apply to a {phase} workflow"))
}

fn bulk_applicable(action: BulkAction, workflow: &ArgoWorkflow) -> bool {
    let phase = workflow.status.phase.as_str();
    match action {
        BulkAction::Delete => !matches!(phase, "Pending" | "Running"),
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

const CURRENT_WORKFLOW_UNKNOWN: &str = "Current workflow unknown, try again later";

/// Whether a workflow is the current workflow of its job.
enum CurrentWorkflow {
    /// The job's current workflow, or a workflow without a known job.
    Current,
    /// The job has another current workflow (after a resubmit).
    SupersededBy(String),
    /// The job's current workflow cannot be determined, for example while
    /// the statistics database fails, or without it before a poll found the job.
    Unknown,
}

async fn current_workflow(state: &BaktaHandler, workflow: &ArgoWorkflow) -> CurrentWorkflow {
    let Some(id) = workflow
        .metadata
        .labels
        .get("jobid")
        .and_then(|value| value.parse::<Uuid>().ok())
    else {
        return CurrentWorkflow::Current;
    };
    let handler = &state.state_handler;
    let in_memory = handler
        .job_state
        .read()
        .await
        .get(&id)
        .map(|job| job.workflowname.clone());
    let current = match (in_memory, handler.stats.db()) {
        (Some(name), _) => name,
        // Not known before the first poll after a restart finds the job.
        (None, None) => return CurrentWorkflow::Unknown,
        (None, Some(db)) => match db.get_job(id).await {
            Ok(Some(record)) => record.workflow_name,
            Ok(None) => return CurrentWorkflow::Current,
            Err(error) => {
                tracing::warn!(%error, job_id = %id, "admin_current_workflow_lookup_failed");
                return CurrentWorkflow::Unknown;
            }
        },
    };
    match current {
        None => CurrentWorkflow::Unknown,
        Some(name) if name == workflow.metadata.name => CurrentWorkflow::Current,
        Some(name) => CurrentWorkflow::SupersededBy(name),
    }
}

/// True while the S3 data of the workflow's job exists. The reference time
/// is the job record's, or the workflow's finish or creation time when there
/// is no record.
async fn workflow_data_available(state: &BaktaHandler, workflow: &ArgoWorkflow) -> bool {
    let record = match (
        state.state_handler.stats.db(),
        workflow
            .metadata
            .labels
            .get("jobid")
            .and_then(|value| value.parse::<Uuid>().ok()),
    ) {
        (Some(db), Some(id)) => db.get_job(id).await.unwrap_or_else(|error| {
            tracing::warn!(%error, job_id = %id, "admin_retention_lookup_failed");
            None
        }),
        _ => None,
    };
    let reference = match record {
        Some(record) => Some(reference_time(record.finished_at, record.created_at)),
        None => workflow
            .status
            .finished_at
            .or(workflow.metadata.creation_timestamp),
    };
    reference.is_none_or(|reference| {
        is_data_available(
            data_expires_at(reference, state.data_retention_days),
            Utc::now(),
        )
    })
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
            let retried_at = Utc::now();
            argo.retry_workflow(name).await?;
            if let Some(id) = job_id {
                after_retry(state, id, name, retried_at).await;
            }
        }
        BulkAction::Resubmit => {
            let requested_at = Utc::now();
            let new_workflow =
                argo.resubmit_workflow(name)
                    .await
                    .map_err(|error| match job_id {
                        Some(id) => state.state_handler.recover_failed_resubmit(
                            id,
                            name,
                            requested_at,
                            error,
                        ),
                        None => error,
                    })?;
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
    extract::Json(request): extract::Json<BulkRequest>,
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
        let mut reason = bulk_phase_reason(action, &workflow);
        // A real resubmit takes the job's reservation, like a start or
        // delete. It is taken before deciding which workflow is current and
        // kept until the result is recorded, so another resubmit cannot
        // publish a new workflow in between.
        let mut _reservation = None;
        if reason.is_none()
            && !dry_run
            && action == BulkAction::Resubmit
            && let Some(id) = workflow
                .metadata
                .labels
                .get("jobid")
                .and_then(|value| value.parse::<Uuid>().ok())
        {
            match state.state_handler.reserve_start(id) {
                Ok(reservation) => _reservation = Some(reservation),
                Err(_) => reason = Some(JOB_BUSY.to_string()),
            }
        }
        if reason.is_none()
            && action == BulkAction::Resubmit
            && let Some(id) = workflow
                .metadata
                .labels
                .get("jobid")
                .and_then(|value| value.parse::<Uuid>().ok())
            && job_deleted(&state, id).await
        {
            reason = Some(JOB_DELETED.to_string());
        }
        // An earlier start or resubmit may have created a workflow that is
        // not known yet.
        if reason.is_none()
            && matches!(action, BulkAction::Retry | BulkAction::Resubmit)
            && workflow
                .metadata
                .labels
                .get("jobid")
                .and_then(|value| value.parse::<Uuid>().ok())
                .is_some_and(|id| state.state_handler.is_submission_unknown(&id))
        {
            reason = Some(SubmissionUnknown.to_string());
        }
        // Retry and resubmit only act on the job's current workflow, and
        // not when it cannot be determined.
        if reason.is_none() && matches!(action, BulkAction::Retry | BulkAction::Resubmit) {
            match current_workflow(&state, &workflow).await {
                CurrentWorkflow::Current => {}
                CurrentWorkflow::SupersededBy(current) => {
                    reason = Some(format!("Workflow was superseded by {current}"));
                }
                CurrentWorkflow::Unknown => reason = Some(CURRENT_WORKFLOW_UNKNOWN.to_string()),
            }
        }
        if reason.is_none()
            && matches!(action, BulkAction::Retry | BulkAction::Resubmit)
            && !workflow_data_available(&state, &workflow).await
        {
            reason = Some("Input data deleted after the retention period".to_string());
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

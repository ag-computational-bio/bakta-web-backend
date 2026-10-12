use crate::api_structs::ArgoStatus;
use crate::api_structs::FailedJobStatus;
use crate::api_structs::FailedJobStatusEnum;
use crate::api_structs::Job;
use crate::api_structs::ListResponse;
use crate::api_structs::ResultResponse;
use crate::api_structs::StartRequest;
use crate::argo::structs::{
    Content, SimpleStatus, SubmitResult, SubmitResultMetadata, WorkflowNodeStatus,
};
use crate::{
    api_structs::{JobStatus, VersionResponse},
    argo::client::ArgoClient,
    metrics::AppMetrics,
    s3_handler::S3Handler,
    stats,
    stats::{JobVersions, NewJob, StatsEvent, StatsRecorder, SubmittedJob, WorkflowState},
    v2::api_structs::{
        FailedJobStatus as V2FailedJobStatus, FailedJobStatusKind, JobReference,
        JobStatus as V2JobStatusKind, ResultKind, StageLog, StageStatus, V2JobStatus,
        V2ListResponse, V2LogsResponse, V2ResultResponse, V2StartRequest, V2VersionResponse,
        WorkflowKind, WorkflowStartConfig,
    },
    workflow_catalog::{workflow_descriptor, workflow_template_name},
};
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use rand::distr::Alphanumeric;
use rand::distr::SampleString;
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct StateHandler {
    pub job_state: RwLock<HashMap<Uuid, FullJobState>>,
    pub argo_client: Arc<ArgoClient>,
    pub metrics: Arc<AppMetrics>,
    pub bakta_version: String,
    pub baktfold_version: String,
    /// Jobs with a workflow submission in progress.
    pub starting_jobs: std::sync::Mutex<HashSet<Uuid>>,
    /// Jobs whose submission may have created a workflow that could not be
    /// found yet. The polling loop resolves them.
    pub unknown_submissions: std::sync::Mutex<HashMap<Uuid, UnknownSubmission>>,
    /// Deleted and expired jobs with the time they were removed. A poll never
    /// adds them back, even when its workflow list was fetched before the
    /// removal or Argo still lists the workflow for a while. Entries are kept
    /// for `JOB_RETENTION_DAYS`: after that, a poll treats a finished workflow
    /// of the job as expired anyway. Job IDs are random and never reused.
    pub deleted_jobs: std::sync::Mutex<HashMap<Uuid, DateTime<Utc>>>,
    /// Live workflow per job from the last successful poll.
    pub live_workflows: std::sync::RwLock<HashMap<Uuid, LiveWorkflow>>,
    /// Environment name for job records and the `bakta-environment` workflow label.
    pub environment: String,
    pub stats: StatsRecorder,
}

/// Polls without a workflow for the job needed before an unknown submission
/// counts as having created nothing.
const UNKNOWN_SUBMISSION_EMPTY_POLLS: u32 = 3;
/// Time after the mark that must pass before an unknown submission counts as
/// having created nothing. Argo may still be creating the workflow.
const UNKNOWN_SUBMISSION_WAIT_MINUTES: i64 = 10;

/// Allowed clock difference between the backend and Argo when a poll checks
/// whether a workflow was created by a resubmit. Argo creation times are
/// whole seconds.
const RESUBMIT_CLOCK_TOLERANCE_SECONDS: i64 = 30;

/// A submission that may have created a workflow that was not found yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSubmission {
    pub marked_at: DateTime<Utc>,
    /// Polls fetched after `marked_at` that found no workflow for the job.
    pub empty_polls: u32,
    /// Set when the submission was a resubmit. `None` for a start.
    pub resubmit: Option<UnknownResubmit>,
}

/// The workflow that a resubmit with an unknown result resubmitted, and the
/// time of the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownResubmit {
    pub previous_workflow: String,
    pub requested_at: DateTime<Utc>,
}

impl UnknownSubmission {
    pub fn new(marked_at: DateTime<Utc>) -> Self {
        Self {
            marked_at,
            empty_polls: 0,
            resubmit: None,
        }
    }

    /// True when `item`, a workflow of the job, can be the workflow the
    /// submission created. After a start, every workflow of the job can be.
    /// After a resubmit, only a workflow other than the resubmitted one that
    /// was created at or after the request can be.
    fn resolved_by(&self, item: &SimpleStatus) -> bool {
        let Some(resubmit) = &self.resubmit else {
            return true;
        };
        item.metadata.name != resubmit.previous_workflow
            && item.metadata.creation_timestamp.is_some_and(|created| {
                created
                    >= resubmit.requested_at - Duration::seconds(RESUBMIT_CLOCK_TOLERANCE_SECONDS)
            })
    }

    /// Counts a poll from `now` that found no workflow for the job and
    /// returns true while the submission is still unknown. It is resolved
    /// after enough empty polls over enough time. A found workflow resolves
    /// it when the poll applies the workflow to the job state.
    fn still_unknown(&mut self, now: DateTime<Utc>) -> bool {
        if now > self.marked_at {
            self.empty_polls += 1;
        }
        self.empty_polls < UNKNOWN_SUBMISSION_EMPTY_POLLS
            || now - self.marked_at < Duration::minutes(UNKNOWN_SUBMISSION_WAIT_MINUTES)
    }
}

/// Name and phase of a live Argo workflow, as seen by the polling loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWorkflow {
    pub name: String,
    pub phase: String,
}

/// Releases a start reservation when dropped.
pub struct StartReservation<'a> {
    starting_jobs: &'a std::sync::Mutex<HashSet<Uuid>>,
    job_id: Uuid,
}

impl Drop for StartReservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut starting) = self.starting_jobs.lock() {
            starting.remove(&self.job_id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiVersion {
    V1,
    V2,
}

#[derive(Clone)]
pub struct FullJobState {
    pub id: Uuid,
    pub argo_uid: Option<Uuid>,
    pub argo_ressource_version: Option<String>,
    pub name: String,
    pub status: Option<ArgoStatus>,
    pub started: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub workflowname: Option<String>,
    /// The job secret. `None` when it is not known, for example for a
    /// workflow without a `secret` label. A job without a secret denies every
    /// user request.
    pub secret: Option<String>,
    pub archived: bool,
    pub api_version: ApiVersion,
    pub workflow_kind: WorkflowKind,
    pub result_kind: ResultKind,
}

impl From<&FullJobState> for Option<JobStatus> {
    fn from(state: &FullJobState) -> Self {
        Some(JobStatus {
            id: state.id,
            status: state.status.as_ref().map(|s| s.clone().into())?,
            started: state.started?,
            updated: state.updated?,
            name: state.name.clone(),
        })
    }
}

pub struct BaktaHandler {
    pub s3_handler: S3Handler,
    /// Token validation for the admin API. `None` disables the admin API.
    pub admin_auth: Option<Arc<crate::admin::auth::AdminAuth>>,
    pub version: VersionResponse,
    pub version_v2: V2VersionResponse,
    pub state_handler: Arc<StateHandler>,
    /// Days after which the job data in S3 is deleted.
    pub data_retention_days: i64,
}

const JOB_RETENTION_DAYS: i64 = 35;

/// Returned when a start request targets a job that already has a workflow.
#[derive(Debug)]
pub struct JobAlreadyStarted;

impl std::fmt::Display for JobAlreadyStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Job already started")
    }
}

impl std::error::Error for JobAlreadyStarted {}

/// Returned when an earlier submission may have created a workflow that
/// could not be found yet.
#[derive(Debug)]
pub struct SubmissionUnknown;

impl std::fmt::Display for SubmissionUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The result of an earlier start request is not known yet, try again later")
    }
}

impl std::error::Error for SubmissionUnknown {}

/// Returned when a delete cannot list the job's workflows in Argo, so it
/// cannot tell whether a workflow still runs.
#[derive(Debug)]
pub struct WorkflowLookupFailed;

impl std::fmt::Display for WorkflowLookupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Could not look up the workflows of the job, try again later")
    }
}

impl std::error::Error for WorkflowLookupFailed {}

/// True when a failed submit may still have created a workflow: timeouts,
/// errors after the request was sent, gateway errors and unreadable
/// responses. Connection failures and error answers from Argo created nothing.
fn is_indeterminate_submit_error(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<reqwest::Error>() {
        Some(error) => match error.status() {
            Some(status) => matches!(status.as_u16(), 502 | 504),
            None => !(error.is_connect() || error.is_builder()),
        },
        None => true,
    }
}

fn has_workflow(state: &FullJobState) -> bool {
    state.status.is_some() || state.workflowname.is_some()
}

impl ApiVersion {
    pub fn as_label_value(self) -> &'static str {
        match self {
            ApiVersion::V1 => "v1",
            ApiVersion::V2 => "v2",
        }
    }

    fn from_label_value(value: Option<&str>) -> Self {
        match value {
            Some("v2") => ApiVersion::V2,
            _ => ApiVersion::V1,
        }
    }
}

pub(crate) fn workflow_kind_label_value(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Bakta => "bakta",
        WorkflowKind::BaktaProteins => "bakta_proteins",
        WorkflowKind::BaktaBaktfold => "bakta_baktfold",
        WorkflowKind::Baktfold => "baktfold",
    }
}

pub(crate) fn workflow_kind_from_label_value(value: Option<&str>) -> WorkflowKind {
    match value {
        Some("bakta_proteins") => WorkflowKind::BaktaProteins,
        Some("bakta_baktfold") => WorkflowKind::BaktaBaktfold,
        Some("baktfold") => WorkflowKind::Baktfold,
        _ => WorkflowKind::Bakta,
    }
}

fn result_kind_label_value(kind: ResultKind) -> &'static str {
    match kind {
        ResultKind::Bakta => "bakta",
        ResultKind::BaktaProteins => "bakta_proteins",
        ResultKind::Baktfold => "baktfold",
    }
}

fn result_kind_from_label_value(value: Option<&str>) -> ResultKind {
    match value {
        Some("bakta_proteins") => ResultKind::BaktaProteins,
        Some("baktfold") => ResultKind::Baktfold,
        _ => ResultKind::Bakta,
    }
}

/// The job secret from the workflow labels. A missing or empty label gives
/// `None`, so nobody can use the job with a guessed secret.
fn secret_label(labels: &HashMap<String, String>) -> Option<String> {
    labels
        .get("secret")
        .filter(|secret| !secret.is_empty())
        .cloned()
}

fn state_from_simple_status(simple_status: SimpleStatus) -> Option<FullJobState> {
    let labels = &simple_status.metadata.labels;
    let job_id = labels
        .get("jobid")
        .and_then(|value| Uuid::from_str(value).ok())?;

    let status = match ArgoStatus::try_from(simple_status.status.phase) {
        Ok(status) => status,
        Err(e) => {
            tracing::error!(?e, job_id = %job_id, "Failed to parse workflow status");
            return None;
        }
    };

    Some(FullJobState {
        id: job_id,
        argo_uid: Some(simple_status.metadata.uid),
        argo_ressource_version: simple_status.metadata.resource_version,
        status: Some(status),
        started: Some(simple_status.status.started_at),
        updated: Some(simple_status.status.finished_at.unwrap_or(Utc::now())),
        workflowname: Some(simple_status.metadata.name),
        secret: secret_label(labels),
        name: labels
            .get("name")
            .cloned()
            .unwrap_or_else(|| "Unknown name".to_string()),
        archived: labels.contains_key("workflows.argoproj.io/workflow-archiving-status"),
        api_version: ApiVersion::from_label_value(labels.get("api-version").map(String::as_str)),
        workflow_kind: workflow_kind_from_label_value(
            labels.get("workflow-kind").map(String::as_str),
        ),
        result_kind: result_kind_from_label_value(labels.get("result-kind").map(String::as_str)),
    })
}

/// Job state for the new workflow of a resubmit when the job has no job
/// state yet, for example after a restart. The fields come from the workflow
/// labels, which Argo copies from the old workflow. The next poll replaces it.
pub(crate) fn resubmitted_job_state(
    id: Uuid,
    workflow_name: &str,
    labels: &HashMap<String, String>,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> FullJobState {
    FullJobState {
        id,
        argo_uid: None,
        argo_ressource_version: None,
        status: Some(ArgoStatus::Pending),
        started: Some(created_at),
        updated: Some(now),
        workflowname: Some(workflow_name.to_string()),
        secret: secret_label(labels),
        name: labels
            .get("name")
            .cloned()
            .unwrap_or_else(|| "Unknown name".to_string()),
        archived: false,
        api_version: ApiVersion::from_label_value(labels.get("api-version").map(String::as_str)),
        workflow_kind: workflow_kind_from_label_value(
            labels.get("workflow-kind").map(String::as_str),
        ),
        result_kind: result_kind_from_label_value(labels.get("result-kind").map(String::as_str)),
    }
}

impl From<ArgoStatus> for V2JobStatusKind {
    fn from(value: ArgoStatus) -> Self {
        match value {
            ArgoStatus::Pending | ArgoStatus::Init => V2JobStatusKind::Init,
            ArgoStatus::Running => V2JobStatusKind::Running,
            ArgoStatus::Succeeded => V2JobStatusKind::Successful,
            ArgoStatus::Failed | ArgoStatus::Error => V2JobStatusKind::Error,
        }
    }
}

fn current_updated_at(state: &FullJobState) -> Option<DateTime<Utc>> {
    let mut updated = state.updated?;
    if !matches!(
        state.status,
        Some(ArgoStatus::Error) | Some(ArgoStatus::Succeeded) | Some(ArgoStatus::Failed)
    ) {
        updated = Utc::now();
    }
    Some(updated)
}

fn is_terminal_status(status: Option<&ArgoStatus>) -> bool {
    matches!(
        status,
        Some(ArgoStatus::Succeeded | ArgoStatus::Failed | ArgoStatus::Error)
    )
}

fn completion_status_label(status: &ArgoStatus) -> Option<&'static str> {
    match status {
        ArgoStatus::Succeeded => Some("succeeded"),
        ArgoStatus::Failed => Some("failed"),
        ArgoStatus::Error => Some("error"),
        _ => None,
    }
}

fn active_status_label(status: Option<&ArgoStatus>) -> Option<&'static str> {
    match status {
        None => Some("created"),
        Some(ArgoStatus::Pending) => Some("pending"),
        Some(ArgoStatus::Init) => Some("init"),
        Some(ArgoStatus::Running) => Some("running"),
        Some(ArgoStatus::Succeeded | ArgoStatus::Failed | ArgoStatus::Error) => None,
    }
}

fn runtime_from_state(state: &FullJobState) -> Option<std::time::Duration> {
    let started = state.started?;
    let updated = state.updated?;
    (updated - started).to_std().ok()
}

pub(crate) fn state_reference_time(state: &FullJobState) -> Option<DateTime<Utc>> {
    state.updated.or(state.started)
}

fn is_state_expired(state: &FullJobState, now: DateTime<Utc>) -> bool {
    state_reference_time(state)
        .map(|reference_time| reference_time < now - Duration::days(JOB_RETENTION_DAYS))
        .unwrap_or(false)
}

/// Removes expired jobs and returns their IDs and workflow names.
fn purge_expired_states(
    job_state: &mut HashMap<Uuid, FullJobState>,
    now: DateTime<Utc>,
) -> Vec<(Uuid, Option<String>)> {
    let mut expired = Vec::new();
    job_state.retain(|id, state| {
        let keep = !is_state_expired(state, now);
        if !keep {
            expired.push((*id, state.workflowname.clone()));
        }
        keep
    });
    expired
}

fn job_id_label(item: &SimpleStatus) -> Option<Uuid> {
    item.metadata
        .labels
        .get("jobid")
        .and_then(|value| Uuid::from_str(value).ok())
}

fn workflows_by_job(items: &[SimpleStatus]) -> HashMap<Uuid, Vec<&SimpleStatus>> {
    let mut by_job = HashMap::<Uuid, Vec<&SimpleStatus>>::new();
    for item in items {
        if let Some(id) = job_id_label(item) {
            by_job.entry(id).or_default().push(item);
        }
    }
    by_job
}

/// The current workflow of each job in a workflow list. After a resubmit,
/// several workflows carry the same job ID. The workflow recorded in the job
/// state, or else in the database, is the current one. Without a recorded
/// workflow, the newest by creation time is, and then the last by name.
/// `startedAt` is not used, because an Argo retry resets it.
fn current_workflows(
    job_state: &HashMap<Uuid, FullJobState>,
    recorded_in_db: &HashMap<Uuid, String>,
    items: &[SimpleStatus],
) -> HashMap<Uuid, String> {
    workflows_by_job(items)
        .into_iter()
        .filter_map(|(id, workflows)| {
            let in_list = |name: &String| workflows.iter().any(|item| item.metadata.name == *name);
            let recorded = match job_state.get(&id) {
                // A recorded workflow that is not listed (yet) stays current.
                Some(state) => state.workflowname.clone(),
                // The database may lag behind, so only use a listed name.
                None => recorded_in_db
                    .get(&id)
                    .filter(|name| in_list(name))
                    .cloned(),
            };
            let name = match recorded {
                Some(recorded) => in_list(&recorded).then_some(recorded)?,
                None => workflows
                    .iter()
                    .max_by_key(|item| (item.metadata.creation_timestamp, &item.metadata.name))?
                    .metadata
                    .name
                    .clone(),
            };
            Some((id, name))
        })
        .collect()
}

const MAX_ERROR_MESSAGE_LENGTH: usize = 500;

fn stats_new_job(state: &FullJobState, environment: &str, created_at: DateTime<Utc>) -> NewJob {
    NewJob {
        id: state.id,
        environment: environment.to_string(),
        workflow_kind: workflow_kind_label_value(state.workflow_kind).to_string(),
        api_version: state.api_version.as_label_value().to_string(),
        name: Some(state.name.clone()),
        created_at,
    }
}

fn stats_status_label(status: &ArgoStatus) -> &'static str {
    match status {
        ArgoStatus::Pending | ArgoStatus::Init => "submitted",
        ArgoStatus::Running => "running",
        ArgoStatus::Succeeded => "succeeded",
        ArgoStatus::Failed => "failed",
        ArgoStatus::Error => "error",
    }
}

/// Statistics state of a job from its current workflow. Times come from the
/// workflow, so they describe the current attempt (Argo resets `startedAt`
/// and clears `finishedAt` on retry).
fn polled_workflow_state(
    state: &FullJobState,
    environment: &str,
    item: &SimpleStatus,
) -> Option<WorkflowState> {
    let status = stats_status_label(state.status.as_ref()?);
    let started = status != "submitted";
    let terminal = stats::is_terminal_status(status);
    let created_at = item.metadata.creation_timestamp;
    Some(WorkflowState {
        job: stats_new_job(
            state,
            environment,
            created_at.unwrap_or(item.status.started_at),
        ),
        workflow_name: item.metadata.name.clone(),
        status,
        created_at,
        started_at: started.then_some(item.status.started_at),
        finished_at: item.status.finished_at.filter(|_| terminal),
        error_message: item
            .status
            .message
            .clone()
            .filter(|_| terminal && status != "succeeded")
            .map(|mut message| {
                message.truncate(message.floor_char_boundary(MAX_ERROR_MESSAGE_LENGTH));
                message
            }),
    })
}

fn versions_for_kind(kind: WorkflowKind, all: &JobVersions) -> JobVersions {
    let uses_bakta = kind != WorkflowKind::Baktfold;
    let uses_baktfold = matches!(kind, WorkflowKind::BaktaBaktfold | WorkflowKind::Baktfold);
    JobVersions {
        bakta_version: all.bakta_version.clone().filter(|_| uses_bakta),
        bakta_db_version: all.bakta_db_version.clone().filter(|_| uses_bakta),
        baktfold_version: all.baktfold_version.clone().filter(|_| uses_baktfold),
        baktfold_db_version: all.baktfold_db_version.clone().filter(|_| uses_baktfold),
    }
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value.clone().filter(|value| !value.trim().is_empty())
}

/// Input details of a start request that go into the job statistics.
#[derive(Default)]
struct SubmissionDetails {
    genus: Option<String>,
    species: Option<String>,
    strain: Option<String>,
    sequence_count: Option<i32>,
    total_length: Option<i64>,
    config: Option<serde_json::Value>,
}

impl SubmissionDetails {
    fn from_v1(config: &crate::api_structs::JobConfig) -> Self {
        let mut value = serde_json::to_value(config).ok();
        // Only record whether a training file was given, not its value.
        if let Some(object) = value.as_mut().and_then(|value| value.as_object_mut())
            && let Some(prodigal) = object.remove("prodigalTrainingFile")
        {
            object.insert(
                "hasProdigalTrainingFile".to_string(),
                serde_json::Value::Bool(!prodigal.is_null()),
            );
        }
        Self {
            genus: non_empty(&config.genus),
            species: non_empty(&config.species),
            strain: non_empty(&config.strain),
            config: value,
            ..Default::default()
        }
    }

    fn from_v2(request: &V2StartRequest) -> Self {
        let mut details = match &request.workflow {
            WorkflowStartConfig::Bakta(config) | WorkflowStartConfig::BaktaBaktfold(config) => {
                Self {
                    genus: non_empty(&config.genus),
                    species: non_empty(&config.species),
                    strain: non_empty(&config.strain),
                    ..Default::default()
                }
            }
            _ => Self::default(),
        };
        details.config = serde_json::to_value(&request.workflow)
            .ok()
            .and_then(|value| value.get("config").cloned());
        if let Some(input_stats) = request.input_stats {
            details.sequence_count = i32::try_from(input_stats.sequence_count).ok();
            details.total_length = i64::try_from(input_stats.total_length).ok();
        }
        details
    }
}

fn normalize_stage_identifier(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_separator = false;

    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            normalized.push(c.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator && !normalized.is_empty() {
            normalized.push('-');
            previous_was_separator = true;
        }
    }

    normalized.trim_matches('-').to_string()
}

fn stage_matches_candidate(candidate: &str, stage: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }

    let normalized_candidate = normalize_stage_identifier(candidate);
    let normalized_stage = normalize_stage_identifier(stage);

    normalized_candidate == normalized_stage
        || normalized_candidate.ends_with(&format!("-{normalized_stage}"))
}

fn stage_status_from_phase(phase: &str) -> StageStatus {
    match phase {
        "Init" | "Pending" => StageStatus::Pending,
        "Running" => StageStatus::Running,
        "Succeeded" => StageStatus::Succeeded,
        "Failed" => StageStatus::Failed,
        "Error" => StageStatus::Error,
        _ => StageStatus::Unknown,
    }
}

fn node_matches_stage(node: &WorkflowNodeStatus, stage: &str) -> bool {
    stage_matches_candidate(&node.display_name, stage)
        || stage_matches_candidate(&node.template_name, stage)
        || stage_matches_candidate(&node.name, stage)
}

fn stage_index_for_node(node: &WorkflowNodeStatus, stages: &[&str]) -> Option<usize> {
    stages
        .iter()
        .position(|stage| node_matches_stage(node, stage))
}

fn stage_status_from_nodes(
    nodes: &HashMap<String, WorkflowNodeStatus>,
    stage: &str,
) -> Option<StageStatus> {
    let matching = nodes
        .values()
        .filter(|node| node_matches_stage(node, stage))
        .collect::<Vec<_>>();
    // With a retry strategy, the Retry parent holds the outcome of the stage.
    // Its failed attempts must not hide a later successful attempt.
    let has_retry_parent = matching.iter().any(|node| node.node_type == "Retry");
    let mut statuses = matching
        .into_iter()
        .filter(|node| !has_retry_parent || node.node_type == "Retry")
        .map(|node| stage_status_from_phase(&node.phase));

    let mut selected = statuses.next()?;
    for status in statuses {
        selected = match (selected, status) {
            (_, StageStatus::Running) | (StageStatus::Succeeded, StageStatus::Failed) => status,
            (StageStatus::Pending, StageStatus::Failed | StageStatus::Error) => status,
            (StageStatus::Unknown, next) => next,
            (current, StageStatus::Error)
                if !matches!(current, StageStatus::Running | StageStatus::Failed) =>
            {
                StageStatus::Error
            }
            (current, _) => current,
        };
    }

    Some(selected)
}

/// Pod name that Argo builds with `POD_NAMES=v2` (see Argo `workflow/util/pod_name.go`):
/// `<workflow>-<template>-<fnv32a(node name)>`, with the prefix cut to fit the
/// Kubernetes name length.
pub(crate) fn argo_v2_pod_name(workflow_name: &str, node: &WorkflowNodeStatus) -> Option<String> {
    const MAX_PREFIX_LENGTH: usize = 253 - 1 - 10;
    if node.name.is_empty() || node.name == workflow_name {
        return None;
    }
    let mut prefix = if node.name.contains(".inline") || node.template_name.is_empty() {
        workflow_name.to_string()
    } else {
        format!("{workflow_name}-{}", node.template_name)
    };
    if prefix.len() > MAX_PREFIX_LENGTH {
        prefix.truncate(MAX_PREFIX_LENGTH);
    }
    let mut hash: u32 = 0x811c9dc5;
    for byte in node.name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    Some(format!("{prefix}-{hash}"))
}

fn stage_indices_by_pod_name(
    stages: &[&str],
    nodes: Option<&HashMap<String, WorkflowNodeStatus>>,
    workflow_name: Option<&str>,
) -> HashMap<String, usize> {
    let mut pod_stage_indices = HashMap::new();
    let Some(nodes) = nodes else {
        return pod_stage_indices;
    };

    for node in nodes.values() {
        let Some(stage_index) = stage_index_for_node(node, stages) else {
            continue;
        };

        if !node.name.is_empty() {
            pod_stage_indices
                .entry(node.name.clone())
                .or_insert(stage_index);
        }
        if !node.id.is_empty() {
            pod_stage_indices
                .entry(node.id.clone())
                .or_insert(stage_index);
        }
        if let Some(pod_name) = workflow_name.and_then(|name| argo_v2_pod_name(name, node)) {
            pod_stage_indices.entry(pod_name).or_insert(stage_index);
        }
    }

    pod_stage_indices
}

fn append_log_content(target: &mut String, content: &str) {
    target.push_str(content);
    if !content.ends_with('\n') {
        target.push('\n');
    }
}

fn stage_contents_from_log_entries(
    stages: &[&str],
    nodes: Option<&HashMap<String, WorkflowNodeStatus>>,
    workflow_name: Option<&str>,
    log_entries: &[Content],
) -> Vec<String> {
    let mut stage_contents = vec![String::new(); stages.len()];
    if stages.is_empty() {
        return stage_contents;
    }

    let pod_stage_indices = stage_indices_by_pod_name(stages, nodes, workflow_name);
    let mut pod_indices = HashMap::<String, usize>::new();
    let mut grouped_logs = Vec::<String>::new();
    let mut ungrouped_logs = String::new();

    for entry in log_entries {
        if let Some(pod_name) = entry
            .pod_name
            .as_deref()
            .filter(|pod_name| !pod_name.is_empty())
        {
            if let Some(&stage_index) = pod_stage_indices.get(pod_name) {
                append_log_content(&mut stage_contents[stage_index], &entry.content);
                continue;
            }

            let index = *pod_indices.entry(pod_name.to_string()).or_insert_with(|| {
                grouped_logs.push(String::new());
                grouped_logs.len() - 1
            });
            append_log_content(&mut grouped_logs[index], &entry.content);
        } else {
            append_log_content(&mut ungrouped_logs, &entry.content);
        }
    }

    for (index, grouped_log) in grouped_logs.into_iter().enumerate() {
        let stage_index = if index < stages.len() {
            index
        } else {
            stages.len() - 1
        };
        stage_contents[stage_index].push_str(&grouped_log);
    }

    if !ungrouped_logs.is_empty() {
        stage_contents[0].push_str(&ungrouped_logs);
    }

    stage_contents
}

fn fallback_stage_status(
    stage_index: usize,
    last_stage_with_logs: Option<usize>,
    overall_status: Option<&ArgoStatus>,
) -> StageStatus {
    match overall_status {
        Some(ArgoStatus::Running) => match last_stage_with_logs {
            Some(last) if stage_index < last => StageStatus::Succeeded,
            Some(last) if stage_index == last => StageStatus::Running,
            Some(_) => StageStatus::Pending,
            None if stage_index == 0 => StageStatus::Running,
            None => StageStatus::Pending,
        },
        Some(ArgoStatus::Succeeded) => match last_stage_with_logs {
            Some(last) if stage_index <= last => StageStatus::Succeeded,
            Some(_) => StageStatus::Pending,
            None => StageStatus::Succeeded,
        },
        Some(ArgoStatus::Failed) => match last_stage_with_logs {
            Some(last) if stage_index < last => StageStatus::Succeeded,
            Some(last) if stage_index == last => StageStatus::Failed,
            Some(_) => StageStatus::Pending,
            None if stage_index == 0 => StageStatus::Failed,
            None => StageStatus::Pending,
        },
        Some(ArgoStatus::Error) => match last_stage_with_logs {
            Some(last) if stage_index < last => StageStatus::Succeeded,
            Some(last) if stage_index == last => StageStatus::Error,
            Some(_) => StageStatus::Pending,
            None if stage_index == 0 => StageStatus::Error,
            None => StageStatus::Pending,
        },
        Some(ArgoStatus::Pending | ArgoStatus::Init) | None => StageStatus::Pending,
    }
}

fn build_stage_logs(
    stages: &[&str],
    nodes: Option<&HashMap<String, WorkflowNodeStatus>>,
    workflow_name: Option<&str>,
    log_entries: &[Content],
    overall_status: Option<&ArgoStatus>,
) -> Vec<StageLog> {
    let stage_contents = stage_contents_from_log_entries(stages, nodes, workflow_name, log_entries);
    let last_stage_with_logs = stage_contents
        .iter()
        .rposition(|content| !content.is_empty());

    stages
        .iter()
        .enumerate()
        .map(|(index, stage)| StageLog {
            stage: (*stage).to_string(),
            status: nodes
                .and_then(|nodes| stage_status_from_nodes(nodes, stage))
                .unwrap_or_else(|| {
                    fallback_stage_status(index, last_stage_with_logs, overall_status)
                }),
            content: stage_contents[index].trim_end().to_string(),
        })
        .collect()
}

fn into_v2_failed_status(id: Uuid, status: FailedJobStatusEnum) -> V2FailedJobStatus {
    V2FailedJobStatus {
        job_id: id,
        status: match status {
            FailedJobStatusEnum::NotFound => FailedJobStatusKind::NotFound,
            FailedJobStatusEnum::Unauthorized => FailedJobStatusKind::Unauthorized,
        },
    }
}

impl FullJobState {
    /// True when `given` is the job secret. Always false when the job has no
    /// known secret.
    pub fn secret_matches(&self, given: &str) -> bool {
        self.secret
            .as_deref()
            .is_some_and(|secret| !secret.is_empty() && secret == given)
    }

    fn to_v2_job_status(&self) -> Option<V2JobStatus> {
        Some(V2JobStatus {
            job_id: self.id,
            status: self.status.clone().map(Into::into)?,
            workflow_kind: self.workflow_kind,
            result_kind: self.result_kind,
            started: self.started?,
            updated: current_updated_at(self)?,
            name: self.name.clone(),
        })
    }
}

impl BaktaHandler {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        argo_token: String,
        argo_url: String,
        argo_namespace: String,
        s3_access_key: String,
        s3_secret_key: String,
        bucket: String,
        endpoint: String,
        bakta_version: String,
        database_version: String,
        baktfold_version: String,
        baktfold_database_version: String,
        backend_version: String,
        environment: String,
        stats: StatsRecorder,
    ) -> Self {
        let argo_client = Arc::new(ArgoClient::new(argo_token, argo_url, argo_namespace));
        let metrics = Arc::new(AppMetrics::new());
        let s3_handler = S3Handler::new(s3_access_key, s3_secret_key, bucket, endpoint);

        let state_handler = Arc::new(StateHandler {
            job_state: RwLock::new(HashMap::new()),
            argo_client,
            metrics,
            bakta_version: bakta_version.clone(),
            baktfold_version: baktfold_version.clone(),
            starting_jobs: Default::default(),
            unknown_submissions: Default::default(),
            deleted_jobs: Default::default(),
            live_workflows: Default::default(),
            environment,
            stats,
        });

        let state_handler_clone = state_handler.clone();
        state_handler_clone.run().await;

        BaktaHandler {
            s3_handler,
            admin_auth: None,
            version: VersionResponse {
                tool: bakta_version.clone(),
                db: database_version.clone(),
                backend: backend_version.clone(),
            },
            version_v2: V2VersionResponse {
                backend_version,
                bakta_version,
                bakta_db_version: database_version,
                baktfold_version: baktfold_version.clone(),
                baktfold_db_version: baktfold_database_version,
            },
            state_handler,
            data_retention_days: crate::retention::DEFAULT_DATA_RETENTION_DAYS,
        }
    }
}

lazy_static::lazy_static! {
    /// This is an example for using doc comment attributes
    pub static ref REGEX: Regex = Regex::new(r"[^0-9a-zA-Z_.]+").unwrap();
}

impl StateHandler {
    async fn run(self: Arc<Self>) {
        let argo_client = self.argo_client.clone();
        tokio::spawn(async move {
            loop {
                let now = Utc::now();
                let initial = match argo_client.get_workflow_status().await {
                    Ok(status) => status,
                    Err(e) => {
                        self.metrics.record_poll_error();
                        let expired = purge_expired_states(&mut *self.job_state.write().await, now);
                        self.record_expired(expired);
                        tracing::error!(?e, "Failed to query workflow_status");
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        continue;
                    }
                };

                self.apply_workflow_statuses(initial.items, now).await;
                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
    }

    /// Merges the workflow list from Argo into the job state, records
    /// metrics and statistics for status changes and drops expired jobs.
    pub(crate) async fn apply_workflow_statuses(
        &self,
        items: Vec<SimpleStatus>,
        now: DateTime<Utc>,
    ) {
        let recorded_in_db = self.recorded_workflow_names(&items).await;
        let mut write_lock = self.job_state.write().await;
        let mut current = current_workflows(&write_lock, &recorded_in_db, &items);
        // Jobs that are being started, deleted or resubmitted keep their
        // state until that is done. The next poll updates them.
        let reserved = self
            .starting_jobs
            .lock()
            .expect("starting jobs lock poisoned")
            .clone();
        // A found workflow resolves a marked submission when it is applied
        // to the job state below. A reserved job keeps its mark, because the
        // workflow is not applied. An empty list does not prove that none was
        // created, because Argo may still be creating it, so several empty
        // polls are needed. After a resubmit, the job state still names the
        // old workflow, which never resolves the mark. The newest workflow
        // that the resubmit may have created becomes current instead.
        let by_job = workflows_by_job(&items);
        let mut resolved = HashSet::new();
        {
            let mut markers = self
                .unknown_submissions
                .lock()
                .expect("unknown submissions lock poisoned");
            for (id, marker) in markers.iter() {
                let created = by_job.get(id).and_then(|workflows| {
                    workflows
                        .iter()
                        .filter(|item| marker.resolved_by(item))
                        .max_by_key(|item| (item.metadata.creation_timestamp, &item.metadata.name))
                });
                let Some(created) = created else {
                    continue;
                };
                if marker.resubmit.is_some() {
                    current.insert(*id, created.metadata.name.clone());
                }
                if current.get(id) == Some(&created.metadata.name) {
                    resolved.insert(*id);
                }
            }
            markers.retain(|id, marker| {
                reserved.contains(id) || resolved.contains(id) || marker.still_unknown(now)
            });
        }
        let deleted = {
            let mut deleted = self
                .deleted_jobs
                .lock()
                .expect("deleted jobs lock poisoned");
            deleted.retain(|_, removed_at| *removed_at > now - Duration::days(JOB_RETENTION_DAYS));
            deleted.clone()
        };
        let is_current = |item: &SimpleStatus| {
            job_id_label(item)
                .and_then(|id| current.get(&id))
                .is_some_and(|name| *name == item.metadata.name)
        };
        let live = items
            .iter()
            .filter(|item| is_current(item))
            .filter_map(|item| {
                Some((
                    job_id_label(item)?,
                    LiveWorkflow {
                        name: item.metadata.name.clone(),
                        phase: item.status.phase.clone(),
                    },
                ))
            })
            .collect();
        *self
            .live_workflows
            .write()
            .expect("live workflows lock poisoned") = live;

        let mut expired = Vec::new();
        let mut polled = Vec::new();
        for item in items {
            // Superseded workflows of a job (after a resubmit) never change
            // or expire the job.
            // Deleted jobs are never added back.
            if !is_current(&item)
                || job_id_label(&item)
                    .is_some_and(|id| reserved.contains(&id) || deleted.contains_key(&id))
            {
                continue;
            }
            let Some(state) = state_from_simple_status(item.clone()) else {
                continue;
            };
            // The workflow is applied (or the job expires) in this step, so
            // the job state now knows whether a workflow exists.
            let marker = resolved
                .contains(&state.id)
                .then(|| {
                    self.unknown_submissions
                        .lock()
                        .expect("unknown submissions lock poisoned")
                        .remove(&state.id)
                })
                .flatten();
            // Recorded before the poll result, like a resubmit that
            // succeeded at once.
            if let Some(UnknownResubmit {
                previous_workflow,
                requested_at,
            }) = marker.and_then(|marker| marker.resubmit)
            {
                self.stats.record(StatsEvent::Resubmitted {
                    id: state.id,
                    previous_workflow_name: previous_workflow,
                    workflow_name: item.metadata.name.clone(),
                    submitted_at: requested_at,
                });
            }
            if is_state_expired(&state, now) {
                if write_lock.remove(&state.id).is_some() {
                    expired.push((state.id, state.workflowname.clone()));
                }
                continue;
            }
            let environment = item
                .metadata
                .labels
                .get("bakta-environment")
                .map(String::as_str)
                .unwrap_or(&self.environment);
            polled.extend(polled_workflow_state(&state, environment, &item));
            if let Some(previous) = write_lock.get(&state.id)
                && !is_terminal_status(previous.status.as_ref())
                && is_terminal_status(state.status.as_ref())
                && let Some(status) = state.status.as_ref().and_then(completion_status_label)
            {
                self.metrics.record_job_completed(
                    workflow_kind_label_value(state.workflow_kind),
                    status,
                    runtime_from_state(&state),
                );
            }
            let mut state = state;
            // A workflow without a secret label keeps the secret that is
            // already known for the job.
            if state.secret.is_none() {
                state.secret = write_lock
                    .get(&state.id)
                    .and_then(|previous| previous.secret.clone());
            }
            write_lock.insert(state.id, state);
        }
        expired.extend(purge_expired_states(&mut write_lock, now));
        // Recorded while the lock is held, so it is ordered with the events
        // of retries and resubmits.
        if !polled.is_empty() {
            self.stats.record(StatsEvent::Polled(polled));
        }
        drop(write_lock);
        self.record_expired(expired);
    }

    /// Workflow names from the database for jobs with several workflows and
    /// no job state, for example after a restart. Empty when the database is
    /// not configured or fails.
    async fn recorded_workflow_names(&self, items: &[SimpleStatus]) -> HashMap<Uuid, String> {
        let Some(db) = self.stats.db() else {
            return HashMap::new();
        };
        let ids: Vec<Uuid> = {
            let job_state = self.job_state.read().await;
            workflows_by_job(items)
                .into_iter()
                .filter(|(id, workflows)| workflows.len() > 1 && !job_state.contains_key(id))
                .map(|(id, _)| id)
                .collect()
        };
        if ids.is_empty() {
            return HashMap::new();
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), db.workflow_names(&ids)).await
        {
            Ok(Ok(names)) => names,
            Ok(Err(error)) => {
                tracing::warn!(%error, "recorded_workflow_names_failed");
                HashMap::new()
            }
            Err(_) => {
                tracing::warn!("recorded_workflow_names_timed_out");
                HashMap::new()
            }
        }
    }

    pub async fn get_job_states(&self, request_jobs: Vec<Job>) -> ListResponse {
        let read_lock = self.job_state.read().await;
        let mut jobs = vec![];
        let mut failed = vec![];

        for Job { id, secret } in request_jobs {
            if let Some(state) = read_lock.get(&id) {
                if !state.secret_matches(&secret) {
                    failed.push(FailedJobStatus {
                        id,
                        status: FailedJobStatusEnum::Unauthorized,
                    });
                    continue;
                }
                if let Some(mut api_status) = Option::<JobStatus>::from(state) {
                    api_status.updated = current_updated_at(state).unwrap_or(api_status.updated);
                    jobs.push(api_status.clone());
                }
            } else {
                failed.push(FailedJobStatus {
                    id,
                    status: FailedJobStatusEnum::NotFound,
                });
            }
        }

        ListResponse { jobs, failed }
    }

    pub async fn render_metrics(&self) -> Result<String> {
        let mut snapshot = HashMap::<(String, String), i64>::new();
        let read_lock = self.job_state.read().await;

        for state in read_lock.values() {
            let Some(status) = active_status_label(state.status.as_ref()) else {
                continue;
            };

            *snapshot
                .entry((
                    workflow_kind_label_value(state.workflow_kind).to_string(),
                    status.to_string(),
                ))
                .or_default() += 1;
        }

        drop(read_lock);

        self.metrics.update_active_jobs(&snapshot);
        self.metrics.encode().map_err(|e| anyhow!(e.to_string()))
    }

    pub async fn get_job_states_v2(&self, request_jobs: Vec<JobReference>) -> V2ListResponse {
        let read_lock = self.job_state.read().await;
        let mut jobs = vec![];
        let mut failed_jobs = vec![];

        for JobReference { job_id, secret } in request_jobs {
            if let Some(state) = read_lock.get(&job_id) {
                if !state.secret_matches(&secret) {
                    failed_jobs.push(into_v2_failed_status(
                        job_id,
                        FailedJobStatusEnum::Unauthorized,
                    ));
                    continue;
                }

                if let Some(api_status) = state.to_v2_job_status() {
                    jobs.push(api_status);
                }
            } else {
                failed_jobs.push(into_v2_failed_status(job_id, FailedJobStatusEnum::NotFound));
            }
        }

        V2ListResponse { jobs, failed_jobs }
    }

    pub async fn get_logs(&self, (job_id, secret): (Uuid, String)) -> Result<String> {
        let state = {
            let read_lock = self.job_state.read().await;
            if let Some(state) = read_lock.get(&job_id) {
                if !state.secret_matches(&secret) {
                    return Err(anyhow!("Unauthorized"));
                }
                state.clone()
            } else {
                return Err(anyhow!("Job not found"));
            }
        };

        tracing::info!(
            job_id = %job_id,
            api_version = state.api_version.as_label_value(),
            workflow_kind = workflow_kind_label_value(state.workflow_kind),
            workflow_name = state.workflowname.as_deref().unwrap_or(""),
            archived = state.archived,
            "workflow_logs_requested"
        );

        let result = self.argo_client.get_logs(&state).await;

        if let Err(e) = &result {
            tracing::warn!(
                job_id = %job_id,
                api_version = state.api_version.as_label_value(),
                workflow_kind = workflow_kind_label_value(state.workflow_kind),
                workflow_name = state.workflowname.as_deref().unwrap_or(""),
                archived = state.archived,
                error = %e,
                "workflow_logs_request_failed"
            );
        }

        result
    }

    pub async fn get_logs_v2(
        &self,
        JobReference { job_id, secret }: JobReference,
    ) -> Result<V2LogsResponse> {
        let (workflow_kind, workflow_name, overall_status, archived, argo_uid) = {
            let read_lock = self.job_state.read().await;
            let Some(state) = read_lock.get(&job_id) else {
                return Err(anyhow!("Job not found"));
            };

            if !state.secret_matches(&secret) {
                return Err(anyhow!("Unauthorized"));
            }

            (
                state.workflow_kind,
                state.workflowname.clone(),
                state.status.clone(),
                state.archived,
                state.argo_uid,
            )
        };

        self.stage_logs(
            job_id,
            workflow_kind,
            workflow_name,
            overall_status,
            archived,
            argo_uid,
        )
        .await
    }

    /// Structured logs of a workflow, grouped by stage. Does not check the job secret.
    pub(crate) async fn stage_logs(
        &self,
        job_id: Uuid,
        workflow_kind: WorkflowKind,
        workflow_name: Option<String>,
        overall_status: Option<ArgoStatus>,
        archived: bool,
        argo_uid: Option<Uuid>,
    ) -> Result<V2LogsResponse> {
        let descriptor = workflow_descriptor(workflow_kind);

        let Some(workflow_name) = workflow_name else {
            return Ok(V2LogsResponse {
                workflow_kind,
                stages: build_stage_logs(
                    descriptor.stages,
                    None,
                    None,
                    &[],
                    overall_status.as_ref(),
                ),
            });
        };

        tracing::info!(
            job_id = %job_id,
            api_version = ApiVersion::V2.as_label_value(),
            workflow_kind = workflow_kind_label_value(workflow_kind),
            workflow_name = %workflow_name,
            archived,
            "workflow_logs_requested"
        );

        let workflow_details = match self
            .argo_client
            .get_workflow_details(&workflow_name, archived, argo_uid.as_ref())
            .await
        {
            Ok(details) => Some(details),
            Err(e) => {
                tracing::warn!(
                    job_id = %job_id,
                    workflow_kind = workflow_kind_label_value(workflow_kind),
                    workflow_name = %workflow_name,
                    archived,
                    error = %e,
                    "workflow_details_request_failed"
                );
                None
            }
        };

        let workflow_logs = match self
            .argo_client
            .get_workflow_log_entries(&workflow_name, archived, argo_uid.as_ref())
            .await
        {
            Ok(log_entries) => Some(log_entries),
            Err(e) => {
                tracing::warn!(
                    job_id = %job_id,
                    workflow_kind = workflow_kind_label_value(workflow_kind),
                    workflow_name = %workflow_name,
                    archived,
                    error = %e,
                    "workflow_logs_request_failed"
                );
                None
            }
        };

        if workflow_details.is_none() && workflow_logs.is_none() {
            return Err(anyhow!("Failed to retrieve workflow logs"));
        }

        let workflow_logs = workflow_logs.unwrap_or_default();

        Ok(V2LogsResponse {
            workflow_kind,
            stages: build_stage_logs(
                descriptor.stages,
                workflow_details
                    .as_ref()
                    .map(|details| &details.status.nodes),
                Some(&workflow_name),
                workflow_logs.as_slice(),
                overall_status.as_ref(),
            ),
        })
    }

    pub async fn delete_job(&self, (job_id, secret): (Uuid, String)) -> Result<()> {
        // The Argo calls run without the job state lock, so a slow Argo does
        // not block other jobs. The reservation keeps starts and polls of
        // this job out until the delete is done.
        let (state, _reservation) = {
            let read_lock = self.job_state.read().await;
            let Some(state) = read_lock.get(&job_id) else {
                return Ok(());
            };
            if !state.secret_matches(&secret) {
                return Err(anyhow!("Unauthorized"));
            }
            let reservation = self
                .reserve_start(job_id)
                .map_err(|_| anyhow!("Job is starting, try again later"))?;
            // A workflow may exist that the job state does not know yet.
            if self.is_submission_unknown(&job_id) {
                return Err(SubmissionUnknown.into());
            }
            (state.clone(), reservation)
        };
        if state.workflowname.is_some() || state.archived {
            self.argo_client.delete_workflow(&state).await?;
        } else {
            self.delete_job_workflows(job_id).await?;
        }
        let mut job_state = self.job_state.write().await;
        self.record_deleted(job_id);
        if job_state.remove(&job_id).is_some() {
            self.stats.record(StatsEvent::Deleted { id: job_id });
        }
        Ok(())
    }

    pub async fn init_job(&self, name: String) -> (Uuid, String) {
        self.init_job_with_metadata(name, ApiVersion::V1, WorkflowKind::Bakta, ResultKind::Bakta)
            .await
    }

    pub async fn init_job_v2(&self, name: String, workflow_kind: WorkflowKind) -> (Uuid, String) {
        let descriptor = workflow_descriptor(workflow_kind);
        self.init_job_with_metadata(name, ApiVersion::V2, workflow_kind, descriptor.result_kind)
            .await
    }

    async fn init_job_with_metadata(
        &self,
        name: String,
        api_version: ApiVersion,
        workflow_kind: WorkflowKind,
        result_kind: ResultKind,
    ) -> (Uuid, String) {
        let mut result = REGEX.replace_all(&name, "_").to_string();
        result.truncate(63);
        let stripped = result
            .trim_end_matches(|c: char| !c.is_alphanumeric())
            .to_string();

        let job_id = Uuid::new_v4();
        let secret = Alphanumeric.sample_string(&mut rand::rng(), 32);
        let now = Utc::now();
        self.stats.record(StatsEvent::Created(NewJob {
            id: job_id,
            environment: self.environment.clone(),
            workflow_kind: workflow_kind_label_value(workflow_kind).to_string(),
            api_version: api_version.as_label_value().to_string(),
            name: Some(stripped.clone()),
            created_at: now,
        }));
        self.job_state.write().await.insert(
            job_id,
            FullJobState {
                id: job_id,
                argo_uid: None,
                argo_ressource_version: None,
                status: None,
                started: None,
                updated: Some(now),
                workflowname: None,
                secret: Some(secret.clone()),
                name: stripped,
                archived: false,
                api_version,
                workflow_kind,
                result_kind,
            },
        );
        (job_id, secret)
    }

    /// Reserves a job for submission or deletion. The reservation is released
    /// when the returned guard is dropped. Callers must hold the job state lock
    /// while they check `has_workflow`, so a finished submission is always visible.
    pub(crate) fn reserve_start(&self, job_id: Uuid) -> Result<StartReservation<'_>> {
        let mut starting = self
            .starting_jobs
            .lock()
            .expect("starting jobs lock poisoned");
        if !starting.insert(job_id) {
            return Err(JobAlreadyStarted.into());
        }
        Ok(StartReservation {
            starting_jobs: &self.starting_jobs,
            job_id,
        })
    }

    pub(crate) fn is_submission_unknown(&self, job_id: &Uuid) -> bool {
        self.unknown_submissions
            .lock()
            .expect("unknown submissions lock poisoned")
            .contains_key(job_id)
    }

    /// Handles a failed submit. When the submit may still have created a
    /// workflow, looks it up by its `jobid` label and returns it. When the
    /// lookup fails or finds nothing, marks the job, so starts and deletes are
    /// refused until the polling loop knows whether a workflow exists.
    async fn recover_failed_submit(
        &self,
        job_id: Uuid,
        error: anyhow::Error,
    ) -> Result<SubmitResult> {
        if !is_indeterminate_submit_error(&error) {
            return Err(error);
        }
        let selector = format!("jobid={job_id}");
        match self.argo_client.list_workflows(Some(&selector)).await {
            Ok(workflows) => {
                let Some(workflow) = workflows.into_iter().max_by(|a, b| {
                    (a.metadata.creation_timestamp, &a.metadata.name)
                        .cmp(&(b.metadata.creation_timestamp, &b.metadata.name))
                }) else {
                    // Argo may still be creating the workflow.
                    tracing::warn!(job_id = %job_id, error = %error, "workflow_submit_not_found");
                    self.mark_submission_unknown(job_id);
                    return Err(SubmissionUnknown.into());
                };
                tracing::info!(
                    job_id = %job_id,
                    workflow_name = %workflow.metadata.name,
                    "workflow_submit_recovered"
                );
                Ok(SubmitResult {
                    metadata: SubmitResultMetadata {
                        name: workflow.metadata.name,
                        creation_timestamp: workflow
                            .metadata
                            .creation_timestamp
                            .unwrap_or_else(Utc::now),
                    },
                })
            }
            Err(lookup_error) => {
                tracing::warn!(job_id = %job_id, error = %lookup_error, "workflow_submit_lookup_failed");
                self.mark_submission_unknown(job_id);
                Err(SubmissionUnknown.into())
            }
        }
    }

    /// Deletes the live workflows with the job's `jobid` label and returns
    /// true when one was deleted. Deletes use it for jobs without a known
    /// workflow name, because a start may have created a workflow that the
    /// backend does not know, for example after a restart. Fails with
    /// `WorkflowLookupFailed` when Argo cannot list the workflows.
    pub(crate) async fn delete_job_workflows(&self, job_id: Uuid) -> Result<bool> {
        let selector = format!("jobid={job_id}");
        let workflows = self
            .argo_client
            .list_workflows(Some(&selector))
            .await
            .map_err(|error| {
                tracing::warn!(job_id = %job_id, %error, "job_workflow_lookup_failed");
                anyhow::Error::new(WorkflowLookupFailed)
            })?;
        let mut deleted = false;
        for workflow in workflows {
            deleted |= self
                .argo_client
                .delete_workflow_by_name(&workflow.metadata.name)
                .await?;
        }
        Ok(deleted)
    }

    /// Handles a failed resubmit of `previous_workflow`, requested at
    /// `requested_at`, and returns the error. When Argo may still have
    /// created the new workflow, marks the job like an unknown start. Starts,
    /// deletes, retries and resubmits of the job are then refused until a
    /// poll finds a newer workflow of the job or enough polls found none.
    pub(crate) fn recover_failed_resubmit(
        &self,
        job_id: Uuid,
        previous_workflow: &str,
        requested_at: DateTime<Utc>,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if is_indeterminate_submit_error(&error) {
            tracing::warn!(
                job_id = %job_id, workflow_name = %previous_workflow, %error,
                "workflow_resubmit_unknown"
            );
            let mut marker = UnknownSubmission::new(Utc::now());
            marker.resubmit = Some(UnknownResubmit {
                previous_workflow: previous_workflow.to_string(),
                requested_at,
            });
            self.unknown_submissions
                .lock()
                .expect("unknown submissions lock poisoned")
                .insert(job_id, marker);
        }
        error
    }

    fn mark_submission_unknown(&self, job_id: Uuid) {
        self.unknown_submissions
            .lock()
            .expect("unknown submissions lock poisoned")
            .insert(job_id, UnknownSubmission::new(Utc::now()));
    }

    /// Keeps polls from adding the job back. Deletes call it while holding
    /// the job state lock, in the same step that removes the job. Expired
    /// jobs are recorded by the polling loop itself before its next poll.
    pub(crate) fn record_deleted(&self, job_id: Uuid) {
        self.deleted_jobs
            .lock()
            .expect("deleted jobs lock poisoned")
            .insert(job_id, Utc::now());
    }

    /// True when the job was deleted or expired within the last
    /// `JOB_RETENTION_DAYS` while the backend ran.
    pub(crate) fn is_deleted(&self, job_id: &Uuid) -> bool {
        self.deleted_jobs
            .lock()
            .expect("deleted jobs lock poisoned")
            .contains_key(job_id)
    }

    fn record_expired(&self, expired: Vec<(Uuid, Option<String>)>) {
        for (id, workflow_name) in expired {
            self.record_deleted(id);
            self.stats.record(StatsEvent::Expired { id, workflow_name });
        }
    }

    fn record_submitted(
        &self,
        state: &FullJobState,
        details: SubmissionDetails,
        workflow_name: &str,
    ) {
        let now = Utc::now();
        self.stats
            .record(StatsEvent::Submitted(Box::new(SubmittedJob {
                job: stats_new_job(state, &self.environment, now),
                genus: details.genus,
                species: details.species,
                strain: details.strain,
                sequence_count: details.sequence_count,
                total_length: details.total_length,
                config: details.config,
                versions: versions_for_kind(state.workflow_kind, &self.stats.versions),
                workflow_name: workflow_name.to_string(),
                submitted_at: now,
            })));
    }

    fn submit_labels(
        &self,
        state: &FullJobState,
        origin: Option<String>,
    ) -> HashMap<String, String> {
        HashMap::from([
            ("bakta-environment".to_string(), self.environment.clone()),
            ("jobid".to_string(), state.id.to_string()),
            ("name".to_string(), state.name.clone()),
            (
                "secret".to_string(),
                state.secret.clone().unwrap_or_default(),
            ),
            (
                "api-version".to_string(),
                state.api_version.as_label_value().to_string(),
            ),
            (
                "workflow-kind".to_string(),
                workflow_kind_label_value(state.workflow_kind).to_string(),
            ),
            (
                "result-kind".to_string(),
                result_kind_label_value(state.result_kind).to_string(),
            ),
            (
                "origin".to_string(),
                origin.unwrap_or_else(|| "Unknown".to_string()),
            ),
        ])
    }

    pub async fn start_job(
        &self,
        start_settings: StartRequest,
        bakta_version: String,
        origin: Option<String>,
    ) -> Result<()> {
        let Job { id, secret } = &start_settings.job;
        let id = *id;

        let details = SubmissionDetails::from_v1(&start_settings.config);
        let parameters = start_settings.config.into_parameters();
        let template_name = format!("bakta-job-{}", bakta_version);

        let (labels, api_version, workflow_kind, _reservation) = {
            let read_lock = self.job_state.read().await;
            let Some(state) = read_lock.get(&id) else {
                return Ok(());
            };
            if !state.secret_matches(secret) {
                return Err(anyhow!("Unauthorized"));
            }
            if state.workflow_kind != WorkflowKind::Bakta {
                return Err(anyhow!(
                    "Workflow kind {} is not supported by the V1 API, use the V2 API",
                    workflow_kind_label_value(state.workflow_kind)
                ));
            }
            if has_workflow(state) {
                return Err(JobAlreadyStarted.into());
            }
            // Reserve first: a failed start marks its submission unknown
            // before it releases the reservation, so the check sees the mark.
            let reservation = self.reserve_start(id)?;
            if self.is_submission_unknown(&id) {
                return Err(SubmissionUnknown.into());
            }
            (
                self.submit_labels(state, origin),
                state.api_version,
                state.workflow_kind,
                reservation,
            )
        };

        let result = self
            .argo_client
            .submit_from_template(
                template_name.clone(),
                Some(labels),
                Some(HashMap::from([
                    ("parameter".to_string(), parameters),
                    ("jobid".to_string(), id.to_string()),
                ])),
                None,
                Some(format!("bakta-job-{}-", id)),
            )
            .await;

        let result = match result {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(
                    job_id = %id,
                    api_version = api_version.as_label_value(),
                    workflow_kind = workflow_kind_label_value(workflow_kind),
                    template = %template_name,
                    error = %e,
                    "workflow_submit_failed"
                );
                match self.recover_failed_submit(id, e).await {
                    Ok(found) => found,
                    Err(e) => {
                        self.metrics
                            .record_submit_error(workflow_kind_label_value(workflow_kind));
                        return Err(e);
                    }
                }
            }
        };

        let mut write_lock = self.job_state.write().await;
        let Some(state) = write_lock.get_mut(&id) else {
            return Ok(());
        };
        let workflow_name = result.metadata.name.clone();
        state.workflowname = Some(result.metadata.name);
        state.status = Some(ArgoStatus::Pending);
        state.started = Some(result.metadata.creation_timestamp);
        state.updated = Some(Utc::now());
        state.api_version = ApiVersion::V1;
        state.workflow_kind = WorkflowKind::Bakta;
        state.result_kind = ResultKind::Bakta;
        self.metrics.record_job_submitted(
            state.api_version.as_label_value(),
            workflow_kind_label_value(state.workflow_kind),
        );
        self.record_submitted(state, details, &workflow_name);
        tracing::info!(
            job_id = %id,
            api_version = state.api_version.as_label_value(),
            workflow_kind = workflow_kind_label_value(state.workflow_kind),
            template = %template_name,
            workflow_name = %workflow_name,
            "workflow_submitted"
        );
        Ok(())
    }

    pub async fn start_job_v2(
        &self,
        start_settings: V2StartRequest,
        origin: Option<String>,
    ) -> Result<()> {
        let workflow_kind = start_settings.workflow.workflow_kind();
        let details = SubmissionDetails::from_v2(&start_settings);
        let parameters = start_settings.workflow.into_parameters()?;
        let JobReference { job_id, secret } = start_settings.job;
        let descriptor = workflow_descriptor(workflow_kind);
        let template_name =
            workflow_template_name(workflow_kind, &self.bakta_version, &self.baktfold_version)?;

        let (labels, api_version, _reservation) = {
            let read_lock = self.job_state.read().await;
            let Some(state) = read_lock.get(&job_id) else {
                return Err(anyhow!("Job not found"));
            };

            if !state.secret_matches(&secret) {
                return Err(anyhow!("Unauthorized"));
            }

            if state.api_version != ApiVersion::V2 {
                return Err(anyhow!("Wrong API version"));
            }

            if state.workflow_kind != workflow_kind {
                return Err(anyhow!("Workflow kind mismatch"));
            }

            if has_workflow(state) {
                return Err(JobAlreadyStarted.into());
            }

            // Reserve first: a failed start marks its submission unknown
            // before it releases the reservation, so the check sees the mark.
            let reservation = self.reserve_start(job_id)?;
            if self.is_submission_unknown(&job_id) {
                return Err(SubmissionUnknown.into());
            }
            (
                self.submit_labels(state, origin),
                state.api_version,
                reservation,
            )
        };

        let result = self
            .argo_client
            .submit_from_template(
                template_name.clone(),
                Some(labels),
                Some(HashMap::from([
                    ("parameter".to_string(), parameters),
                    ("jobid".to_string(), job_id.to_string()),
                ])),
                None,
                Some(format!("{}-{}-", template_name, job_id)),
            )
            .await;

        let result = match result {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!(
                    job_id = %job_id,
                    api_version = api_version.as_label_value(),
                    workflow_kind = workflow_kind_label_value(workflow_kind),
                    template = %template_name,
                    error = %e,
                    "workflow_submit_failed"
                );
                match self.recover_failed_submit(job_id, e).await {
                    Ok(found) => found,
                    Err(e) => {
                        self.metrics
                            .record_submit_error(workflow_kind_label_value(workflow_kind));
                        return Err(e);
                    }
                }
            }
        };

        let mut write_lock = self.job_state.write().await;
        let Some(state) = write_lock.get_mut(&job_id) else {
            return Ok(());
        };
        let workflow_name = result.metadata.name.clone();
        state.workflowname = Some(result.metadata.name);
        state.status = Some(ArgoStatus::Pending);
        state.started = Some(result.metadata.creation_timestamp);
        state.updated = Some(Utc::now());
        state.result_kind = descriptor.result_kind;
        self.metrics.record_job_submitted(
            state.api_version.as_label_value(),
            workflow_kind_label_value(state.workflow_kind),
        );
        self.record_submitted(state, details, &workflow_name);
        tracing::info!(
            job_id = %job_id,
            api_version = state.api_version.as_label_value(),
            workflow_kind = workflow_kind_label_value(state.workflow_kind),
            template = %template_name,
            workflow_name = %workflow_name,
            "workflow_submitted"
        );

        Ok(())
    }

    pub async fn get_results_v2(
        &self,
        JobReference { job_id, secret }: JobReference,
        s3_handler: &S3Handler,
    ) -> Result<V2ResultResponse> {
        let (workflow_kind, result_kind, started, updated, name) = {
            let read_lock = self.job_state.read().await;
            let Some(state) = read_lock.get(&job_id) else {
                return Err(anyhow!("Job not found"));
            };

            if !state.secret_matches(&secret) {
                return Err(anyhow!("Unauthorized"));
            }

            if state.status != Some(ArgoStatus::Succeeded) {
                return Err(anyhow!("Job not finished"));
            }

            (
                state.workflow_kind,
                state.result_kind,
                state.started.unwrap_or_default(),
                state.updated.unwrap_or_default(),
                state.name.clone(),
            )
        };

        let job_id_string = job_id.to_string();

        Ok(V2ResultResponse {
            job_id,
            workflow_kind,
            started,
            updated,
            name: name.clone(),
            result: s3_handler
                .sign_download_urls_v2(job_id_string.as_str(), &name, result_kind)
                .await?,
        })
    }

    pub async fn get_results(
        &self,
        Job { id, secret }: Job,
        s3_handler: &S3Handler,
    ) -> Result<ResultResponse> {
        if let Some(state) = self.job_state.read().await.get(&id) {
            if !state.secret_matches(&secret) {
                return Err(anyhow!("Unauthorized"));
            }

            if state.status != Some(ArgoStatus::Succeeded) {
                return Err(anyhow!("Job not finished"));
            }

            return Ok(ResultResponse {
                id,
                started: state.started.unwrap_or_default(),
                updated: state.updated.unwrap_or_default(),
                name: state.name.clone(),
                files: s3_handler
                    .sign_download_urls(id.to_string().as_str(), &state.name)
                    .await?,
            });
        }
        Err(anyhow!("Job not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_state(
        id: Uuid,
        status: Option<ArgoStatus>,
        updated: DateTime<Utc>,
        workflow_kind: WorkflowKind,
    ) -> FullJobState {
        FullJobState {
            id,
            argo_uid: None,
            argo_ressource_version: None,
            name: id.to_string(),
            status,
            started: Some(updated - Duration::minutes(5)),
            updated: Some(updated),
            workflowname: None,
            secret: Some("secret".to_string()),
            archived: false,
            api_version: ApiVersion::V2,
            workflow_kind,
            result_kind: ResultKind::Bakta,
        }
    }

    #[test]
    fn test_state_from_simple_status_ignores_workflow_without_job_label() {
        let status = SimpleStatus::default();
        assert!(state_from_simple_status(status).is_none());
    }

    #[test]
    fn test_state_from_simple_status_defaults_to_v1_metadata_without_v2_labels() {
        let mut status = SimpleStatus::default();
        status.metadata.name = "workflow-name".to_string();
        status
            .metadata
            .labels
            .insert("jobid".to_string(), Uuid::nil().to_string());
        status.status.phase = "Pending".to_string();
        status.status.started_at = Utc::now();

        let state = state_from_simple_status(status).expect("expected state to parse");

        assert_eq!(state.api_version, ApiVersion::V1);
        assert_eq!(state.workflow_kind, WorkflowKind::Bakta);
        assert_eq!(state.result_kind, ResultKind::Bakta);
    }

    #[test]
    fn test_build_stage_logs_uses_node_status_and_log_order() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "node-1".to_string(),
            WorkflowNodeStatus {
                display_name: "bakta".to_string(),
                phase: "Succeeded".to_string(),
                ..Default::default()
            },
        );
        nodes.insert(
            "node-2".to_string(),
            WorkflowNodeStatus {
                display_name: "baktfold".to_string(),
                phase: "Running".to_string(),
                ..Default::default()
            },
        );

        let logs = vec![
            Content {
                content: "bakta line".to_string(),
                pod_name: Some("wf-bakta-111".to_string()),
            },
            Content {
                content: "baktfold line".to_string(),
                pod_name: Some("wf-baktfold-222".to_string()),
            },
        ];

        let stages = build_stage_logs(
            &["bakta", "baktfold"],
            Some(&nodes),
            None,
            &logs,
            Some(&ArgoStatus::Running),
        );

        assert_eq!(stages[0].status, StageStatus::Succeeded);
        assert_eq!(stages[0].content, "bakta line");
        assert_eq!(stages[1].status, StageStatus::Running);
        assert_eq!(stages[1].content, "baktfold line");
    }

    #[test]
    fn test_build_stage_logs_falls_back_when_workflow_details_are_missing() {
        let logs = vec![
            Content {
                content: "initial ungrouped line".to_string(),
                pod_name: None,
            },
            Content {
                content: "bakta line".to_string(),
                pod_name: Some("wf-bakta-111".to_string()),
            },
            Content {
                content: "baktfold line".to_string(),
                pod_name: Some("wf-baktfold-222".to_string()),
            },
            Content {
                content: "extra retry line".to_string(),
                pod_name: Some("wf-baktfold-retry-333".to_string()),
            },
        ];

        let stages = build_stage_logs(
            &["bakta", "baktfold"],
            None,
            None,
            &logs,
            Some(&ArgoStatus::Failed),
        );

        assert_eq!(stages[0].status, StageStatus::Succeeded);
        assert_eq!(stages[0].content, "bakta line\ninitial ungrouped line");
        assert_eq!(stages[1].status, StageStatus::Failed);
        assert_eq!(stages[1].content, "baktfold line\nextra retry line");
    }

    #[test]
    fn test_build_stage_logs_uses_template_name_when_display_name_does_not_match() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "node-1".to_string(),
            WorkflowNodeStatus {
                display_name: "run-annotation".to_string(),
                template_name: "bakta_proteins".to_string(),
                phase: "Succeeded".to_string(),
                ..Default::default()
            },
        );

        let stages = build_stage_logs(
            &["bakta_proteins"],
            Some(&nodes),
            None,
            &[],
            Some(&ArgoStatus::Succeeded),
        );

        assert_eq!(stages[0].status, StageStatus::Succeeded);
    }

    #[test]
    fn test_build_stage_logs_maps_pod_ids_to_stages() {
        let workflow_name = "bakta-baktfold-job-1.12.0-0.1.0-ab37efdd-46ae-4564-943e-3bpp7nj";
        let baktfold_pod = format!("{workflow_name}-1730121812");
        let bakta_pod = format!("{workflow_name}-2082099159");
        let mut nodes = HashMap::new();
        nodes.insert(
            "node-1".to_string(),
            WorkflowNodeStatus {
                id: baktfold_pod.clone(),
                display_name: "baktfold".to_string(),
                phase: "Succeeded".to_string(),
                node_type: "Pod".to_string(),
                ..Default::default()
            },
        );
        nodes.insert(
            "node-2".to_string(),
            WorkflowNodeStatus {
                id: bakta_pod.clone(),
                display_name: "bakta".to_string(),
                phase: "Succeeded".to_string(),
                node_type: "Pod".to_string(),
                ..Default::default()
            },
        );

        let logs = vec![
            Content {
                content: "baktfold line".to_string(),
                pod_name: Some(baktfold_pod),
            },
            Content {
                content: "bakta line".to_string(),
                pod_name: Some(bakta_pod),
            },
        ];

        let stages = build_stage_logs(
            &["bakta", "baktfold"],
            Some(&nodes),
            Some(workflow_name),
            &logs,
            Some(&ArgoStatus::Succeeded),
        );

        assert_eq!(stages[0].content, "bakta line");
        assert_eq!(stages[1].content, "baktfold line");
    }

    #[test]
    fn test_build_stage_logs_maps_v2_pod_names_to_stages() {
        let workflow_name = "bakta-job-abc";
        let mut nodes = HashMap::new();
        for (key, node_name, stage, template) in [
            ("n1", "bakta-job-abc[0].bakta", "bakta", "run-bakta"),
            (
                "n2",
                "bakta-job-abc[1].baktfold",
                "baktfold",
                "run-baktfold",
            ),
        ] {
            nodes.insert(
                key.to_string(),
                WorkflowNodeStatus {
                    id: format!("{workflow_name}-{key}"),
                    name: node_name.to_string(),
                    display_name: stage.to_string(),
                    template_name: template.to_string(),
                    phase: "Succeeded".to_string(),
                    node_type: "Pod".to_string(),
                    ..Default::default()
                },
            );
        }

        // Pod names as built by Argo with POD_NAMES=v2: <workflow>-<template>-<fnv32a(node name)>.
        let logs = vec![
            Content {
                content: "baktfold line".to_string(),
                pod_name: Some("bakta-job-abc-run-baktfold-4284446571".to_string()),
            },
            Content {
                content: "bakta line".to_string(),
                pod_name: Some("bakta-job-abc-run-bakta-4251716466".to_string()),
            },
        ];

        let stages = build_stage_logs(
            &["bakta", "baktfold"],
            Some(&nodes),
            Some(workflow_name),
            &logs,
            Some(&ArgoStatus::Succeeded),
        );

        assert_eq!(stages[0].content, "bakta line");
        assert_eq!(stages[1].content, "baktfold line");
    }

    #[test]
    fn test_stage_status_uses_retry_parent_when_a_retry_succeeded() {
        let mut nodes = HashMap::new();
        for (key, name, node_type, phase) in [
            ("retry", "wf[0].bakta", "Retry", "Succeeded"),
            ("first", "wf[0].bakta(0)", "Pod", "Failed"),
            ("second", "wf[0].bakta(1)", "Pod", "Succeeded"),
        ] {
            nodes.insert(
                key.to_string(),
                WorkflowNodeStatus {
                    id: key.to_string(),
                    name: name.to_string(),
                    display_name: name.trim_start_matches("wf[0].").to_string(),
                    template_name: "bakta".to_string(),
                    node_type: node_type.to_string(),
                    phase: phase.to_string(),
                    ..Default::default()
                },
            );
        }

        assert_eq!(
            stage_status_from_nodes(&nodes, "bakta"),
            Some(StageStatus::Succeeded)
        );
    }

    #[test]
    fn test_unknown_submission_needs_enough_empty_polls_over_enough_time() {
        let marked_at = Utc::now();
        let mut marker = UnknownSubmission::new(marked_at);
        // A list fetched before the mark does not count.
        assert!(marker.still_unknown(marked_at - Duration::seconds(1)));
        assert!(marker.still_unknown(marked_at + Duration::minutes(20)));
        assert!(marker.still_unknown(marked_at + Duration::minutes(21)));
        assert!(!marker.still_unknown(marked_at + Duration::minutes(22)));

        let mut marker = UnknownSubmission::new(marked_at);
        for minutes in [1, 2, 3, 4, 5] {
            assert!(marker.still_unknown(marked_at + Duration::minutes(minutes)));
        }
        assert!(!marker.still_unknown(marked_at + Duration::minutes(10)));
    }

    #[test]
    fn test_is_state_expired_after_retention_window() {
        let now = Utc::now();
        let state = FullJobState {
            id: Uuid::nil(),
            argo_uid: None,
            argo_ressource_version: None,
            name: "expired".to_string(),
            status: Some(ArgoStatus::Succeeded),
            started: Some(now - Duration::days(JOB_RETENTION_DAYS + 2)),
            updated: Some(now - Duration::days(JOB_RETENTION_DAYS + 1)),
            workflowname: None,
            secret: Some("secret".to_string()),
            archived: false,
            api_version: ApiVersion::V1,
            workflow_kind: WorkflowKind::Bakta,
            result_kind: ResultKind::Bakta,
        };

        assert!(is_state_expired(&state, now));
    }

    #[test]
    fn test_purge_expired_states_removes_old_jobs() {
        let now = Utc::now();
        let expired_id = Uuid::new_v4();
        let fresh_id = Uuid::new_v4();
        let mut job_state = HashMap::from([
            (
                expired_id,
                FullJobState {
                    id: expired_id,
                    argo_uid: None,
                    argo_ressource_version: None,
                    name: "expired".to_string(),
                    status: Some(ArgoStatus::Succeeded),
                    started: Some(now - Duration::days(JOB_RETENTION_DAYS + 2)),
                    updated: Some(now - Duration::days(JOB_RETENTION_DAYS + 1)),
                    workflowname: None,
                    secret: Some("secret".to_string()),
                    archived: false,
                    api_version: ApiVersion::V1,
                    workflow_kind: WorkflowKind::Bakta,
                    result_kind: ResultKind::Bakta,
                },
            ),
            (
                fresh_id,
                FullJobState {
                    id: fresh_id,
                    argo_uid: None,
                    argo_ressource_version: None,
                    name: "fresh".to_string(),
                    status: Some(ArgoStatus::Running),
                    started: Some(now - Duration::days(1)),
                    updated: Some(now),
                    workflowname: None,
                    secret: Some("secret".to_string()),
                    archived: false,
                    api_version: ApiVersion::V2,
                    workflow_kind: WorkflowKind::BaktaBaktfold,
                    result_kind: ResultKind::Bakta,
                },
            ),
        ]);

        let expired = purge_expired_states(&mut job_state, now);

        assert_eq!(expired, vec![(expired_id, None)]);

        assert!(!job_state.contains_key(&expired_id));
        assert!(job_state.contains_key(&fresh_id));
    }

    #[tokio::test]
    async fn test_render_metrics_counts_only_active_jobs() {
        let now = Utc::now();
        let created_id = Uuid::new_v4();
        let running_id = Uuid::new_v4();
        let succeeded_id = Uuid::new_v4();
        let state_handler = StateHandler {
            job_state: RwLock::new(HashMap::from([
                (
                    created_id,
                    build_test_state(created_id, None, now, WorkflowKind::Bakta),
                ),
                (
                    running_id,
                    build_test_state(
                        running_id,
                        Some(ArgoStatus::Running),
                        now,
                        WorkflowKind::BaktaBaktfold,
                    ),
                ),
                (
                    succeeded_id,
                    build_test_state(
                        succeeded_id,
                        Some(ArgoStatus::Succeeded),
                        now,
                        WorkflowKind::Baktfold,
                    ),
                ),
            ])),
            argo_client: Arc::new(ArgoClient::new(
                "token".to_string(),
                "http://argo.example".to_string(),
                "argo".to_string(),
            )),
            metrics: Arc::new(AppMetrics::new()),
            bakta_version: "1.0.0".to_string(),
            baktfold_version: "1.0.0".to_string(),
            starting_jobs: Default::default(),
            unknown_submissions: Default::default(),
            deleted_jobs: Default::default(),
            live_workflows: Default::default(),
            environment: "production".to_string(),
            stats: StatsRecorder::disabled(),
        };

        let rendered = state_handler
            .render_metrics()
            .await
            .expect("metrics should render");

        assert!(rendered.contains("jobs_active{workflow_kind=\"bakta\",status=\"created\"} 1"));
        assert!(
            rendered.contains("jobs_active{workflow_kind=\"bakta_baktfold\",status=\"running\"} 1")
        );
        assert!(!rendered.contains("workflow_kind=\"baktfold\",status=\"succeeded\""));
    }
}

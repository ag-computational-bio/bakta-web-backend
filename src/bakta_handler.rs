use crate::api_structs::ArgoStatus;
use crate::api_structs::FailedJobStatus;
use crate::api_structs::FailedJobStatusEnum;
use crate::api_structs::Job;
use crate::api_structs::ListResponse;
use crate::api_structs::ResultResponse;
use crate::api_structs::StartRequest;
use crate::argo::structs::{Content, SimpleStatus, WorkflowNodeStatus};
use crate::{
    api_structs::{JobStatus, VersionResponse},
    argo::client::ArgoClient,
    metrics::AppMetrics,
    s3_handler::S3Handler,
    v2::api_structs::{
        FailedJobStatus as V2FailedJobStatus, FailedJobStatusKind, JobReference,
        JobStatus as V2JobStatusKind, ResultKind, StageLog, StageStatus, V2JobStatus,
        V2ListResponse, V2LogsResponse, V2ResultResponse, V2StartRequest, V2VersionResponse,
        WorkflowKind,
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
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct StateHandler {
    pub job_state: RwLock<HashMap<Uuid, FullJobState>>,
    pub argo_client: Arc<ArgoClient>,
    pub metrics: Arc<AppMetrics>,
    pub bakta_version: String,
    pub baktfold_version: String,
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
    pub secret: String,
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
    pub version: VersionResponse,
    pub version_v2: V2VersionResponse,
    pub state_handler: Arc<StateHandler>,
}

const JOB_RETENTION_DAYS: i64 = 35;

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

fn workflow_kind_label_value(kind: WorkflowKind) -> &'static str {
    match kind {
        WorkflowKind::Bakta => "bakta",
        WorkflowKind::BaktaProteins => "bakta_proteins",
        WorkflowKind::BaktaBaktfold => "bakta_baktfold",
        WorkflowKind::Baktfold => "baktfold",
    }
}

fn workflow_kind_from_label_value(value: Option<&str>) -> WorkflowKind {
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
        secret: labels
            .get("secret")
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string()),
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

fn state_reference_time(state: &FullJobState) -> Option<DateTime<Utc>> {
    state.updated.or(state.started)
}

fn is_state_expired(state: &FullJobState, now: DateTime<Utc>) -> bool {
    state_reference_time(state)
        .map(|reference_time| reference_time < now - Duration::days(JOB_RETENTION_DAYS))
        .unwrap_or(false)
}

fn purge_expired_states(job_state: &mut HashMap<Uuid, FullJobState>, now: DateTime<Utc>) {
    job_state.retain(|_, state| !is_state_expired(state, now));
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

fn stage_status_from_nodes(
    nodes: &HashMap<String, WorkflowNodeStatus>,
    stage: &str,
) -> Option<StageStatus> {
    let mut statuses = nodes
        .values()
        .filter(|node| {
            stage_matches_candidate(&node.display_name, stage)
                || stage_matches_candidate(&node.template_name, stage)
                || stage_matches_candidate(&node.name, stage)
        })
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

fn append_log_content(target: &mut String, content: &str) {
    target.push_str(content);
    if !content.ends_with('\n') {
        target.push('\n');
    }
}

fn stage_contents_from_log_entries(stage_count: usize, log_entries: &[Content]) -> Vec<String> {
    let mut stage_contents = vec![String::new(); stage_count];
    if stage_count == 0 {
        return stage_contents;
    }

    let mut pod_indices = HashMap::<String, usize>::new();
    let mut grouped_logs = Vec::<String>::new();
    let mut ungrouped_logs = String::new();

    for entry in log_entries {
        if let Some(pod_name) = entry
            .pod_name
            .as_deref()
            .filter(|pod_name| !pod_name.is_empty())
        {
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
        let stage_index = if index < stage_count {
            index
        } else {
            stage_count - 1
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
    log_entries: &[Content],
    overall_status: Option<&ArgoStatus>,
) -> Vec<StageLog> {
    let stage_contents = stage_contents_from_log_entries(stages.len(), log_entries);
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
        });

        let state_handler_clone = state_handler.clone();
        state_handler_clone.run().await;

        BaktaHandler {
            s3_handler,
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
                        purge_expired_states(&mut *self.job_state.write().await, now);
                        tracing::error!(?e, "Failed to query workflow_status");
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        continue;
                    }
                };

                let mut write_lock = self.job_state.write().await;
                for item in initial.items {
                    if let Some(state) = state_from_simple_status(item) {
                        if is_state_expired(&state, now) {
                            write_lock.remove(&state.id);
                            continue;
                        }
                        if let Some(previous) = write_lock.get(&state.id)
                            && !is_terminal_status(previous.status.as_ref())
                            && is_terminal_status(state.status.as_ref())
                            && let Some(status) =
                                state.status.as_ref().and_then(completion_status_label)
                        {
                            self.metrics.record_job_completed(
                                workflow_kind_label_value(state.workflow_kind),
                                status,
                                runtime_from_state(&state),
                            );
                        }
                        write_lock.insert(state.id, state);
                    }
                }
                purge_expired_states(&mut write_lock, now);
                drop(write_lock);
                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
    }

    pub async fn get_job_states(&self, request_jobs: Vec<Job>) -> ListResponse {
        let read_lock = self.job_state.read().await;
        let mut jobs = vec![];
        let mut failed = vec![];

        for Job { id, secret } in request_jobs {
            if let Some(state) = read_lock.get(&id) {
                if state.secret != secret {
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
                if state.secret != secret {
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
                if state.secret != secret {
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

            if state.secret != secret {
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

        let descriptor = workflow_descriptor(workflow_kind);

        let Some(workflow_name) = workflow_name else {
            return Ok(V2LogsResponse {
                workflow_kind,
                stages: build_stage_logs(descriptor.stages, None, &[], overall_status.as_ref()),
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

        let workflow_details = if archived {
            None
        } else {
            match self.argo_client.get_workflow_details(&workflow_name).await {
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
                workflow_logs.as_slice(),
                overall_status.as_ref(),
            ),
        })
    }

    pub async fn delete_job(&self, (job_id, secret): (Uuid, String)) -> Result<()> {
        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get(&job_id) {
            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }
            self.argo_client.delete_workflow(state).await?;
        }
        write_lock.remove(&job_id);
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
        self.job_state.write().await.insert(
            job_id,
            FullJobState {
                id: job_id,
                argo_uid: None,
                argo_ressource_version: None,
                status: None,
                started: None,
                updated: Some(Utc::now()),
                workflowname: None,
                secret: secret.clone(),
                name: stripped,
                archived: false,
                api_version,
                workflow_kind,
                result_kind,
            },
        );
        (job_id, secret)
    }

    pub async fn start_job(
        &self,
        start_settings: StartRequest,
        bakta_version: String,
        origin: Option<String>,
    ) -> Result<()> {
        let Job { id, secret } = &start_settings.job;

        let parameters = start_settings.config.into_parameters();
        let template_name = format!("bakta-job-{}", bakta_version);

        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get_mut(id) {
            if state.secret != *secret {
                return Err(anyhow!("Unauthorized"));
            }

            let result = self
                .argo_client
                .submit_from_template(
                    template_name.clone(),
                    Some(HashMap::from([
                        ("jobid".to_string(), id.to_string()),
                        ("name".to_string(), state.name.clone()),
                        ("secret".to_string(), state.secret.clone()),
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
                    ])),
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
                        api_version = state.api_version.as_label_value(),
                        workflow_kind = workflow_kind_label_value(state.workflow_kind),
                        template = %template_name,
                        error = %e,
                        "workflow_submit_failed"
                    );
                    self.metrics
                        .record_submit_error(workflow_kind_label_value(state.workflow_kind));
                    return Err(e);
                }
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
            tracing::info!(
                job_id = %id,
                api_version = state.api_version.as_label_value(),
                workflow_kind = workflow_kind_label_value(state.workflow_kind),
                template = %template_name,
                workflow_name = %workflow_name,
                "workflow_submitted"
            );
        }
        Ok(())
    }

    pub async fn start_job_v2(
        &self,
        start_settings: V2StartRequest,
        origin: Option<String>,
    ) -> Result<()> {
        let workflow_kind = start_settings.workflow.workflow_kind();
        let parameters = start_settings.workflow.into_parameters()?;
        let JobReference { job_id, secret } = start_settings.job;
        let descriptor = workflow_descriptor(workflow_kind);
        let template_name =
            workflow_template_name(workflow_kind, &self.bakta_version, &self.baktfold_version)?;

        let mut write_lock = self.job_state.write().await;
        let Some(state) = write_lock.get_mut(&job_id) else {
            return Err(anyhow!("Job not found"));
        };

        if state.secret != secret {
            return Err(anyhow!("Unauthorized"));
        }

        if state.api_version != ApiVersion::V2 {
            return Err(anyhow!("Wrong API version"));
        }

        if state.workflow_kind != workflow_kind {
            return Err(anyhow!("Workflow kind mismatch"));
        }

        let result = self
            .argo_client
            .submit_from_template(
                template_name.clone(),
                Some(HashMap::from([
                    ("jobid".to_string(), job_id.to_string()),
                    ("name".to_string(), state.name.clone()),
                    ("secret".to_string(), state.secret.clone()),
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
                ])),
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
                    api_version = state.api_version.as_label_value(),
                    workflow_kind = workflow_kind_label_value(state.workflow_kind),
                    template = %template_name,
                    error = %e,
                    "workflow_submit_failed"
                );
                self.metrics
                    .record_submit_error(workflow_kind_label_value(state.workflow_kind));
                return Err(e);
            }
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

            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }

            if let Some(status) = &state.status
                && status != &ArgoStatus::Succeeded
            {
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
            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }

            if let Some(status) = &state.status
                && status != &ArgoStatus::Succeeded
            {
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
            secret: "secret".to_string(),
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
            &[],
            Some(&ArgoStatus::Succeeded),
        );

        assert_eq!(stages[0].status, StageStatus::Succeeded);
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
            secret: "secret".to_string(),
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
                    secret: "secret".to_string(),
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
                    secret: "secret".to_string(),
                    archived: false,
                    api_version: ApiVersion::V2,
                    workflow_kind: WorkflowKind::BaktaBaktfold,
                    result_kind: ResultKind::Bakta,
                },
            ),
        ]);

        purge_expired_states(&mut job_state, now);

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

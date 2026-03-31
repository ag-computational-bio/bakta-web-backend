use crate::api_structs::ArgoStatus;
use crate::api_structs::FailedJobStatus;
use crate::api_structs::FailedJobStatusEnum;
use crate::api_structs::Job;
use crate::api_structs::ListResponse;
use crate::api_structs::ResultResponse;
use crate::api_structs::StartRequest;
use crate::argo::structs::SimpleStatus;
use crate::{
    api_structs::{JobStatus, VersionResponse},
    argo::client::ArgoClient,
    s3_handler::S3Handler,
    v2::api_structs::{
        FailedJobStatus as V2FailedJobStatus, FailedJobStatusKind, JobReference,
        JobStatus as V2JobStatusKind, ResultKind, V2JobStatus, V2ListResponse, V2VersionResponse,
        WorkflowKind,
    },
    workflow_catalog::workflow_descriptor,
};
use anyhow::Result;
use anyhow::anyhow;
use chrono::DateTime;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiVersion {
    V1,
    V2,
}

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
    #[allow(dead_code)]
    pub version_v2: V2VersionResponse,
    pub state_handler: Arc<StateHandler>,
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
    let Some(job_id) = labels
        .get("jobid")
        .and_then(|value| Uuid::from_str(value).ok())
    else {
        return None;
    };

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

#[allow(dead_code)]
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
    #[allow(dead_code)]
    fn into_v2_job_status(&self) -> Option<V2JobStatus> {
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
        let s3_handler = S3Handler::new(s3_access_key, s3_secret_key, bucket, endpoint);

        let state_handler = Arc::new(StateHandler {
            job_state: RwLock::new(HashMap::new()),
            argo_client,
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
                baktfold_version,
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
                let initial = match argo_client.get_workflow_status().await {
                    Ok(status) => status,
                    Err(e) => {
                        tracing::error!(?e, "Failed to query workflow_status");
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        continue;
                    }
                };

                let mut write_lock = self.job_state.write().await;
                for item in initial.items {
                    if let Some(state) = state_from_simple_status(item) {
                        write_lock.insert(state.id, state);
                    }
                }
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

    #[allow(dead_code)]
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

                if let Some(api_status) = state.into_v2_job_status() {
                    jobs.push(api_status);
                }
            } else {
                failed_jobs.push(into_v2_failed_status(job_id, FailedJobStatusEnum::NotFound));
            }
        }

        V2ListResponse { jobs, failed_jobs }
    }

    pub async fn get_logs(&self, (job_id, secret): (Uuid, String)) -> Result<String> {
        let read_lock = self.job_state.read().await;
        if let Some(state) = read_lock.get(&job_id) {
            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }
            return self.argo_client.get_logs(state).await;
        }
        Err(anyhow!("Job not found"))
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

    #[allow(dead_code)]
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
                updated: None,
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

        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get_mut(id) {
            if state.secret != *secret {
                return Err(anyhow!("Unauthorized"));
            }

            let result = self
                .argo_client
                .submit_from_template(
                    format!("bakta-job-{}", bakta_version),
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
                .await?;

            state.workflowname = Some(result.metadata.name);
            state.status = Some(ArgoStatus::Pending);
            state.started = Some(result.metadata.creation_timestamp);
            state.updated = Some(Utc::now());
            state.api_version = ApiVersion::V1;
            state.workflow_kind = WorkflowKind::Bakta;
            state.result_kind = ResultKind::Bakta;
        }
        Ok(())
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
}

use crate::api_structs::FailedJobStatus;
use crate::api_structs::FailedJobStatusEnum;
use crate::api_structs::Job;
use crate::api_structs::ListResponse;
use crate::api_structs::ResultResponse;
use crate::api_structs::StartRequest;
use crate::argo::structs::SimpleStatus;
use crate::{
    api_structs::{JobStatus, JobStatusEnum, VersionResponse},
    argo::client::ArgoClient,
    s3_handler::S3Handler,
};
use anyhow::anyhow;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use rand::distributions::Alphanumeric;
use rand::distributions::DistString;
use regex::Regex;
use std::{collections::HashMap, str::FromStr, sync::Arc};
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct StateHandler {
    pub job_state: RwLock<HashMap<Uuid, FullJobState>>,
    pub argo_client: Arc<ArgoClient>,
}

pub struct FullJobState {
    pub id: Uuid,
    pub name: String,
    pub status: Option<JobStatusEnum>,
    pub started: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
    pub workflowname: Option<String>,
    pub secret: String,
    pub archived: bool,
}

impl From<&FullJobState> for Option<JobStatus> {
    fn from(state: &FullJobState) -> Self {
        Some(JobStatus {
            id: state.id,
            status: state.status.clone()?,
            started: state.started?,
            updated: state.updated?,
            name: state.name.clone(),
        })
    }
}

pub struct BaktaHandler {
    pub s3_handler: S3Handler,
    pub version: VersionResponse,
    pub state_handler: Arc<StateHandler>,
}

impl BaktaHandler {
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
                tool: bakta_version,
                db: database_version,
                backend: backend_version,
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
            let into_state = |simple_status: SimpleStatus| -> Result<FullJobState> {
                let job_id = Uuid::from_str(
                    simple_status
                        .metadata
                        .labels
                        .get("jobid")
                        .ok_or_else(|| anyhow!("Missing JobID"))?,
                )?;
                let workflowname = simple_status.metadata.name;

                Ok(FullJobState {
                    id: job_id,
                    status: Some(JobStatusEnum::try_from(simple_status.status.phase)?),
                    started: Some(simple_status.status.started_at),
                    updated: Some(simple_status.status.finished_at.unwrap_or(Utc::now())),
                    workflowname: Some(workflowname),
                    secret: simple_status
                        .metadata
                        .labels
                        .get("secret")
                        .cloned()
                        .unwrap_or_else(|| "Unknown".to_string()),
                    name: simple_status
                        .metadata
                        .labels
                        .get("name")
                        .cloned()
                        .unwrap_or_else(|| "Unknown name".to_string()),
                    archived: if simple_status
                        .metadata
                        .labels
                        .contains_key("workflows.argoproj.io/workflow-archiving-status")
                    {
                        true
                    } else {
                        false
                    },
                })
            };

            let initial = argo_client.get_workflow_status().await.map_err(|e| {
                tracing::error!(?e, "Failed to get initial workflow status");
                e
            })?;
            let mut write_lock = self.job_state.write().await;
            for item in initial.items {
                let state = into_state(item).map_err(|e| {
                    tracing::error!(?e, "Failed to parse state");
                    e
                })?;
                write_lock.insert(state.id, state);
            }
            drop(write_lock);
            'outer: loop {
                let Ok(initial) = argo_client.get_workflow_status().await.map_err(|e| {
                    tracing::error!(?e, "Failed to query workflow_status");
                }) else {
                    continue;
                };
                for item in initial.items {
                    let Ok(state) = into_state(item).map_err(|e| {
                        tracing::error!(?e, "Failed to parse_state");
                    }) else {
                        continue 'outer;
                    };
                    self.job_state.write().await.insert(state.id, state);
                }
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
                    if !matches!(
                        state.status,
                        Some(JobStatusEnum::ERROR) | Some(JobStatusEnum::SUCCESSFUL)
                    ) {
                        api_status.updated = Utc::now();
                    }
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

    pub async fn get_logs(&self, (job_id, secret): (Uuid, String)) -> Result<String> {
        todo!();

        // let read_lock = self.job_state.read().await;
        // if let Some(state) = read_lock.get(&job_id) {
        //     if state.secret != secret {
        //         return Err(anyhow!("Unauthorized"));
        //     }
        //     if let Some(workflowname) = &state.workflowname {
        //         return self
        //             .argo_client
        //             .get_workflow_logs(workflowname.clone())
        //             .await;
        //     }
        // }
        // Err(anyhow!("Job not found"))
    }

    pub async fn delete_job(&self, (job_id, secret): (Uuid, String)) -> Result<()> {
        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get(&job_id) {
            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }
            if let Some(workflowname) = &state.workflowname {
                self.argo_client
                    .delete_workflow(workflowname.clone(), state.archived)
                    .await?;
            }
        }
        write_lock.remove(&job_id);
        Ok(())
    }

    pub async fn init_job(&self, name: String) -> (Uuid, String) {
        let mut result = REGEX.replace_all(&name, "_").to_string();
        result.truncate(63);
        let stripped = result
            .trim_end_matches(|c: char| !c.is_alphanumeric())
            .to_string();

        let job_id = Uuid::new_v4();
        let secret = Alphanumeric.sample_string(&mut rand::thread_rng(), 32);
        self.job_state.write().await.insert(
            job_id,
            FullJobState {
                id: job_id,
                status: None,
                started: None,
                updated: None,
                workflowname: None,
                secret: secret.clone(),
                name: stripped,
                archived: false,
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
            state.status = Some(JobStatusEnum::INIT);
            state.started = Some(result.metadata.creation_timestamp);
            state.updated = Some(Utc::now());
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

            if let Some(status) = &state.status {
                if status != &JobStatusEnum::SUCCESSFUL {
                    return Err(anyhow!("Job not finished"));
                }
            }

            return Ok(ResultResponse {
                id,
                started: state.started.unwrap_or_default(),
                updated: state.updated.unwrap_or_default(),
                name: state.name.clone(),
                files: s3_handler.sign_download_urls(id.to_string().as_str())?,
            });
        }
        Err(anyhow!("Job not found"))
    }
}

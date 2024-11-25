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
    pub api_status: Option<JobStatus>,
    pub workflowname: Option<String>,
    pub secret: String,
    pub name: String,
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
            argo_client: argo_client,
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
    static ref REGEX: Regex = Regex::new(r"[^0-9a-zA-Z_.]+").unwrap();
}

impl StateHandler {
    async fn run(self: Arc<Self>) {
        let argo_client = self.argo_client.clone();
        tokio::spawn(async move {
            let into_state = |simple_status: SimpleStatus| -> Result<(Uuid, FullJobState)> {
                let job_id = Uuid::from_str(
                    &simple_status
                        .metadata
                        .labels
                        .get("jobid")
                        .ok_or_else(|| anyhow!("Missing JobID"))?,
                )?;
                let workflowname = simple_status.metadata.name;

                let api_status = JobStatus {
                    id: job_id.clone(),
                    status: JobStatusEnum::try_from(simple_status.status.phase)?,
                    started: simple_status.status.started_at,
                    updated: simple_status.status.finished_at.unwrap_or(Utc::now()),
                    name: simple_status
                        .metadata
                        .labels
                        .get("name")
                        .cloned()
                        .unwrap_or_else(|| "Unknown name".to_string()),
                };

                let name = api_status.name.clone();

                Ok((
                    api_status.id.clone(),
                    FullJobState {
                        api_status: Some(api_status),
                        workflowname: Some(workflowname),
                        secret: simple_status
                            .metadata
                            .labels
                            .get("secret")
                            .cloned()
                            .unwrap_or_else(|| "Unknown".to_string()),
                        name,
                    },
                ))
            };

            let initial = argo_client.get_workflow_status().await.map_err(|e| {
                tracing::error!(?e, "Failed to get initial workflow status");
                e
            })?;
            let mut write_lock = self.job_state.write().await;
            for item in initial.items {
                let (id, state) = into_state(item).map_err(|e| {
                    tracing::error!(?e, "Failed to parse state");
                    e
                })?;
                write_lock.insert(id, state);
            }
            drop(write_lock);
            'outer: loop {
                let Ok(initial) = argo_client.get_workflow_status().await.map_err(|e| {
                    tracing::error!(?e, "Failed to query workflow_status");
                }) else {
                    continue;
                };
                for item in initial.items {
                    let Ok((id, state)) = into_state(item).map_err(|e| {
                        tracing::error!(?e, "Failed to parse_state");
                    }) else {
                        continue 'outer;
                    };
                    self.job_state.write().await.insert(id, state);
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
                if let Some(api_status) = &state.api_status {
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

    pub async fn delete_job(&self, (job_id, secret): (Uuid, String)) -> Result<()> {
        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get(&job_id) {
            if state.secret != secret {
                return Err(anyhow!("Unauthorized"));
            }
            if let Some(workflowname) = &state.workflowname {
                self.argo_client
                    .delete_workflow(workflowname.clone())
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
                api_status: None,
                workflowname: None,
                secret: secret.clone(),
                name: stripped,
            },
        );
        (job_id, secret)
    }

    pub async fn start_job(
        &self,
        start_settings: StartRequest,
        bakta_version: String,
    ) -> Result<()> {
        let Job { id, secret } = &start_settings.job;

        let parameters = start_settings.config.into_parameters();

        let mut write_lock = self.job_state.write().await;
        if let Some(state) = write_lock.get_mut(&id) {
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
                    ])),
                    Some(HashMap::from([
                        ("parameter".to_string(), parameters),
                        ("jobid".to_string(), id.to_string()),
                    ])),
                    None,
                    Some(format!("bakta-job-{}-", id.to_string())),
                )
                .await?;

            state.workflowname = Some(result.metadata.name);
            state.api_status = Some(JobStatus {
                id: id.clone(),
                status: JobStatusEnum::INIT,
                started: result.metadata.creation_timestamp,
                updated: Utc::now(),
                name: state.name.clone(),
            });
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

            if let Some(state) = &state.api_status {
                if state.status != JobStatusEnum::SUCCESSFULL {
                    return Err(anyhow!("Job not finished"));
                }
            }

            if let Some(state) = &state.api_status {
                return Ok(ResultResponse {
                    id,
                    started: state.started,
                    updated: state.updated,
                    name: state.name.clone(),
                    files: s3_handler.sign_download_urls(id.to_string().as_str())?,
                });
            }
        }
        Err(anyhow!("Job not found"))
    }
}

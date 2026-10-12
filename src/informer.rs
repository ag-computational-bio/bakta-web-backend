//! Workflows from the Kubernetes API, kept current with a watch. At
//! startup, all Workflow objects of the namespace are listed in pages. A
//! watch from the resource version of the list then reports added, changed
//! and deleted workflows. A dropped watch resumes from the last resource
//! version. Only when Kubernetes no longer has that version (`410 Gone`) are
//! all workflows listed again. The job state is updated from these
//! workflows, so no request lists all workflows from Argo.

use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    time::Instant,
};
use uuid::Uuid;

use crate::{
    argo::structs::SimpleStatus,
    backfill::workflows::{KubeWorkflow, Page},
    bakta_handler::{StateHandler, job_id_label},
};

const SERVICE_ACCOUNT_CA: &str = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt";
const PAGE_SIZE: &str = "500";
const PAGE_TIMEOUT: Duration = Duration::from_secs(120);
/// Kubernetes ends a watch after this time. It is resumed at once.
const WATCH_SECONDS: &str = "300";
/// A watch that sends nothing, not even a bookmark, for this long is
/// dropped and resumed.
const READ_TIMEOUT: Duration = Duration::from_secs(150);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Changed workflows are applied together this long after the first change.
const BATCH_DELAY: Duration = Duration::from_secs(1);
/// Interval of the time-based work: expiry, unresolved submissions, skipped
/// jobs and old rows of gone workflows.
const TICK: Duration = Duration::from_secs(15);

/// Reads Workflow objects of one namespace from the Kubernetes API.
pub struct KubeApi {
    client: reqwest::Client,
    workflows_url: String,
    token: String,
}

impl KubeApi {
    /// The API server from `KUBERNETES_API_URL`, or in a pod from
    /// `KUBERNETES_SERVICE_HOST` and `KUBERNETES_SERVICE_PORT`. The CA
    /// certificate comes from `KUBERNETES_CA_FILE` or the service account.
    /// Without one, the system certificates are used.
    pub fn from_env(token: String, namespace: &str) -> Result<Self> {
        let var = |name: &str| dotenvy::var(name).ok().filter(|value| !value.is_empty());
        let base_url = match var("KUBERNETES_API_URL") {
            Some(url) => url,
            None => {
                let host = var("KUBERNETES_SERVICE_HOST")
                    .context("KUBERNETES_API_URL or KUBERNETES_SERVICE_HOST must be set")?;
                let port = var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|| "443".to_string());
                if host.contains(':') {
                    format!("https://[{host}]:{port}")
                } else {
                    format!("https://{host}:{port}")
                }
            }
        };
        let ca_file = var("KUBERNETES_CA_FILE").or_else(|| {
            std::path::Path::new(SERVICE_ACCOUNT_CA)
                .exists()
                .then(|| SERVICE_ACCOUNT_CA.to_string())
        });
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(READ_TIMEOUT);
        if let Some(path) = ca_file {
            let ca = std::fs::read(&path).with_context(|| format!("cannot read {path}"))?;
            builder = builder.tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca)?);
        }
        Ok(Self::new(builder.build()?, &base_url, token, namespace))
    }

    pub fn new(client: reqwest::Client, base_url: &str, token: String, namespace: &str) -> Self {
        Self {
            client,
            workflows_url: format!(
                "{}/apis/argoproj.io/v1alpha1/namespaces/{namespace}/workflows",
                base_url.trim_end_matches('/')
            ),
            token,
        }
    }

    /// All workflows with a `jobid` or `bakta-environment` label, and the
    /// resource version of the list. Workflows that cannot be read are
    /// logged and skipped.
    async fn list(&self) -> Result<(Vec<SimpleStatus>, String)> {
        let mut items = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let mut url = url::Url::parse(&self.workflows_url)?;
            url.query_pairs_mut().append_pair("limit", PAGE_SIZE);
            if let Some(token) = next.as_deref() {
                url.query_pairs_mut().append_pair("continue", token);
            }
            let page: Page<Box<RawValue>> = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .timeout(PAGE_TIMEOUT)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            for item in page.items {
                match serde_json::from_str::<KubeWorkflow>(item.get()) {
                    Ok(workflow) => items.extend(relevant(workflow.into_simple_status())),
                    Err(error) => tracing::warn!(%error, "workflow_unreadable"),
                }
            }
            next = page.metadata.next.filter(|token| !token.is_empty());
            if next.is_none() {
                let version = page
                    .metadata
                    .resource_version
                    .context("workflow list without resource version")?;
                return Ok((items, version));
            }
        }
    }

    async fn watch(&self, resource_version: &str) -> Result<reqwest::Response, WatchEnd> {
        let mut url =
            url::Url::parse(&self.workflows_url).map_err(|e| WatchEnd::Failed(e.into()))?;
        url.query_pairs_mut()
            .append_pair("watch", "true")
            .append_pair("resourceVersion", resource_version)
            .append_pair("allowWatchBookmarks", "true")
            .append_pair("timeoutSeconds", WATCH_SECONDS);
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| WatchEnd::Failed(e.into()))?;
        if response.status() == reqwest::StatusCode::GONE {
            return Err(WatchEnd::Gone);
        }
        response
            .error_for_status()
            .map_err(|e| WatchEnd::Failed(e.into()))
    }
}

/// Only workflows with a `jobid` or `bakta-environment` label matter for
/// the job state and the admin API.
fn relevant(item: SimpleStatus) -> Option<SimpleStatus> {
    let labels = &item.metadata.labels;
    (labels.contains_key("jobid") || labels.contains_key("bakta-environment")).then_some(item)
}

/// A change of the workflows in `StateHandler::workflows`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WorkflowChange {
    /// A complete list replaced all workflows. `observed_at` is a time
    /// before the list was requested.
    Listed { observed_at: DateTime<Utc> },
    /// A workflow was added, changed or deleted. `jobs` are the job IDs of
    /// its old and new state.
    Changed {
        name: String,
        jobs: Vec<Uuid>,
        observed_at: DateTime<Utc>,
    },
}

#[derive(Debug)]
enum WatchEnd {
    /// The resource version is too old: all workflows must be listed again.
    Gone,
    /// Kubernetes ended the watch.
    Closed,
    Failed(anyhow::Error),
}

#[derive(Deserialize)]
struct WatchEvent {
    #[serde(rename = "type")]
    kind: String,
    object: Box<RawValue>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ObjectMetadata {
    #[serde(default)]
    resource_version: Option<String>,
}

#[derive(Deserialize)]
struct Object {
    #[serde(default)]
    metadata: ObjectMetadata,
}

#[derive(Deserialize)]
struct WatchStatus {
    #[serde(default)]
    code: u16,
    #[serde(default)]
    message: String,
}

/// Starts the list and watch and the task that applies the changes.
pub fn start(state: Arc<StateHandler>, api: KubeApi) {
    let (changes, receiver) = unbounded_channel();
    tokio::spawn(process(state.clone(), receiver));
    tokio::spawn(
        Watcher {
            api,
            state,
            changes,
        }
        .run(),
    );
}

struct Watcher {
    api: KubeApi,
    state: Arc<StateHandler>,
    changes: UnboundedSender<WorkflowChange>,
}

impl Watcher {
    async fn run(self) {
        let mut resource_version: Option<String> = None;
        let mut backoff = MIN_BACKOFF;
        loop {
            let mut version = match resource_version.take() {
                Some(version) => version,
                None => match self.list().await {
                    Ok(version) => version,
                    Err(error) => {
                        self.state.metrics.record_watch_error();
                        tracing::warn!(error = format!("{error:#}"), "workflow_list_failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                },
            };
            let (end, received) = self.watch(&mut version).await;
            if received {
                backoff = MIN_BACKOFF;
            }
            match end {
                WatchEnd::Gone => {
                    self.state.metrics.record_relist();
                    tracing::warn!("workflow_watch_expired, listing all workflows again");
                }
                WatchEnd::Closed if received => resource_version = Some(version),
                WatchEnd::Closed => {
                    resource_version = Some(version);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                WatchEnd::Failed(error) => {
                    resource_version = Some(version);
                    self.state.metrics.record_watch_error();
                    tracing::warn!(error = format!("{error:#}"), "workflow_watch_failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Lists all workflows, replaces the known ones and returns the resource
    /// version of the list. Workflows missing from the list count as deleted.
    async fn list(&self) -> Result<String> {
        let observed_at = Utc::now();
        let (items, version) = self.api.list().await?;
        let count = items.len();
        *self
            .state
            .workflows
            .write()
            .expect("workflows lock poisoned") = items
            .into_iter()
            .map(|item| (item.metadata.name.clone(), item))
            .collect();
        self.state.metrics.record_workflow_list(Utc::now());
        tracing::info!(workflows = count, "workflow_list_finished");
        let _ = self.changes.send(WorkflowChange::Listed { observed_at });
        Ok(version)
    }

    /// Applies the events of one watch and keeps `version` at the last seen
    /// resource version. Returns how the watch ended and whether it sent
    /// anything.
    async fn watch(&self, version: &mut String) -> (WatchEnd, bool) {
        let mut response = match self.api.watch(version).await {
            Ok(response) => response,
            Err(end) => return (end, false),
        };
        self.state.watch_connected.store(true, Ordering::Relaxed);
        self.state.metrics.record_watch_event(Utc::now());
        let mut buffer = Vec::new();
        let mut received = false;
        let end = 'stream: loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<u8> = buffer.drain(..=end).collect();
                        if line.trim_ascii().is_empty() {
                            continue;
                        }
                        received = true;
                        if let Err(end) = self.apply_event(&line, version) {
                            break 'stream end;
                        }
                    }
                }
                Ok(None) => break WatchEnd::Closed,
                Err(error) => break WatchEnd::Failed(error.into()),
            }
        };
        self.state.watch_connected.store(false, Ordering::Relaxed);
        (end, received)
    }

    fn apply_event(&self, line: &[u8], version: &mut String) -> Result<(), WatchEnd> {
        let event: WatchEvent =
            serde_json::from_slice(line).map_err(|error| WatchEnd::Failed(error.into()))?;
        self.state.metrics.record_watch_event(Utc::now());
        let deleted = match event.kind.as_str() {
            "ADDED" | "MODIFIED" => false,
            "DELETED" => true,
            "BOOKMARK" => {
                if let Ok(object) = serde_json::from_str::<Object>(event.object.get())
                    && let Some(bookmark) = object.metadata.resource_version
                {
                    *version = bookmark;
                }
                return Ok(());
            }
            "ERROR" => {
                let status: WatchStatus = serde_json::from_str(event.object.get())
                    .map_err(|error| WatchEnd::Failed(error.into()))?;
                if status.code == 410 {
                    return Err(WatchEnd::Gone);
                }
                return Err(WatchEnd::Failed(anyhow!(
                    "watch error {}: {}",
                    status.code,
                    status.message
                )));
            }
            other => {
                tracing::debug!(kind = other, "workflow_watch_event_ignored");
                return Ok(());
            }
        };
        let workflow = match serde_json::from_str::<KubeWorkflow>(event.object.get()) {
            Ok(workflow) => workflow,
            Err(error) => {
                tracing::warn!(%error, "workflow_unreadable");
                if let Ok(object) = serde_json::from_str::<Object>(event.object.get())
                    && let Some(next) = object.metadata.resource_version
                {
                    *version = next;
                }
                return Ok(());
            }
        };
        if let Some(next) = workflow.resource_version() {
            *version = next.to_string();
        }
        self.apply_workflow(workflow.into_simple_status(), deleted);
        Ok(())
    }

    fn apply_workflow(&self, item: SimpleStatus, deleted: bool) {
        let observed_at = Utc::now();
        let name = item.metadata.name.clone();
        let mut jobs: Vec<Uuid> = job_id_label(&item).into_iter().collect();
        let new = relevant(item).filter(|_| !deleted);
        let changed = new.is_some();
        let previous = {
            let mut workflows = self
                .state
                .workflows
                .write()
                .expect("workflows lock poisoned");
            match new {
                Some(item) => workflows.insert(name.clone(), item),
                None => workflows.remove(&name),
            }
        };
        if !changed && previous.is_none() {
            return;
        }
        jobs.extend(previous.as_ref().and_then(job_id_label));
        let _ = self.changes.send(WorkflowChange::Changed {
            name,
            jobs,
            observed_at,
        });
    }
}

/// Changes waiting to be applied together.
#[derive(Default)]
struct Batch {
    names: HashSet<String>,
    jobs: HashSet<Uuid>,
    /// The earliest time a change of the batch was seen.
    observed_at: Option<DateTime<Utc>>,
    due: Option<Instant>,
}

/// Applies the workflow changes to the job state and the `workflows`
/// table, and runs the time-based work.
async fn process(state: Arc<StateHandler>, mut changes: UnboundedReceiver<WorkflowChange>) {
    let mut batch = Batch::default();
    // Jobs that were skipped and need another pass.
    let mut retry = HashSet::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let due = batch.due;
        tokio::select! {
            change = changes.recv() => match change {
                None => return,
                Some(WorkflowChange::Listed { observed_at }) => {
                    // The list covers all changes before it.
                    batch = Batch::default();
                    retry = state.apply_listed_workflows(observed_at).await;
                }
                Some(WorkflowChange::Changed { name, jobs, observed_at }) => {
                    batch.names.insert(name);
                    batch.jobs.extend(jobs);
                    batch.observed_at.get_or_insert(observed_at);
                    batch.due.get_or_insert_with(|| Instant::now() + BATCH_DELAY);
                }
            },
            () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                let batch = std::mem::take(&mut batch);
                retry = state.apply_changed_workflows(batch, std::mem::take(&mut retry)).await;
            }
            _ = tick.tick() => {
                retry = state.run_timed_work(std::mem::take(&mut retry)).await;
            }
        }
    }
}

impl StateHandler {
    /// IDs of the jobs with an unresolved submission.
    fn marked_jobs(&self) -> Vec<Uuid> {
        self.unknown_submissions
            .lock()
            .expect("unknown submissions lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    /// All known workflows of the jobs in `jobs`.
    fn job_workflows(&self, jobs: &HashSet<Uuid>) -> Vec<SimpleStatus> {
        self.workflows
            .read()
            .expect("workflows lock poisoned")
            .values()
            .filter(|item| job_id_label(item).is_some_and(|id| jobs.contains(&id)))
            .cloned()
            .collect()
    }

    /// Applies a complete list, like a poll did before.
    async fn apply_listed_workflows(&self, observed_at: DateTime<Utc>) -> HashSet<Uuid> {
        let items: Vec<SimpleStatus> = self
            .workflows
            .read()
            .expect("workflows lock poisoned")
            .values()
            .cloned()
            .collect();
        self.record_workflows(&items, observed_at).await;
        self.apply_workflow_statuses(items, observed_at).await
    }

    /// Writes the changed workflows and applies all workflows of their jobs,
    /// of the `retry` jobs and of jobs with an unresolved submission.
    async fn apply_changed_workflows(
        &self,
        batch: Batch,
        mut jobs: HashSet<Uuid>,
    ) -> HashSet<Uuid> {
        jobs.extend(batch.jobs);
        jobs.extend(self.marked_jobs());
        let (changed, gone): (Vec<_>, Vec<_>) = {
            let workflows = self.workflows.read().expect("workflows lock poisoned");
            let mut changed = Vec::new();
            let mut gone = Vec::new();
            for name in batch.names {
                match workflows.get(&name) {
                    Some(item) => changed.push(item.clone()),
                    None => gone.push(name),
                }
            }
            (changed, gone)
        };
        let observed_at = batch.observed_at.unwrap_or_else(Utc::now);
        self.record_changed_workflows(&changed, &gone, observed_at)
            .await;
        let items = self.job_workflows(&jobs);
        self.apply_job_workflows(items, &jobs, Utc::now()).await
    }

    /// Drops expired jobs, counts checks for unresolved submissions, applies
    /// skipped jobs again and deletes old rows of gone workflows. While the
    /// watch is not connected, the workflows may be outdated: then only
    /// expired jobs are dropped, like after a failed poll before.
    async fn run_timed_work(&self, retry: HashSet<Uuid>) -> HashSet<Uuid> {
        let now = Utc::now();
        if let Some(db) = self.stats.db() {
            let prune = db.prune_gone_workflows(self.argo_client.namespace(), now);
            match tokio::time::timeout(Duration::from_secs(10), prune).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "workflow_table_prune_failed"),
                Err(_) => tracing::warn!("workflow_table_prune_timed_out"),
            }
        }
        if !self.watch_connected.load(Ordering::Relaxed) {
            self.apply_job_workflows(Vec::new(), &HashSet::new(), now)
                .await;
            return retry;
        }
        let mut jobs = retry;
        jobs.extend(self.marked_jobs());
        let items = self.job_workflows(&jobs);
        self.apply_job_workflows(items, &jobs, now).await
    }
}

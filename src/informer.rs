//! Workflows from the Kubernetes API, kept current with a watch. Only
//! workflows that are not completed are watched: at startup, they are
//! listed in pages, and a watch from the resource version of the list then
//! reports added, changed and deleted ones. A dropped watch resumes from the
//! last resource version. Only when Kubernetes no longer has that version
//! (`410 Gone`) are they listed again.
//!
//! A workflow that completes gets the label
//! `workflows.argoproj.io/completed=true` and leaves the watched set.
//! Kubernetes then sends a `DELETED` event with the state before the
//! change, so the workflow is fetched once to tell a completion (final
//! state) from a real delete (not found). After each list, the completed
//! workflows are listed once, to write those that completed while the
//! backend did not watch. The job state is updated from these workflows,
//! so no request lists all workflows from Argo.

use std::{
    collections::{HashMap, HashSet},
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
    bakta_handler::{JOB_RETENTION_DAYS, StateHandler, job_id_label},
};

/// Label selectors of workflows that are not completed, and of completed ones.
const LIVE_SELECTOR: &str = "workflows.argoproj.io/completed!=true";
const COMPLETED_SELECTOR: &str = "workflows.argoproj.io/completed=true";
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
/// jobs, finished jobs and old rows of gone workflows.
const TICK: Duration = Duration::from_secs(15);
/// Interval of the expiry of finished job records in the database.
const RECORD_EXPIRY_INTERVAL: Duration = Duration::from_secs(3600);
/// Attempts to fetch a workflow after a `DELETED` event.
const FETCH_ATTEMPTS: u32 = 3;

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

    /// One page of the workflows that match `selector`, with a
    /// `jobid` or `bakta-environment` label, and the continue token and
    /// resource version. Workflows that cannot be read are logged and skipped.
    async fn list_page(
        &self,
        selector: &str,
        next: Option<&str>,
    ) -> Result<(Vec<SimpleStatus>, Option<String>, Option<String>)> {
        let mut url = url::Url::parse(&self.workflows_url)?;
        url.query_pairs_mut()
            .append_pair("labelSelector", selector)
            .append_pair("limit", PAGE_SIZE);
        if let Some(token) = next {
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
        let mut items = Vec::new();
        for item in page.items {
            match serde_json::from_str::<KubeWorkflow>(item.get()) {
                Ok(workflow) => items.extend(relevant(workflow.into_simple_status())),
                Err(error) => tracing::warn!(%error, "workflow_unreadable"),
            }
        }
        let next = page.metadata.next.filter(|token| !token.is_empty());
        Ok((items, next, page.metadata.resource_version))
    }

    /// All workflows that are not completed, and the resource version of
    /// the list.
    async fn list(&self) -> Result<(Vec<SimpleStatus>, String)> {
        let mut items = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let (page, page_next, version) = self.list_page(LIVE_SELECTOR, next.as_deref()).await?;
            items.extend(page);
            next = page_next;
            if next.is_none() {
                let version = version.context("workflow list without resource version")?;
                return Ok((items, version));
            }
        }
    }

    /// One workflow, or `None` when it does not exist.
    async fn get(&self, name: &str) -> Result<Option<SimpleStatus>> {
        let response = self
            .client
            .get(format!("{}/{name}", self.workflows_url))
            .bearer_auth(&self.token)
            .timeout(PAGE_TIMEOUT)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = response.error_for_status()?.bytes().await?;
        let workflow: KubeWorkflow = serde_json::from_slice(&body)?;
        Ok(Some(workflow.into_simple_status()))
    }

    async fn watch(&self, resource_version: &str) -> Result<reqwest::Response, WatchEnd> {
        let mut url =
            url::Url::parse(&self.workflows_url).map_err(|e| WatchEnd::Failed(e.into()))?;
        url.query_pairs_mut()
            .append_pair("labelSelector", LIVE_SELECTOR)
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

fn is_completed(item: &SimpleStatus) -> bool {
    item.metadata
        .labels
        .get("workflows.argoproj.io/completed")
        .is_some_and(|value| value == "true")
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
    /// A workflow completed. It is no longer watched: `item` is its final
    /// state.
    Completed {
        item: Box<SimpleStatus>,
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
    let api = Arc::new(api);
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
    api: Arc<KubeApi>,
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
        tokio::spawn(reconcile_completed(
            self.api.clone(),
            self.state.clone(),
            self.changes.clone(),
            observed_at,
        ));
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
                        if let Err(end) = self.apply_event(&line, version).await {
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

    async fn apply_event(&self, line: &[u8], version: &mut String) -> Result<(), WatchEnd> {
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
        let item = workflow.into_simple_status();
        if is_completed(&item) {
            self.complete(item);
        } else if deleted {
            // The event has the state before the workflow left the watched
            // set: a completion or a real delete.
            match self.final_state(&item.metadata.name).await {
                Some(completed) => self.complete(completed),
                None => self.apply_workflow(item, true),
            }
        } else {
            self.apply_workflow(item, false);
        }
        Ok(())
    }

    /// The final state of a workflow that left the watched set, or `None`
    /// when it was deleted (or cannot be fetched).
    async fn final_state(&self, name: &str) -> Option<SimpleStatus> {
        for attempt in 1..=FETCH_ATTEMPTS {
            match self.api.get(name).await {
                Ok(item) => return item.filter(is_completed),
                Err(error) if attempt < FETCH_ATTEMPTS => {
                    tracing::debug!(workflow_name = name, %error, "workflow_fetch_retry");
                    tokio::time::sleep(MIN_BACKOFF).await;
                }
                Err(error) => {
                    self.state.metrics.record_watch_error();
                    tracing::warn!(workflow_name = name, %error, "workflow_fetch_failed");
                }
            }
        }
        None
    }

    /// Drops a completed workflow from the watched ones and reports its
    /// final state.
    fn complete(&self, item: SimpleStatus) {
        let Some(item) = relevant(item) else {
            return;
        };
        let name = item.metadata.name.clone();
        let previous = self
            .state
            .workflows
            .write()
            .expect("workflows lock poisoned")
            .remove(&name);
        let mut jobs: Vec<Uuid> = job_id_label(&item).into_iter().collect();
        jobs.extend(previous.as_ref().and_then(job_id_label));
        let _ = self.changes.send(WorkflowChange::Completed {
            item: Box::new(item),
            jobs,
            observed_at: Utc::now(),
        });
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
    /// Final states of workflows that completed.
    completed: HashMap<String, SimpleStatus>,
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
    // Final workflow states of finished jobs that are still in memory.
    let mut finished = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut records_expired_at: Option<Instant> = None;
    loop {
        let due = batch.due;
        tokio::select! {
            change = changes.recv() => match change {
                None => return,
                Some(WorkflowChange::Listed { observed_at }) => {
                    retry = state.apply_listed_workflows(observed_at).await;
                }
                Some(WorkflowChange::Changed { name, jobs, observed_at }) => {
                    batch.add(name, jobs, observed_at);
                }
                Some(WorkflowChange::Completed { item, jobs, observed_at }) => {
                    let name = item.metadata.name.clone();
                    batch.completed.insert(name.clone(), *item);
                    batch.add(name, jobs, observed_at);
                }
            },
            () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                let batch = std::mem::take(&mut batch);
                retry = state
                    .apply_changed_workflows(batch, std::mem::take(&mut retry), &mut finished)
                    .await;
            }
            _ = tick.tick() => {
                let expire_records = records_expired_at
                    .is_none_or(|at| at.elapsed() >= RECORD_EXPIRY_INTERVAL);
                if expire_records {
                    records_expired_at = Some(Instant::now());
                }
                retry = state
                    .run_timed_work(std::mem::take(&mut retry), &mut finished, expire_records)
                    .await;
            }
        }
    }
}

impl Batch {
    fn add(&mut self, name: String, jobs: Vec<Uuid>, observed_at: DateTime<Utc>) {
        self.names.insert(name);
        self.jobs.extend(jobs);
        self.observed_at.get_or_insert(observed_at);
        self.due.get_or_insert_with(|| Instant::now() + BATCH_DELAY);
    }
}

/// Removes the `secret` labels and returns their hashes by workflow name.
fn take_secret_hashes(items: &mut [SimpleStatus]) -> HashMap<String, String> {
    items
        .iter_mut()
        .filter_map(|item| {
            let secret = item.metadata.labels.remove("secret")?;
            (!secret.is_empty()).then(|| {
                (
                    item.metadata.name.clone(),
                    crate::stats::secret_hash(&secret),
                )
            })
        })
        .collect()
}

/// Lists the completed workflows once, page by page, and writes those that
/// the database does not have in this state yet. Jobs in memory get the
/// final state of their workflow like after a completion. Then all rows of
/// workflows that Kubernetes no longer has are marked as gone. Without a
/// database, all finished jobs are kept in memory instead, as before.
async fn reconcile_completed(
    api: Arc<KubeApi>,
    state: Arc<StateHandler>,
    changes: UnboundedSender<WorkflowChange>,
    observed_at: DateTime<Utc>,
) {
    let mut names: Vec<String> = Vec::new();
    let mut next: Option<String> = None;
    let mut count = 0;
    loop {
        let (page, page_next) = match api.list_page(COMPLETED_SELECTOR, next.as_deref()).await {
            Ok((page, page_next, _)) => (page, page_next),
            Err(error) => {
                state.metrics.record_watch_error();
                tracing::warn!(
                    error = format!("{error:#}"),
                    "completed_workflows_list_failed"
                );
                return;
            }
        };
        let mut page = page;
        // Without a database, the jobs are kept in memory with their secret.
        let hashes = match state.stats.db() {
            Some(_) => take_secret_hashes(&mut page),
            None => HashMap::new(),
        };
        count += page.len();
        names.extend(page.iter().map(|item| item.metadata.name.clone()));
        state.upsert_workflows(&page, observed_at).await;
        for item in state.reconcile_jobs(page, &hashes, observed_at).await {
            let jobs = job_id_label(&item).into_iter().collect();
            let _ = changes.send(WorkflowChange::Completed {
                item: Box::new(item),
                jobs,
                observed_at,
            });
        }
        next = page_next;
        if next.is_none() {
            break;
        }
    }
    names.extend(
        state
            .workflows
            .read()
            .expect("workflows lock poisoned")
            .keys()
            .cloned(),
    );
    state.remove_missing_workflows(&names, observed_at).await;
    tracing::info!(workflows = count, "completed_workflows_reconciled");
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

    /// All watched workflows of the jobs in `jobs`.
    fn job_workflows(&self, jobs: &HashSet<Uuid>) -> Vec<SimpleStatus> {
        self.workflows
            .read()
            .expect("workflows lock poisoned")
            .values()
            .filter(|item| job_id_label(item).is_some_and(|id| jobs.contains(&id)))
            .cloned()
            .collect()
    }

    /// Applies the list of all watched workflows.
    async fn apply_listed_workflows(&self, observed_at: DateTime<Utc>) -> HashSet<Uuid> {
        let items: Vec<SimpleStatus> = self
            .workflows
            .read()
            .expect("workflows lock poisoned")
            .values()
            .cloned()
            .collect();
        self.upsert_workflows(&items, observed_at).await;
        self.apply_workflow_statuses(items, observed_at).await
    }

    /// Writes the changed workflows and applies all workflows of their jobs,
    /// of the `retry` jobs and of jobs with an unresolved submission.
    /// Completed workflows are applied with their final state and kept in
    /// `finished` until the job leaves memory.
    async fn apply_changed_workflows(
        &self,
        batch: Batch,
        mut jobs: HashSet<Uuid>,
        finished: &mut HashMap<Uuid, SimpleStatus>,
    ) -> HashSet<Uuid> {
        jobs.extend(batch.jobs);
        jobs.extend(self.marked_jobs());
        let (changed, gone) = {
            let workflows = self.workflows.read().expect("workflows lock poisoned");
            let mut changed = Vec::new();
            let mut gone = Vec::new();
            for name in batch.names {
                match workflows.get(&name).or(batch.completed.get(&name)) {
                    Some(item) => changed.push(item.clone()),
                    None => gone.push(name),
                }
            }
            (changed, gone)
        };
        let observed_at = batch.observed_at.unwrap_or_else(Utc::now);
        self.record_changed_workflows(&changed, &gone, observed_at)
            .await;
        let mut items = self.job_workflows(&jobs);
        for item in batch.completed.into_values() {
            if let Some(id) = job_id_label(&item)
                && jobs.contains(&id)
            {
                finished.insert(id, item.clone());
                items.push(item);
            }
        }
        self.apply_job_workflows(items, &jobs, Utc::now()).await
    }

    /// Drops expired jobs, counts checks for unresolved submissions, applies
    /// skipped jobs again, drops finished jobs whose final state is in the
    /// database and deletes old rows of gone workflows. While the watch is
    /// not connected, the workflows may be outdated: then only expired jobs
    /// are dropped, like after a failed poll before.
    async fn run_timed_work(
        &self,
        retry: HashSet<Uuid>,
        finished: &mut HashMap<Uuid, SimpleStatus>,
        expire_records: bool,
    ) -> HashSet<Uuid> {
        let now = Utc::now();
        if let Some(db) = self.stats.db() {
            let namespace = self.argo_client.namespace();
            let work = async {
                db.prune_gone_workflows(namespace, now).await?;
                if expire_records {
                    db.expire_finished_jobs(now - chrono::Duration::days(JOB_RETENTION_DAYS))
                        .await?;
                }
                anyhow::Ok(())
            };
            match tokio::time::timeout(Duration::from_secs(10), work).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, "timed_database_work_failed"),
                Err(_) => tracing::warn!("timed_database_work_timed_out"),
            }
            self.drop_finished_jobs(finished).await;
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

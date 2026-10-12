//! Workflows from the Kubernetes API, kept current with a watch. At
//! startup, all Workflow objects of the namespace are listed in pages. A
//! watch from the resource version of the list then reports added, changed
//! and deleted workflows. A dropped watch resumes from the last resource
//! version. Only when Kubernetes no longer has that version (`410 Gone`) are
//! all workflows listed again.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{
    argo::structs::SimpleStatus,
    backfill::workflows::{KubeWorkflow, Page},
    bakta_handler::StateHandler,
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

/// Starts the list and watch.
pub fn start(state: Arc<StateHandler>, api: KubeApi) {
    tokio::spawn(Watcher { api, state }.run());
}

struct Watcher {
    api: KubeApi,
    state: Arc<StateHandler>,
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
        let name = item.metadata.name.clone();
        let mut workflows = self
            .state
            .workflows
            .write()
            .expect("workflows lock poisoned");
        match relevant(item).filter(|_| !deleted) {
            Some(item) => workflows.insert(name, item),
            None => workflows.remove(&name),
        };
    }
}

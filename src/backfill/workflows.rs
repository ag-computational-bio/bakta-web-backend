//! Bakta Web workflows read from the Kubernetes API, and the job records
//! made from them. The Argo server only lists recent workflows, the
//! Kubernetes API lists all of them.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::{db::BackfillJob, parse};
use crate::{
    argo::structs::{SimpleStatus, SimpleStatusStatus, StatusMetadata},
    bakta_handler::{
        is_state_expired, job_id_label, polled_workflow_state, state_from_simple_status,
        workflow_environment,
    },
    stats::is_terminal_status,
    workflow_catalog::is_bakta_web_template,
};

const KUBERNETES_URL: &str = "https://kubernetes.default.svc";
const SERVICE_ACCOUNT_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
const PAGE_SIZE: &str = "500";

/// The parts of a workflow the backfill uses.
#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    /// Name, labels and status, in the form the poll reads them.
    pub item: SimpleStatus,
    pub template: String,
    /// The `parameter` argument: the Bakta command line.
    pub parameter: String,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    items: Vec<serde_json::Value>,
    #[serde(default)]
    metadata: PageMetadata,
}

#[derive(Deserialize, Default)]
struct PageMetadata {
    #[serde(rename = "continue", default)]
    next: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KubeWorkflow {
    metadata: KubeMetadata,
    #[serde(default)]
    spec: KubeSpec,
    #[serde(default)]
    status: KubeStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KubeMetadata {
    name: String,
    #[serde(default)]
    uid: Uuid,
    creation_timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    labels: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KubeSpec {
    workflow_template_ref: Option<KubeTemplateRef>,
    #[serde(default)]
    arguments: KubeArguments,
}

#[derive(Deserialize)]
struct KubeTemplateRef {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct KubeArguments {
    #[serde(default)]
    parameters: Vec<KubeParameter>,
}

#[derive(Deserialize)]
struct KubeParameter {
    name: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KubeStatus {
    #[serde(default)]
    phase: String,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    message: Option<String>,
}

fn parse_workflow(value: serde_json::Value) -> Result<Workflow> {
    let workflow: KubeWorkflow = serde_json::from_value(value)?;
    let mut metadata = workflow.metadata;
    // The job secret grants access to the results and is never needed here.
    metadata.labels.remove("secret");
    let status = workflow.status;
    let parameter = workflow
        .spec
        .arguments
        .parameters
        .into_iter()
        .find(|parameter| parameter.name == "parameter")
        .map(|parameter| parameter.value)
        .unwrap_or_default();
    Ok(Workflow {
        item: SimpleStatus {
            metadata: StatusMetadata {
                name: metadata.name,
                uid: metadata.uid,
                resource_version: None,
                creation_timestamp: metadata.creation_timestamp,
                labels: metadata.labels,
            },
            // the template name is kept in `template` below
            spec: Default::default(),
            status: SimpleStatusStatus {
                phase: status.phase,
                // A workflow that never started has no start time.
                started_at: status
                    .started_at
                    .or(metadata.creation_timestamp)
                    .unwrap_or_default(),
                finished_at: status.finished_at,
                message: status.message,
            },
        },
        template: workflow
            .spec
            .workflow_template_ref
            .map(|template| template.name)
            .unwrap_or_default(),
        parameter,
    })
}

/// Reads workflows with the pod's service account.
pub struct KubeClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

impl KubeClient {
    pub fn in_cluster() -> Result<Self> {
        let token = std::fs::read_to_string(format!("{SERVICE_ACCOUNT_DIR}/token"))?;
        let ca = std::fs::read(format!("{SERVICE_ACCOUNT_DIR}/ca.crt"))?;
        let client = reqwest::Client::builder()
            .tls_certs_only(reqwest::Certificate::from_pem_bundle(&ca)?)
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self::new(client, KUBERNETES_URL, token.trim()))
    }

    pub fn new(client: reqwest::Client, base_url: &str, token: &str) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// All workflows of the namespace, page by page. Workflows that cannot be
    /// read are logged and counted, not returned.
    pub async fn list_workflows(&self, namespace: &str) -> Result<(Vec<Workflow>, usize)> {
        let url = format!(
            "{}/apis/argoproj.io/v1alpha1/namespaces/{namespace}/workflows",
            self.base_url
        );
        let mut workflows = Vec::new();
        let mut unreadable = 0;
        let mut next = None;
        loop {
            let mut page_url = url::Url::parse(&url)?;
            page_url.query_pairs_mut().append_pair("limit", PAGE_SIZE);
            if let Some(token) = next.as_deref() {
                page_url.query_pairs_mut().append_pair("continue", token);
            }
            let page: Page = self
                .client
                .get(page_url)
                .bearer_auth(&self.token)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            for item in page.items {
                match parse_workflow(item) {
                    Ok(workflow) => workflows.push(workflow),
                    Err(error) => {
                        unreadable += 1;
                        tracing::warn!(%error, "backfill_workflow_unreadable");
                    }
                }
            }
            tracing::info!(workflows = workflows.len(), "backfill_workflows_listed");
            next = page.metadata.next.filter(|token| !token.is_empty());
            if next.is_none() {
                return Ok((workflows, unreadable));
            }
        }
    }
}

/// Same rule as the admin API: a `bakta-environment` label, or a `jobid`
/// label and a Bakta Web workflow template.
pub fn is_bakta_web(workflow: &Workflow) -> bool {
    let labels = &workflow.item.metadata.labels;
    labels.contains_key("bakta-environment")
        || (labels.contains_key("jobid") && is_bakta_web_template(&workflow.template))
}

/// The newest workflow of each job, by creation time and then by name, like
/// the poll picks the current workflow after a resubmit. Sorted by creation time.
pub fn newest_per_job(workflows: Vec<Workflow>) -> Vec<Workflow> {
    let key = |workflow: &Workflow| {
        (
            workflow.item.metadata.creation_timestamp,
            workflow.item.metadata.name.clone(),
        )
    };
    let mut newest = HashMap::<Uuid, Workflow>::new();
    for workflow in workflows {
        let Some(id) = job_id_label(&workflow.item) else {
            continue;
        };
        if newest
            .get(&id)
            .is_none_or(|current| key(current) < key(&workflow))
        {
            newest.insert(id, workflow);
        }
    }
    let mut workflows: Vec<_> = newest.into_values().collect();
    workflows.sort_by_key(key);
    workflows
}

fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_milliseconds() as f64 / 1000.0
}

/// The job record the poll would have written for this workflow, plus the
/// details of the start request. Jobs past the job retention are `expired`,
/// with their outcome. `None` when the workflow has no valid job ID or phase.
pub fn backfill_job(workflow: &Workflow, now: DateTime<Utc>) -> Option<BackfillJob> {
    let state = state_from_simple_status(workflow.item.clone())?;
    let environment = workflow_environment(&workflow.item.metadata.labels);
    let polled = polled_workflow_state(&state, environment, &workflow.item)?;
    let terminal = is_terminal_status(polled.status);
    let status = if is_state_expired(&state, now) {
        "expired"
    } else {
        polled.status
    };
    // Computed like the poll's update of the record.
    let runtime_seconds = match (polled.started_at, polled.finished_at) {
        (Some(started), Some(finished)) => Some(seconds_between(started, finished).max(0.0)),
        _ => None,
    };
    let queue_seconds = match (polled.created_at, polled.started_at) {
        (Some(submitted), Some(started)) if started >= submitted => {
            Some(seconds_between(submitted, started))
        }
        _ => None,
    };
    let details = parse::submission_details(
        state.workflow_kind,
        state.api_version,
        state.id,
        &workflow.parameter,
    );
    let versions = parse::template_versions(&workflow.template)
        .map(|(_, versions)| versions)
        .unwrap_or_default();
    Some(BackfillJob {
        job: polled.job,
        status: status.to_string(),
        outcome: terminal.then(|| polled.status.to_string()),
        origin: polled.origin,
        workflow_name: polled.workflow_name,
        submitted_at: polled.created_at,
        started_at: polled.started_at,
        finished_at: polled.finished_at,
        runtime_seconds,
        queue_seconds,
        error_message: polled.error_message,
        genus: details.genus,
        species: details.species,
        strain: details.strain,
        config: details.config,
        versions,
        sequence_count: None,
        total_length: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use serde_json::json;

    /// A workflow as the Kubernetes API returns it, reduced to the used fields.
    fn workflow_json(
        name: &str,
        job_id: &str,
        template: &str,
        created: &str,
        labels: serde_json::Value,
    ) -> serde_json::Value {
        let mut all_labels = json!({
            "jobid": job_id,
            "name": "genome.fasta",
            "origin": "bakta.computational.bio",
            "secret": "not-a-real-secret",
            "workflows.argoproj.io/phase": "Succeeded",
        });
        all_labels
            .as_object_mut()
            .unwrap()
            .extend(labels.as_object().unwrap().clone());
        json!({
            "apiVersion": "argoproj.io/v1alpha1",
            "kind": "Workflow",
            "metadata": {
                "name": name,
                "uid": "7c1a4bf6-0d43-4f4c-9d4c-3f1f6b1f0a01",
                "creationTimestamp": created,
                "labels": all_labels,
            },
            "spec": {
                "arguments": {"parameters": [
                    {"name": "jobid", "value": job_id},
                    {"name": "parameter", "value": "--replicons /data/replicons.tsv --complete --genus 'Pantoea' --strain 'LMG 2665' --gram ?"},
                ]},
                "workflowTemplateRef": {"name": template},
            },
            "status": {
                "phase": "Succeeded",
                "startedAt": created,
                "finishedAt": "2026-06-01T10:05:00Z",
                "nodes": {"ignored": {"phase": "Succeeded"}},
            },
        })
    }

    const JOB: &str = "e1adc3a8-7e08-403c-8582-53f99aa588df";

    fn workflow(name: &str, job_id: &str, template: &str, created: &str) -> Workflow {
        parse_workflow(workflow_json(name, job_id, template, created, json!({}))).unwrap()
    }

    #[test]
    fn test_parse_workflow_reads_the_used_fields() {
        let workflow = workflow(
            "bakta-job-a",
            JOB,
            "bakta-job-1.12.0",
            "2026-06-01T10:00:00Z",
        );
        assert_eq!(workflow.template, "bakta-job-1.12.0");
        assert!(workflow.parameter.starts_with("--replicons"));
        assert_eq!(workflow.item.status.phase, "Succeeded");
        assert_eq!(workflow.item.metadata.labels["jobid"], JOB);
        assert!(!workflow.item.metadata.labels.contains_key("secret"));
        assert!(parse_workflow(json!({"metadata": {}})).is_err());
    }

    #[test]
    fn test_newest_workflow_per_job_is_kept() {
        let other_job = "0b6c1f3e-55a1-4c55-9a43-0c5b8a3d2f10";
        let workflows = vec![
            workflow("wf-b", JOB, "bakta-job-1.12.0", "2026-06-01T10:00:00Z"),
            workflow("wf-c", JOB, "bakta-job-1.12.0", "2026-06-02T10:00:00Z"),
            workflow("wf-a", JOB, "bakta-job-1.12.0", "2026-06-02T10:00:00Z"),
            workflow(
                "wf-d",
                other_job,
                "bakta-job-1.12.0",
                "2026-05-01T10:00:00Z",
            ),
            workflow(
                "wf-e",
                "not-a-uuid",
                "bakta-job-1.12.0",
                "2026-07-01T10:00:00Z",
            ),
        ];
        let names: Vec<_> = newest_per_job(workflows)
            .into_iter()
            .map(|workflow| workflow.item.metadata.name)
            .collect();
        // Same creation time: the last name wins, like in the poll.
        assert_eq!(names, ["wf-d", "wf-c"]);
    }

    #[test]
    fn test_only_bakta_web_workflows_are_kept() {
        assert!(is_bakta_web(&workflow(
            "a",
            JOB,
            "bakta-job-1.11.3",
            "2026-06-01T10:00:00Z"
        )));
        assert!(!is_bakta_web(&workflow(
            "b",
            JOB,
            "other-job",
            "2026-06-01T10:00:00Z"
        )));
        let labelled = workflow_json(
            "c",
            JOB,
            "other-job",
            "2026-06-01T10:00:00Z",
            json!({"bakta-environment": "staging"}),
        );
        assert!(is_bakta_web(&parse_workflow(labelled).unwrap()));
    }

    #[test]
    fn test_backfill_job_of_an_old_production_workflow() {
        let workflow = workflow(
            "bakta-job-a",
            JOB,
            "bakta-job-1.12.0",
            "2026-06-01T10:00:00Z",
        );
        let now = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
        let job = backfill_job(&workflow, now).unwrap();
        let created = Utc.with_ymd_and_hms(2026, 6, 1, 10, 0, 0).unwrap();
        assert_eq!(job.job.id.to_string(), JOB);
        assert_eq!(job.job.environment, "production");
        assert_eq!(job.job.workflow_kind, "bakta");
        assert_eq!(job.job.api_version, "v1");
        assert_eq!(job.job.name.as_deref(), Some("genome.fasta"));
        assert_eq!(job.job.created_at, created);
        assert_eq!(job.status, "expired");
        assert_eq!(job.outcome.as_deref(), Some("succeeded"));
        assert_eq!(job.origin.as_deref(), Some("bakta.computational.bio"));
        assert_eq!(job.submitted_at, Some(created));
        assert_eq!(job.started_at, Some(created));
        assert_eq!(job.finished_at, Some(created + Duration::minutes(5)));
        assert_eq!(job.runtime_seconds, Some(300.0));
        assert_eq!(job.queue_seconds, Some(0.0));
        assert_eq!(job.error_message, None);
        assert_eq!(job.genus.as_deref(), Some("Pantoea"));
        assert_eq!(job.species, None);
        assert_eq!(job.strain.as_deref(), Some("LMG 2665"));
        assert_eq!(job.config.unwrap()["completeGenome"], json!(true));
        assert_eq!(job.versions.bakta_version.as_deref(), Some("1.12.0"));
        assert_eq!(job.versions.baktfold_version, None);
    }

    #[test]
    fn test_backfill_job_of_a_recent_failed_staging_workflow() {
        let mut value = workflow_json(
            "bakta-baktfold-job-a",
            JOB,
            "bakta-baktfold-job-1.12.1-0.3.0",
            "2026-06-01T09:58:00Z",
            json!({"api-version": "v2", "workflow-kind": "bakta_baktfold",
                   "origin": "staging.bakta.computational.bio"}),
        );
        value["status"]["phase"] = json!("Failed");
        value["status"]["startedAt"] = json!("2026-06-01T10:00:00Z");
        value["status"]["message"] = json!("child failed");
        let now = Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap();
        let job = backfill_job(&parse_workflow(value).unwrap(), now).unwrap();
        assert_eq!(job.job.environment, "staging");
        assert_eq!(job.job.workflow_kind, "bakta_baktfold");
        assert_eq!(job.job.api_version, "v2");
        assert_eq!(job.status, "failed");
        assert_eq!(job.outcome.as_deref(), Some("failed"));
        assert_eq!(job.error_message.as_deref(), Some("child failed"));
        assert_eq!(job.queue_seconds, Some(120.0));
        assert_eq!(job.runtime_seconds, Some(300.0));
        assert_eq!(job.config.unwrap()["genus"], json!("Pantoea"));
        assert_eq!(job.versions.bakta_version.as_deref(), Some("1.12.1"));
        assert_eq!(job.versions.baktfold_version.as_deref(), Some("0.3.0"));
    }

    #[tokio::test]
    async fn test_list_workflows_reads_all_pages() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path, query_param, query_param_is_missing},
        };
        let server = MockServer::start().await;
        let url = "/apis/argoproj.io/v1alpha1/namespaces/bakta/workflows";
        let first = workflow_json(
            "wf-1",
            JOB,
            "bakta-job-1.12.0",
            "2026-06-01T10:00:00Z",
            json!({}),
        );
        let second = workflow_json(
            "wf-2",
            JOB,
            "bakta-job-1.12.0",
            "2026-06-02T10:00:00Z",
            json!({}),
        );
        Mock::given(method("GET"))
            .and(path(url))
            .and(header("authorization", "Bearer token"))
            .and(query_param("limit", "500"))
            .and(query_param_is_missing("continue"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metadata": {"continue": "next-page"},
                "items": [first, {"metadata": {}}],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(url))
            .and(query_param("continue", "next-page"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metadata": {"continue": ""},
                "items": [second],
            })))
            .mount(&server)
            .await;

        let client = KubeClient::new(reqwest::Client::new(), &server.uri(), "token");
        let (workflows, unreadable) = client.list_workflows("bakta").await.unwrap();
        let names: Vec<_> = workflows
            .iter()
            .map(|workflow| workflow.item.metadata.name.as_str())
            .collect();
        assert_eq!(names, ["wf-1", "wf-2"]);
        assert_eq!(unreadable, 1);
    }
}

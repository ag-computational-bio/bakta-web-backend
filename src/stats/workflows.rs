//! Live Bakta Web workflows in the `workflows` table, for the admin workflow
//! list and the bulk preview. The polling loop and admin actions write it.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{Row, types::Json};
use uuid::Uuid;

use super::StatsDb;
use crate::{
    argo::structs::{
        ArgoWorkflow, ArgoWorkflowMetadata, ArgoWorkflowSpec, ArgoWorkflowStatus,
        WorkflowTemplateRef,
    },
    bakta_handler::workflow_environment,
};

/// Rows of workflows that are gone from Argo are kept this long, so that a
/// poll that fetched its list before an admin delete does not add them back.
const GONE_ROWS_KEPT_MINUTES: i64 = 60;

impl StatsDb {
    /// Writes a batch of workflow states seen at `observed_at`. Rows are only
    /// written when they differ, and never with a state older than the one
    /// stored. A row of a gone workflow becomes live again.
    pub async fn upsert_workflows(
        &self,
        namespace: &str,
        observed_at: DateTime<Utc>,
        workflows: &[ArgoWorkflow],
    ) -> Result<()> {
        if workflows.is_empty() {
            return Ok(());
        }
        let text = |value: fn(&ArgoWorkflow) -> Option<String>| -> Vec<Option<String>> {
            workflows.iter().map(value).collect()
        };
        let time =
            |value: fn(&ArgoWorkflow) -> Option<DateTime<Utc>>| -> Vec<Option<DateTime<Utc>>> {
                workflows.iter().map(value).collect()
            };
        sqlx::query(
            "INSERT INTO workflows (namespace, name, job_id, environment, workflow_kind, template, phase,
                                    created_at, started_at, finished_at, message, suspended, origin, labels,
                                    gone_at, updated_at)
             SELECT $1, name, job_id, environment, workflow_kind, template, phase, created_at, started_at,
                 finished_at, message, suspended, origin, labels, NULL, $2
             FROM UNNEST($3::text[], $4::uuid[], $5::text[], $6::text[], $7::text[], $8::text[],
                         $9::timestamptz[], $10::timestamptz[], $11::timestamptz[], $12::text[],
                         $13::boolean[], $14::text[], $15::jsonb[])
                 AS polled(name, job_id, environment, workflow_kind, template, phase, created_at,
                           started_at, finished_at, message, suspended, origin, labels)
             ON CONFLICT (namespace, name) DO UPDATE SET job_id = EXCLUDED.job_id,
                 environment = EXCLUDED.environment, workflow_kind = EXCLUDED.workflow_kind,
                 template = EXCLUDED.template, phase = EXCLUDED.phase, created_at = EXCLUDED.created_at,
                 started_at = EXCLUDED.started_at, finished_at = EXCLUDED.finished_at,
                 message = EXCLUDED.message, suspended = EXCLUDED.suspended, origin = EXCLUDED.origin,
                 labels = EXCLUDED.labels, gone_at = NULL, updated_at = EXCLUDED.updated_at
             WHERE workflows.updated_at <= EXCLUDED.updated_at
                 AND (workflows.job_id, workflows.environment, workflows.workflow_kind, workflows.template,
                      workflows.phase, workflows.created_at, workflows.started_at, workflows.finished_at,
                      workflows.message, workflows.suspended, workflows.origin, workflows.labels,
                      workflows.gone_at)
                 IS DISTINCT FROM
                     (EXCLUDED.job_id, EXCLUDED.environment, EXCLUDED.workflow_kind, EXCLUDED.template,
                      EXCLUDED.phase, EXCLUDED.created_at, EXCLUDED.started_at, EXCLUDED.finished_at,
                      EXCLUDED.message, EXCLUDED.suspended, EXCLUDED.origin, EXCLUDED.labels,
                      NULL::timestamptz)",
        )
        .bind(namespace)
        .bind(observed_at)
        .bind(text(|workflow| Some(workflow.metadata.name.clone())))
        .bind(
            workflows
                .iter()
                .map(|workflow| label(workflow, "jobid").and_then(|id| id.parse::<Uuid>().ok()))
                .collect::<Vec<_>>(),
        )
        .bind(text(|workflow| {
            Some(workflow_environment(&workflow.metadata.labels).to_string())
        }))
        .bind(text(|workflow| label(workflow, "workflow-kind")))
        .bind(text(|workflow| {
            workflow
                .spec
                .workflow_template_ref
                .as_ref()
                .map(|template| template.name.clone())
        }))
        .bind(text(|workflow| Some(workflow.status.phase.clone())))
        .bind(time(|workflow| workflow.metadata.creation_timestamp))
        .bind(time(|workflow| workflow.status.started_at))
        .bind(time(|workflow| workflow.status.finished_at))
        .bind(text(|workflow| workflow.status.message.clone()))
        .bind(
            workflows
                .iter()
                .map(ArgoWorkflow::is_suspended)
                .collect::<Vec<_>>(),
        )
        .bind(text(|workflow| label(workflow, "origin")))
        .bind(
            workflows
                .iter()
                .map(|workflow| Json(workflow.metadata.labels.clone()))
                .collect::<Vec<_>>(),
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Takes `present` as the complete list of live workflows at
    /// `observed_at`: all others are marked as gone. Rows that are gone for
    /// a while are deleted.
    pub async fn remove_missing_workflows(
        &self,
        namespace: &str,
        observed_at: DateTime<Utc>,
        present: &[String],
    ) -> Result<()> {
        sqlx::query(
            "UPDATE workflows SET gone_at = $2, updated_at = $2
             WHERE namespace = $1 AND gone_at IS NULL AND updated_at <= $2 AND NOT (name = ANY($3))",
        )
        .bind(namespace)
        .bind(observed_at)
        .bind(present)
        .execute(self.pool())
        .await?;
        sqlx::query("DELETE FROM workflows WHERE namespace = $1 AND gone_at < $2")
            .bind(namespace)
            .bind(observed_at - chrono::Duration::minutes(GONE_ROWS_KEPT_MINUTES))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Live workflows of one environment, or of all with `None`, newest first.
    /// They have no nodes: `spec.suspend` is set for suspended workflows.
    pub async fn live_workflows(
        &self,
        namespace: &str,
        environment: Option<&str>,
    ) -> Result<Vec<ArgoWorkflow>> {
        let rows = sqlx::query(
            "SELECT name, template, phase, created_at, started_at, finished_at, message, suspended, labels
             FROM workflows
             WHERE namespace = $1 AND gone_at IS NULL AND ($2::text IS NULL OR environment = $2)
             ORDER BY created_at DESC NULLS LAST, name",
        )
        .bind(namespace)
        .bind(environment)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let Json(labels) = row.try_get::<Json<HashMap<String, String>>, _>("labels")?;
                Ok(ArgoWorkflow {
                    metadata: ArgoWorkflowMetadata {
                        name: row.try_get("name")?,
                        labels,
                        creation_timestamp: row.try_get("created_at")?,
                    },
                    spec: ArgoWorkflowSpec {
                        workflow_template_ref: row
                            .try_get::<Option<String>, _>("template")?
                            .map(|name| WorkflowTemplateRef { name }),
                        suspend: Some(row.try_get("suspended")?),
                    },
                    status: ArgoWorkflowStatus {
                        phase: row.try_get("phase")?,
                        started_at: row.try_get("started_at")?,
                        finished_at: row.try_get("finished_at")?,
                        message: row.try_get("message")?,
                        nodes: HashMap::new(),
                    },
                })
            })
            .collect()
    }
}

fn label(workflow: &ArgoWorkflow, name: &str) -> Option<String> {
    workflow.metadata.labels.get(name).cloned()
}

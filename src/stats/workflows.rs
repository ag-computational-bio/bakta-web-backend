//! Live Bakta Web workflows in the `workflows` table, for the admin workflow
//! list and the bulk preview. The workflow watch and admin actions write it.

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
                                    resource_version, gone_at, updated_at)
             SELECT $1, name, job_id, environment, workflow_kind, template, phase, created_at, started_at,
                 finished_at, message, suspended, origin, labels, resource_version, NULL, $2
             FROM UNNEST($3::text[], $4::uuid[], $5::text[], $6::text[], $7::text[], $8::text[],
                         $9::timestamptz[], $10::timestamptz[], $11::timestamptz[], $12::text[],
                         $13::boolean[], $14::text[], $15::jsonb[], $16::text[])
                 AS polled(name, job_id, environment, workflow_kind, template, phase, created_at,
                           started_at, finished_at, message, suspended, origin, labels, resource_version)
             ON CONFLICT (namespace, name) DO UPDATE SET job_id = EXCLUDED.job_id,
                 environment = EXCLUDED.environment, workflow_kind = EXCLUDED.workflow_kind,
                 template = EXCLUDED.template, phase = EXCLUDED.phase, created_at = EXCLUDED.created_at,
                 started_at = EXCLUDED.started_at, finished_at = EXCLUDED.finished_at,
                 message = EXCLUDED.message, suspended = EXCLUDED.suspended, origin = EXCLUDED.origin,
                 labels = EXCLUDED.labels,
                 resource_version = COALESCE(EXCLUDED.resource_version, workflows.resource_version),
                 gone_at = NULL, updated_at = EXCLUDED.updated_at
             WHERE workflows.updated_at <= EXCLUDED.updated_at
                 AND (workflows.job_id, workflows.environment, workflows.workflow_kind, workflows.template,
                      workflows.phase, workflows.created_at, workflows.started_at, workflows.finished_at,
                      workflows.message, workflows.suspended, workflows.origin, workflows.labels,
                      workflows.gone_at, workflows.resource_version)
                 IS DISTINCT FROM
                     (EXCLUDED.job_id, EXCLUDED.environment, EXCLUDED.workflow_kind, EXCLUDED.template,
                      EXCLUDED.phase, EXCLUDED.created_at, EXCLUDED.started_at, EXCLUDED.finished_at,
                      EXCLUDED.message, EXCLUDED.suspended, EXCLUDED.origin, EXCLUDED.labels,
                      NULL::timestamptz,
                      COALESCE(EXCLUDED.resource_version, workflows.resource_version))",
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
                .map(|workflow| {
                    // The job secret grants access to the results and is never stored here.
                    let mut labels = workflow.metadata.labels.clone();
                    labels.remove("secret");
                    Json(labels)
                })
                .collect::<Vec<_>>(),
        )
        .bind(text(|workflow| workflow.metadata.resource_version.clone()))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Resource versions of the stored live rows of the named workflows
    /// whose state is complete in the database: the workflow has no job, or
    /// its job record is deleted or expired, or has this workflow in the
    /// final state of the row (same status and finish time) with the secret
    /// hash, result kind and Argo uid, or has a newer workflow. Other workflows are missing, so the reconcile fetches them.
    pub async fn workflow_versions(
        &self,
        namespace: &str,
        names: &[String],
    ) -> Result<HashMap<String, Option<String>>> {
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT workflows.name, workflows.resource_version
             FROM workflows
             LEFT JOIN jobs ON jobs.id = workflows.job_id
             LEFT JOIN workflows current
                 ON current.namespace = workflows.namespace AND current.name = jobs.workflow_name
             WHERE workflows.namespace = $1 AND workflows.name = ANY($2) AND workflows.gone_at IS NULL
                 AND (workflows.job_id IS NULL
                      OR jobs.status IN ('deleted', 'expired')
                      OR (jobs.workflow_name = workflows.name
                          AND jobs.status = CASE workflows.phase WHEN 'Succeeded' THEN 'succeeded'
                              WHEN 'Failed' THEN 'failed' WHEN 'Error' THEN 'error' END
                          AND jobs.finished_at IS NOT DISTINCT FROM workflows.finished_at
                          AND jobs.secret_hash IS NOT NULL AND jobs.result_kind IS NOT NULL
                          AND jobs.argo_uid IS NOT NULL)
                      OR (jobs.workflow_name <> workflows.name
                          AND current.created_at >= workflows.created_at))",
        )
        .bind(namespace)
        .bind(names)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().collect())
    }

    /// Marks the named workflows as gone from Argo at `observed_at`.
    pub async fn remove_workflows(
        &self,
        namespace: &str,
        observed_at: DateTime<Utc>,
        names: &[String],
    ) -> Result<()> {
        sqlx::query(
            "UPDATE workflows SET gone_at = $2, updated_at = $2
             WHERE namespace = $1 AND name = ANY($3) AND gone_at IS NULL AND updated_at <= $2",
        )
        .bind(namespace)
        .bind(observed_at)
        .bind(names)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Marks all workflows of a job as gone from Argo at `observed_at`.
    pub async fn remove_job_workflows(
        &self,
        namespace: &str,
        observed_at: DateTime<Utc>,
        job_id: Uuid,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE workflows SET gone_at = $2, updated_at = $2
             WHERE namespace = $1 AND job_id = $3 AND gone_at IS NULL AND updated_at <= $2",
        )
        .bind(namespace)
        .bind(observed_at)
        .bind(job_id)
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
        self.prune_gone_workflows(namespace, observed_at).await
    }

    /// Deletes the rows of workflows that are gone for a while at `now`.
    pub async fn prune_gone_workflows(&self, namespace: &str, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("DELETE FROM workflows WHERE namespace = $1 AND gone_at < $2")
            .bind(namespace)
            .bind(now - chrono::Duration::minutes(GONE_ROWS_KEPT_MINUTES))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Name and phase of the current workflow of each job whose recorded
    /// workflow has a live row.
    pub async fn job_workflows(
        &self,
        namespace: &str,
    ) -> Result<HashMap<Uuid, crate::bakta_handler::LiveWorkflow>> {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT workflows.job_id, workflows.name, workflows.phase
             FROM workflows JOIN jobs ON jobs.id = workflows.job_id AND jobs.workflow_name = workflows.name
             WHERE workflows.namespace = $1 AND workflows.gone_at IS NULL",
        )
        .bind(namespace)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, phase)| (id, crate::bakta_handler::LiveWorkflow { name, phase }))
            .collect())
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
                        resource_version: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        argo::structs::SimpleStatus,
        stats::{StatsRecorder, test_db::test_db},
        test_support::test_handler_with_stats,
    };
    use serde_json::{Value, json};
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    fn listed(name: &str, phase: &str, environment: &str) -> Value {
        json!({
            "metadata": {"name": name, "uid": Uuid::new_v4(), "creationTimestamp": "2026-10-01T10:00:00Z",
                         "labels": {"bakta-environment": environment, "jobid": Uuid::new_v4().to_string(),
                                    "workflow-kind": "bakta", "origin": "bakta.computational.bio"}},
            "spec": {"workflowTemplateRef": {"name": "bakta-job-1.9.0"}},
            "status": {"phase": phase, "startedAt": "2026-10-01T10:00:05Z", "message": "msg"}
        })
    }

    /// Writes a complete list of workflows like the workflow watch after a list.
    async fn record_list(
        state: &crate::bakta_handler::StateHandler,
        items: &[SimpleStatus],
        at: DateTime<Utc>,
    ) {
        assert!(state.upsert_workflows(items, at).await);
        let names: Vec<String> = items
            .iter()
            .map(|item| item.metadata.name.clone())
            .collect();
        state.remove_missing_workflows(&names, at).await;
    }

    fn workflow(value: &Value) -> ArgoWorkflow {
        serde_json::from_value(value.clone()).unwrap()
    }

    async fn updated_at(db: &StatsDb, name: &str) -> DateTime<Utc> {
        sqlx::query_scalar("SELECT updated_at FROM workflows WHERE name = $1")
            .bind(name)
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    fn names(workflows: &[ArgoWorkflow]) -> Vec<&str> {
        workflows
            .iter()
            .map(|workflow| workflow.metadata.name.as_str())
            .collect()
    }

    #[tokio::test]
    async fn upsert_only_writes_changed_and_newer_states() {
        let Some(db) = test_db("upsert_only_writes_changed_and_newer_states").await else {
            return;
        };
        // Postgres stores microseconds.
        let t0 = chrono::SubsecRound::trunc_subsecs(Utc::now(), 6) - chrono::Duration::minutes(5);
        let t1 = t0 + chrono::Duration::seconds(15);
        let failed = workflow(&listed("wf-1", "Failed", "production"));
        let mut with_secret = failed.clone();
        with_secret
            .metadata
            .labels
            .insert("secret".to_string(), "not-a-real-secret".to_string());
        db.upsert_workflows("bakta", t0, std::slice::from_ref(&with_secret))
            .await
            .unwrap();
        let stored = db
            .live_workflows("bakta", Some("production"))
            .await
            .unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].metadata.labels, failed.metadata.labels);
        assert_eq!(stored[0].status.phase, "Failed");
        assert_eq!(
            stored[0].spec.workflow_template_ref,
            failed.spec.workflow_template_ref
        );
        assert!(!stored[0].is_suspended());

        // The same state is not written again.
        db.upsert_workflows("bakta", t1, std::slice::from_ref(&failed))
            .await
            .unwrap();
        assert_eq!(updated_at(&db, "wf-1").await, t0);

        // A changed state is written, an older one is ignored.
        let mut running = failed.clone();
        running.status.phase = "Running".to_string();
        db.upsert_workflows("bakta", t1, std::slice::from_ref(&running))
            .await
            .unwrap();
        db.upsert_workflows("bakta", t0, std::slice::from_ref(&failed))
            .await
            .unwrap();
        let stored = db.live_workflows("bakta", None).await.unwrap();
        assert_eq!(stored[0].status.phase, "Running");
        assert_eq!(updated_at(&db, "wf-1").await, t1);
        assert!(
            db.live_workflows("bakta", Some("staging"))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(db.live_workflows("other", None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn gone_workflows_are_hidden_until_they_are_listed_again() {
        let Some(db) = test_db("gone_workflows_are_hidden_until_they_are_listed_again").await
        else {
            return;
        };
        let t0 = Utc::now() - chrono::Duration::hours(3);
        let all = [
            workflow(&listed("wf-a", "Failed", "production")),
            workflow(&listed("wf-b", "Failed", "production")),
            workflow(&listed("wf-c", "Failed", "production")),
        ];
        db.upsert_workflows("bakta", t0, &all).await.unwrap();
        db.upsert_workflows("other", t0, &all[..1]).await.unwrap();
        // A poll that no longer lists wf-b, and a delete of wf-c.
        let t1 = t0 + chrono::Duration::seconds(15);
        db.remove_missing_workflows("bakta", t1, &["wf-a".to_string(), "wf-c".to_string()])
            .await
            .unwrap();
        db.remove_workflows("bakta", t1, &["wf-c".to_string()])
            .await
            .unwrap();
        let live = db.live_workflows("bakta", None).await.unwrap();
        assert_eq!(names(&live), ["wf-a"]);
        // Other namespaces are not touched.
        assert_eq!(
            names(&db.live_workflows("other", None).await.unwrap()),
            ["wf-a"]
        );
        // A poll fetched before the delete does not add wf-c back.
        db.upsert_workflows("bakta", t0 + chrono::Duration::seconds(10), &all[2..])
            .await
            .unwrap();
        assert_eq!(
            names(&db.live_workflows("bakta", None).await.unwrap()),
            ["wf-a"]
        );
        // A later list with wf-b makes it live again.
        db.upsert_workflows("bakta", t1 + chrono::Duration::seconds(15), &all[1..2])
            .await
            .unwrap();
        let mut live = names(&db.live_workflows("bakta", None).await.unwrap())
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        live.sort();
        assert_eq!(live, ["wf-a", "wf-b"]);
        // Rows that are gone for long are deleted.
        db.remove_missing_workflows("bakta", t1 + chrono::Duration::hours(2), &[])
            .await
            .unwrap();
        db.remove_missing_workflows("bakta", t1 + chrono::Duration::hours(4), &[])
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM workflows WHERE namespace = 'bakta'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn a_full_list_records_bakta_web_workflows_with_their_suspend_state() {
        let Some(db) = test_db("a_full_list_records_bakta_web_workflows").await else {
            return;
        };
        // The suspend state comes from `spec.suspend`, so Argo is not asked.
        let argo = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&argo)
            .await;
        let (stats, _events) = StatsRecorder::capturing_with_db((*db).clone());
        let handler = test_handler_with_stats(&argo.uri(), "https://s3.example.com", stats);
        let mut paused = listed("paused", "Running", "production");
        paused["spec"]["suspend"] = json!(true);
        let mut foreign = listed("foreign", "Failed", "production");
        foreign["metadata"]["labels"] = json!({"app": "other"});
        let polled: Vec<SimpleStatus> = [
            paused,
            listed("running", "Running", "staging"),
            listed("done", "Succeeded", "production"),
            foreign,
        ]
        .iter()
        .map(|item| serde_json::from_value(item.clone()).unwrap())
        .collect();
        record_list(&handler.state_handler, &polled, Utc::now()).await;

        let live = db.live_workflows("bakta", None).await.unwrap();
        let mut all = names(&live);
        all.sort();
        assert_eq!(all, ["done", "paused", "running"]);
        let suspended: Vec<_> = live
            .iter()
            .filter(|workflow| workflow.is_suspended())
            .map(|workflow| workflow.metadata.name.as_str())
            .collect();
        assert_eq!(suspended, ["paused"]);
        assert_eq!(
            names(&db.live_workflows("bakta", Some("staging")).await.unwrap()),
            ["running"]
        );

        // The next full list no longer has `done`.
        record_list(&handler.state_handler, &polled[..2], Utc::now()).await;
        let mut all = names(&db.live_workflows("bakta", None).await.unwrap())
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        all.sort();
        assert_eq!(all, ["paused", "running"]);
    }

    #[tokio::test]
    async fn changed_workflows_are_written_and_deleted_ones_marked_gone() {
        let Some(db) = test_db("changed_workflows_are_written").await else {
            return;
        };
        let (stats, _events) = StatsRecorder::capturing_with_db((*db).clone());
        let handler =
            test_handler_with_stats("http://127.0.0.1:1", "https://s3.example.com", stats);
        let state = &handler.state_handler;
        let item = |name: &str, phase: &str| -> SimpleStatus {
            serde_json::from_value(listed(name, phase, "production")).unwrap()
        };
        let t0 = Utc::now() - chrono::Duration::minutes(5);
        record_list(state, &[item("a", "Running"), item("b", "Running")], t0).await;
        // Only the changed workflow is written. Rows of others are kept.
        state
            .record_changed_workflows(
                &[item("a", "Succeeded")],
                &[],
                t0 + chrono::Duration::seconds(1),
            )
            .await;
        let live = db.live_workflows("bakta", None).await.unwrap();
        let phase = |name: &str| {
            live.iter()
                .find(|workflow| workflow.metadata.name == name)
                .map(|workflow| workflow.status.phase.clone())
        };
        assert_eq!(phase("a").as_deref(), Some("Succeeded"));
        assert_eq!(phase("b").as_deref(), Some("Running"));
        // A deleted workflow is marked gone.
        state
            .record_changed_workflows(&[], &["b".to_string()], t0 + chrono::Duration::seconds(2))
            .await;
        assert_eq!(
            names(&db.live_workflows("bakta", None).await.unwrap()),
            ["a"]
        );
        // Its rows are deleted an hour later.
        db.prune_gone_workflows("bakta", t0 + chrono::Duration::minutes(59))
            .await
            .unwrap();
        let count = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workflows WHERE name = 'b'")
                .fetch_one(db.pool())
                .await
                .unwrap()
        };
        assert_eq!(count().await, 1);
        db.prune_gone_workflows("bakta", t0 + chrono::Duration::minutes(62))
            .await
            .unwrap();
        assert_eq!(count().await, 0);
    }
}

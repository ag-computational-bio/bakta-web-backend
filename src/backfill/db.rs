//! Writing backfilled jobs: missing records are inserted, existing records
//! only get the columns that are still empty.

use std::{collections::HashSet, sync::LazyLock};

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::stats::{JobVersions, NewJob, StatsDb};

/// Everything the backfill knows about one job.
#[derive(Debug, Clone, PartialEq)]
pub struct BackfillJob {
    pub job: NewJob,
    /// Only written to new records.
    pub status: String,
    pub outcome: Option<String>,
    pub origin: Option<String>,
    pub workflow_name: String,
    pub submitted_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub runtime_seconds: Option<f64>,
    pub queue_seconds: Option<f64>,
    pub error_message: Option<String>,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub config: Option<serde_json::Value>,
    pub versions: JobVersions,
    pub sequence_count: Option<i32>,
    pub total_length: Option<i64>,
    pub argo_uid: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Inserted,
    Filled,
    Unchanged,
}

/// Columns that an existing record gets when they are NULL. The ID,
/// environment, kind, API version, status and creation time are never changed.
const FILL_COLUMNS: [&str; 23] = [
    "outcome",
    "origin",
    "name",
    "workflow_name",
    "submitted_at",
    "started_at",
    "finished_at",
    "runtime_seconds",
    "queue_seconds",
    "error_message",
    "genus",
    "species",
    "strain",
    "config",
    "bakta_version",
    "bakta_db_version",
    "baktfold_version",
    "baktfold_db_version",
    "sequence_count",
    "total_length",
    "secret_hash",
    "result_kind",
    "argo_uid",
];

/// Columns that belong to one workflow attempt. They are only filled when the
/// record points to the same workflow, so a resubmit that started after the
/// backfill read the workflows does not get the values of the old attempt.
const ATTEMPT_COLUMNS: [&str; 8] = [
    "argo_uid",
    "outcome",
    "submitted_at",
    "started_at",
    "finished_at",
    "runtime_seconds",
    "queue_seconds",
    "error_message",
];

static FILL_SQL: LazyLock<String> = LazyLock::new(|| {
    let fill = |column: &str| {
        if ATTEMPT_COLUMNS.contains(&column) {
            format!(
                "CASE WHEN jobs.workflow_name IS DISTINCT FROM EXCLUDED.workflow_name
                     AND jobs.workflow_name IS NOT NULL
                     THEN jobs.{column} ELSE COALESCE(jobs.{column}, EXCLUDED.{column}) END"
            )
        } else {
            format!("COALESCE(jobs.{column}, EXCLUDED.{column})")
        }
    };
    let set: Vec<String> = FILL_COLUMNS
        .iter()
        .map(|column| format!("{column} = {}", fill(column)))
        .collect();
    let changed: Vec<String> = FILL_COLUMNS
        .iter()
        .map(|column| format!("(jobs.{column} IS DISTINCT FROM {})", fill(column)))
        .collect();
    let placeholders: Vec<String> = (1..=FILL_COLUMNS.len() + 6)
        .map(|index| format!("${index}"))
        .collect();
    format!(
        "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, created_at, {}, updated_at)
         VALUES ({}, now())
         ON CONFLICT (id) DO UPDATE SET {}, updated_at = now()
         WHERE {}
         RETURNING (xmax = 0) AS inserted",
        FILL_COLUMNS.join(", "),
        placeholders.join(", "),
        set.join(", "),
        changed.join(" OR "),
    )
});

impl StatsDb {
    /// Inserts the job, or fills the empty columns of its existing record.
    pub async fn backfill_job(&self, job: &BackfillJob) -> Result<Fill> {
        let inserted: Option<bool> = sqlx::query_scalar(FILL_SQL.as_str())
            .bind(job.job.id)
            .bind(&job.job.environment)
            .bind(&job.job.workflow_kind)
            .bind(&job.job.api_version)
            .bind(&job.status)
            .bind(job.job.created_at)
            .bind(&job.outcome)
            .bind(&job.origin)
            .bind(&job.job.name)
            .bind(&job.workflow_name)
            .bind(job.submitted_at)
            .bind(job.started_at)
            .bind(job.finished_at)
            .bind(job.runtime_seconds)
            .bind(job.queue_seconds)
            .bind(&job.error_message)
            .bind(&job.genus)
            .bind(&job.species)
            .bind(&job.strain)
            .bind(&job.config)
            .bind(&job.versions.bakta_version)
            .bind(&job.versions.bakta_db_version)
            .bind(&job.versions.baktfold_version)
            .bind(&job.versions.baktfold_db_version)
            .bind(job.sequence_count)
            .bind(job.total_length)
            .bind(&job.job.secret_hash)
            .bind(&job.job.result_kind)
            .bind(job.argo_uid)
            .fetch_optional(self.pool())
            .await?;
        Ok(match inserted {
            Some(true) => Fill::Inserted,
            Some(false) => Fill::Filled,
            None => Fill::Unchanged,
        })
    }

    /// Jobs whose genome size is recorded, so their input is not read again.
    pub async fn jobs_with_size(&self) -> Result<HashSet<Uuid>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE total_length IS NOT NULL AND sequence_count IS NOT NULL",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(ids.into_iter().collect())
    }

    /// Jobs whose summary values are all recorded, so S3 is not read again.
    pub async fn jobs_with_summary(&self) -> Result<HashSet<Uuid>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE total_length IS NOT NULL
                 AND sequence_count IS NOT NULL AND bakta_db_version IS NOT NULL",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(ids.into_iter().collect())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stats::{
        StatsEvent, WorkflowState,
        test_db::{test_db, unique_environment},
    };
    use chrono::Duration;
    use serde_json::json;
    use sqlx::Row;

    pub(crate) fn backfill_job(environment: &str, created_at: DateTime<Utc>) -> BackfillJob {
        BackfillJob {
            job: NewJob {
                id: Uuid::new_v4(),
                environment: environment.to_string(),
                workflow_kind: "bakta".to_string(),
                api_version: "v1".to_string(),
                name: Some("genome.fasta".to_string()),
                created_at,
                secret_hash: None,
                result_kind: None,
            },
            status: "expired".to_string(),
            outcome: Some("succeeded".to_string()),
            origin: Some("bakta.computational.bio".to_string()),
            workflow_name: "bakta-job-x".to_string(),
            submitted_at: Some(created_at),
            started_at: Some(created_at + Duration::seconds(5)),
            finished_at: Some(created_at + Duration::seconds(65)),
            runtime_seconds: Some(60.0),
            queue_seconds: Some(5.0),
            error_message: None,
            genus: Some("Escherichia".to_string()),
            species: Some("coli".to_string()),
            strain: None,
            config: Some(json!({"completeGenome": true})),
            versions: JobVersions {
                bakta_version: Some("1.12.0".to_string()),
                bakta_db_version: Some("6.0".to_string()),
                ..Default::default()
            },
            sequence_count: Some(2),
            total_length: Some(5_000_000),
            argo_uid: None,
        }
    }

    async fn row(db: &StatsDb, id: Uuid) -> sqlx::postgres::PgRow {
        sqlx::query("SELECT * FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_backfill_inserts_missing_jobs_once() {
        let Some(db) = test_db("test_backfill_inserts_missing_jobs_once").await else {
            return;
        };
        let job = backfill_job(&unique_environment(), Utc::now() - Duration::days(90));

        assert_eq!(db.backfill_job(&job).await.unwrap(), Fill::Inserted);
        let record = row(&db, job.job.id).await;
        assert_eq!(record.get::<String, _>("status"), "expired");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").unwrap(),
            "succeeded"
        );
        assert_eq!(record.get::<String, _>("environment"), job.job.environment);
        assert_eq!(
            record.get::<Option<String>, _>("genus").unwrap(),
            "Escherichia"
        );
        assert_eq!(record.get::<Option<String>, _>("strain"), None);
        assert_eq!(
            record.get::<Option<i64>, _>("total_length"),
            Some(5_000_000)
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), Some(60.0));
        assert_eq!(
            record.get::<Option<serde_json::Value>, _>("config"),
            job.config
        );
        assert_eq!(
            record.get::<Option<String>, _>("bakta_db_version").unwrap(),
            "6.0"
        );
        assert!(db.jobs_with_summary().await.unwrap().contains(&job.job.id));

        // A second run changes nothing.
        let updated_at = record.get::<DateTime<Utc>, _>("updated_at");
        assert_eq!(db.backfill_job(&job).await.unwrap(), Fill::Unchanged);
        assert_eq!(
            row(&db, job.job.id)
                .await
                .get::<DateTime<Utc>, _>("updated_at"),
            updated_at
        );
    }

    #[tokio::test]
    async fn test_backfill_only_fills_empty_columns_of_existing_jobs() {
        let Some(db) = test_db("test_backfill_only_fills_empty_columns_of_existing_jobs").await
        else {
            return;
        };
        let environment = unique_environment();
        let created_at = Utc::now() - Duration::hours(2);
        let mut job = backfill_job(&environment, created_at);
        // The poll recorded the job with its own values.
        let polled = WorkflowState {
            job: NewJob {
                workflow_kind: "bakta_baktfold".to_string(),
                api_version: "v2".to_string(),
                name: Some("polled.fasta".to_string()),
                ..job.job.clone()
            },
            workflow_name: "polled-workflow".to_string(),
            status: "failed",
            created_at: Some(created_at),
            started_at: Some(created_at + Duration::seconds(10)),
            finished_at: Some(created_at + Duration::seconds(20)),
            error_message: Some("polled error".to_string()),
            origin: Some("staging.bakta.computational.bio".to_string()),
            argo_uid: None,
        };
        db.apply(&StatsEvent::Polled(vec![polled])).await.unwrap();
        job.job.environment = "other".to_string();
        job.error_message = Some("backfill error".to_string());

        assert_eq!(db.backfill_job(&job).await.unwrap(), Fill::Filled);
        let record = row(&db, job.job.id).await;
        // Values that were already recorded are kept.
        assert_eq!(record.get::<String, _>("status"), "failed");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").unwrap(),
            "failed"
        );
        assert_eq!(record.get::<String, _>("environment"), environment);
        assert_eq!(record.get::<String, _>("workflow_kind"), "bakta_baktfold");
        assert_eq!(record.get::<String, _>("api_version"), "v2");
        assert_eq!(
            record.get::<Option<String>, _>("name").unwrap(),
            "polled.fasta"
        );
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").unwrap(),
            "polled-workflow"
        );
        assert_eq!(
            record.get::<Option<String>, _>("origin").unwrap(),
            "staging.bakta.computational.bio"
        );
        assert_eq!(
            record.get::<Option<String>, _>("error_message").unwrap(),
            "polled error"
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), Some(10.0));
        // Empty columns are filled.
        assert_eq!(
            record.get::<Option<String>, _>("genus").unwrap(),
            "Escherichia"
        );
        assert_eq!(record.get::<Option<i32>, _>("sequence_count"), Some(2));
        assert_eq!(
            record.get::<Option<String>, _>("bakta_version").unwrap(),
            "1.12.0"
        );
        assert_eq!(
            record.get::<Option<serde_json::Value>, _>("config"),
            job.config
        );

        // A second run changes nothing.
        assert_eq!(db.backfill_job(&job).await.unwrap(), Fill::Unchanged);
    }

    #[tokio::test]
    async fn test_backfill_keeps_the_cleared_fields_of_a_newer_attempt() {
        let Some(db) = test_db("test_backfill_keeps_the_cleared_fields_of_a_newer_attempt").await
        else {
            return;
        };
        let mut job = backfill_job(&unique_environment(), Utc::now() - Duration::hours(2));
        job.argo_uid = Some(Uuid::new_v4());
        // A resubmit after the backfill read the workflows: the record points
        // to the new workflow and its attempt fields are empty.
        sqlx::query(
            "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, created_at,
                               workflow_name, updated_at)
             VALUES ($1, $2, 'bakta', 'v1', 'submitted', $3, 'bakta-job-new', now())",
        )
        .bind(job.job.id)
        .bind(&job.job.environment)
        .bind(job.job.created_at)
        .execute(db.pool())
        .await
        .unwrap();

        assert_eq!(db.backfill_job(&job).await.unwrap(), Fill::Filled);
        let record = row(&db, job.job.id).await;
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").unwrap(),
            "bakta-job-new"
        );
        let filled: i32 = sqlx::query_scalar(
            "SELECT num_nonnulls(outcome, submitted_at, started_at, finished_at,
                                 runtime_seconds, queue_seconds, error_message, argo_uid)
             FROM jobs WHERE id = $1",
        )
        .bind(job.job.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(filled, 0);
        assert_eq!(
            record.get::<Option<String>, _>("genus").unwrap(),
            "Escherichia"
        );
        assert_eq!(record.get::<Option<i32>, _>("sequence_count"), Some(2));
    }
}

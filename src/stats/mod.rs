//! Job statistics database: schema migrations, job lifecycle recording and
//! the queries used by the admin API.

pub mod queries;

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Upper limit for a single database write. Recording must never stall.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Number of events that may wait for the database before new ones are dropped.
const EVENT_QUEUE_SIZE: usize = 10_000;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct StatsDb {
    pool: PgPool,
}

/// Job versions written to the statistics record when a job is submitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobVersions {
    pub bakta_version: Option<String>,
    pub bakta_db_version: Option<String>,
    pub baktfold_version: Option<String>,
    pub baktfold_db_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub id: Uuid,
    pub environment: String,
    pub workflow_kind: String,
    pub api_version: String,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubmittedJob {
    pub job: NewJob,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub sequence_count: Option<i32>,
    pub total_length: Option<i64>,
    pub config: Option<serde_json::Value>,
    pub versions: JobVersions,
    pub workflow_name: String,
    pub submitted_at: DateTime<Utc>,
}

/// Workflow progress seen by the polling loop. `status` is `running`,
/// `succeeded`, `failed` or `error`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusUpdate {
    pub job: NewJob,
    pub workflow_name: String,
    pub status: &'static str,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatsEvent {
    Created(NewJob),
    Submitted(Box<SubmittedJob>),
    Status(StatusUpdate),
    Retried {
        id: Uuid,
    },
    Resubmitted {
        id: Uuid,
        workflow_name: String,
        submitted_at: DateTime<Utc>,
    },
    Deleted {
        id: Uuid,
    },
    Expired {
        id: Uuid,
    },
}

pub fn is_terminal_status(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "error")
}

impl StatsDb {
    /// Creates a lazy pool. No connection is opened until the first query.
    pub fn connect_lazy(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(WRITE_TIMEOUT)
            .connect_lazy(url)?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn insert_job(&self, job: &NewJob, status: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(job.id)
        .bind(&job.environment)
        .bind(&job.workflow_kind)
        .bind(&job.api_version)
        .bind(status)
        .bind(&job.name)
        .bind(job.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn apply(&self, event: &StatsEvent) -> Result<()> {
        match event {
            StatsEvent::Created(job) => self.insert_job(job, "created").await?,
            StatsEvent::Submitted(submitted) => {
                let job = &submitted.job;
                self.insert_job(job, "submitted").await?;
                sqlx::query(
                    "UPDATE jobs SET status = 'submitted', workflow_kind = $2, api_version = $3,
                         name = COALESCE(name, $4), genus = $5, species = $6, strain = $7,
                         sequence_count = $8, total_length = $9, config = $10,
                         bakta_version = $11, bakta_db_version = $12,
                         baktfold_version = $13, baktfold_db_version = $14,
                         workflow_name = $15, submitted_at = $16, updated_at = now()
                     WHERE id = $1",
                )
                .bind(job.id)
                .bind(&job.workflow_kind)
                .bind(&job.api_version)
                .bind(&job.name)
                .bind(&submitted.genus)
                .bind(&submitted.species)
                .bind(&submitted.strain)
                .bind(submitted.sequence_count)
                .bind(submitted.total_length)
                .bind(&submitted.config)
                .bind(&submitted.versions.bakta_version)
                .bind(&submitted.versions.bakta_db_version)
                .bind(&submitted.versions.baktfold_version)
                .bind(&submitted.versions.baktfold_db_version)
                .bind(&submitted.workflow_name)
                .bind(submitted.submitted_at)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Status(update) => {
                self.insert_job(&update.job, "submitted").await?;
                sqlx::query(
                    "UPDATE jobs SET status = $2, workflow_name = $3,
                         submitted_at = COALESCE(submitted_at, $4),
                         started_at = COALESCE(started_at, $4),
                         finished_at = $5,
                         outcome = CASE WHEN $6 THEN $2 ELSE outcome END,
                         error_message = $7,
                         runtime_seconds = CASE WHEN $5 IS NULL THEN NULL
                             ELSE GREATEST(0, EXTRACT(EPOCH FROM ($5 - COALESCE(started_at, $4))))::double precision END,
                         queue_seconds = GREATEST(0, EXTRACT(EPOCH FROM (COALESCE(started_at, $4) - COALESCE(submitted_at, $4))))::double precision,
                         updated_at = now()
                     WHERE id = $1 AND status NOT IN ('deleted', 'expired')",
                )
                .bind(update.job.id)
                .bind(update.status)
                .bind(&update.workflow_name)
                .bind(update.started_at)
                .bind(update.finished_at)
                .bind(is_terminal_status(update.status))
                .bind(&update.error_message)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Retried { id } => {
                sqlx::query(
                    "UPDATE jobs SET status = 'running', outcome = NULL, finished_at = NULL,
                         runtime_seconds = NULL, error_message = NULL, updated_at = now()
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Resubmitted {
                id,
                workflow_name,
                submitted_at,
            } => {
                sqlx::query(
                    "UPDATE jobs SET status = 'submitted', outcome = NULL, workflow_name = $2,
                         submitted_at = $3, started_at = NULL, finished_at = NULL,
                         runtime_seconds = NULL, queue_seconds = NULL, error_message = NULL,
                         updated_at = now()
                     WHERE id = $1",
                )
                .bind(id)
                .bind(workflow_name)
                .bind(submitted_at)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Deleted { id } => {
                sqlx::query("UPDATE jobs SET status = 'deleted', updated_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            StatsEvent::Expired { id } => {
                sqlx::query(
                    "UPDATE jobs SET status = 'expired', updated_at = now()
                     WHERE id = $1 AND status <> 'deleted'",
                )
                .bind(id)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }
}

/// Sends job lifecycle events to the statistics database in the background.
/// Events are written in order by a single worker. When recording is
/// disabled, every call is a no-op.
#[derive(Clone, Default)]
pub struct StatsRecorder {
    sender: Option<mpsc::Sender<StatsEvent>>,
    db: Option<StatsDb>,
    pub versions: JobVersions,
}

impl StatsRecorder {
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Starts the background writer. Must be called inside a Tokio runtime.
    pub fn new(db: StatsDb, versions: JobVersions) -> Self {
        let (sender, mut receiver) = mpsc::channel::<StatsEvent>(EVENT_QUEUE_SIZE);
        let worker_db = db.clone();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                match tokio::time::timeout(WRITE_TIMEOUT, worker_db.apply(&event)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, ?event, "stats_write_failed");
                    }
                    Err(_) => tracing::warn!(?event, "stats_write_timed_out"),
                }
            }
        });
        Self {
            sender: Some(sender),
            db: Some(db),
            versions,
        }
    }

    pub fn db(&self) -> Option<&StatsDb> {
        self.db.as_ref()
    }

    pub fn record(&self, event: StatsEvent) {
        let Some(sender) = &self.sender else {
            return;
        };
        if let Err(error) = sender.try_send(event) {
            tracing::warn!(%error, "stats_event_dropped");
        }
    }
}

#[cfg(test)]
pub mod test_db {
    use super::StatsDb;

    /// Returns a migrated database from `TEST_DATABASE_URL`, or `None` with a
    /// message when the variable is unset.
    pub async fn test_db(test_name: &str) -> Option<StatsDb> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("skipping {test_name}: TEST_DATABASE_URL is not set");
            return None;
        };
        let db = StatsDb::connect_lazy(&url).expect("TEST_DATABASE_URL should be valid");
        db.migrate().await.expect("migrations should run");
        Some(db)
    }

    /// Unique environment name, so tests can share one database.
    pub fn unique_environment() -> String {
        format!("test-{}", uuid::Uuid::new_v4().simple())
    }
}

#[cfg(test)]
mod tests {
    use super::{test_db::*, *};
    use chrono::Duration as ChronoDuration;
    use sqlx::Row;

    fn new_job(environment: &str, created_at: DateTime<Utc>) -> NewJob {
        NewJob {
            id: Uuid::new_v4(),
            environment: environment.to_string(),
            workflow_kind: "bakta".to_string(),
            api_version: "v2".to_string(),
            name: Some("sample".to_string()),
            created_at,
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
    async fn test_job_lifecycle_is_recorded() {
        let Some(db) = test_db("test_job_lifecycle_is_recorded").await else {
            return;
        };
        let environment = unique_environment();
        let t0 = Utc::now() - ChronoDuration::minutes(30);
        let job = new_job(&environment, t0);
        let id = job.id;

        db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
        assert_eq!(row(&db, id).await.get::<String, _>("status"), "created");

        db.apply(&StatsEvent::Submitted(Box::new(SubmittedJob {
            job: job.clone(),
            genus: Some("Escherichia".to_string()),
            species: Some("coli".to_string()),
            strain: None,
            sequence_count: Some(3),
            total_length: Some(5_594_605),
            config: Some(serde_json::json!({"complete_genome": true})),
            versions: JobVersions {
                bakta_version: Some("1.12.2".to_string()),
                ..Default::default()
            },
            workflow_name: "wf-1".to_string(),
            submitted_at: t0 + ChronoDuration::seconds(10),
        })))
        .await
        .unwrap();

        let running = StatusUpdate {
            job: job.clone(),
            workflow_name: "wf-1".to_string(),
            status: "running",
            started_at: t0 + ChronoDuration::seconds(20),
            finished_at: None,
            error_message: None,
        };
        db.apply(&StatsEvent::Status(running.clone()))
            .await
            .unwrap();
        db.apply(&StatsEvent::Status(StatusUpdate {
            status: "succeeded",
            // The first observed running time wins over a later start time.
            started_at: t0 + ChronoDuration::seconds(25),
            finished_at: Some(t0 + ChronoDuration::seconds(120)),
            ..running
        }))
        .await
        .unwrap();

        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "succeeded");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").unwrap(),
            "succeeded"
        );
        assert_eq!(
            record.get::<Option<i64>, _>("total_length"),
            Some(5_594_605)
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), Some(100.0));
        assert_eq!(record.get::<Option<f64>, _>("queue_seconds"), Some(10.0));
        assert_eq!(
            record.get::<Option<String>, _>("bakta_version").as_deref(),
            Some("1.12.2")
        );

        db.apply(&StatsEvent::Expired { id }).await.unwrap();
        db.apply(&StatsEvent::Status(StatusUpdate {
            job: job.clone(),
            workflow_name: "wf-1".to_string(),
            status: "failed",
            started_at: t0,
            finished_at: Some(t0),
            error_message: None,
        }))
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "expired");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").unwrap(),
            "succeeded"
        );

        db.apply(&StatsEvent::Deleted { id }).await.unwrap();
        db.apply(&StatsEvent::Expired { id }).await.unwrap();
        assert_eq!(row(&db, id).await.get::<String, _>("status"), "deleted");
    }

    #[tokio::test]
    async fn test_status_update_creates_missing_rows_and_resubmit_resets_times() {
        let Some(db) = test_db("test_status_update_creates_missing_rows").await else {
            return;
        };
        let environment = unique_environment();
        let t0 = Utc::now();
        let job = new_job(&environment, t0);
        let id = job.id;
        db.apply(&StatsEvent::Status(StatusUpdate {
            job,
            workflow_name: "wf-a".to_string(),
            status: "failed",
            started_at: t0,
            finished_at: Some(t0 + ChronoDuration::seconds(5)),
            error_message: Some("OOMKilled".to_string()),
        }))
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "failed");
        assert_eq!(
            record.get::<Option<String>, _>("error_message").as_deref(),
            Some("OOMKilled")
        );

        db.apply(&StatsEvent::Resubmitted {
            id,
            workflow_name: "wf-b".to_string(),
            submitted_at: t0 + ChronoDuration::seconds(60),
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "submitted");
        assert_eq!(record.get::<Option<String>, _>("outcome"), None);
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").unwrap(),
            "wf-b"
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), None);

        db.apply(&StatsEvent::Retried { id }).await.unwrap();
        assert_eq!(row(&db, id).await.get::<String, _>("status"), "running");
    }

    #[tokio::test]
    async fn test_recorder_writes_in_background_and_disabled_recorder_is_a_no_op() {
        StatsRecorder::disabled().record(StatsEvent::Deleted { id: Uuid::new_v4() });

        let Some(db) = test_db("test_recorder_writes_in_background").await else {
            return;
        };
        let recorder = StatsRecorder::new(db.clone(), JobVersions::default());
        let job = new_job(&unique_environment(), Utc::now());
        let id = job.id;
        recorder.record(StatsEvent::Created(job));
        recorder.record(StatsEvent::Deleted { id });
        for _ in 0..50 {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
                    .bind(id)
                    .fetch_optional(db.pool())
                    .await
                    .unwrap();
            if status.as_deref() == Some("deleted") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("recorder did not write the events");
    }
}

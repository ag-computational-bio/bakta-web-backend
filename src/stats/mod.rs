//! Job statistics database: schema migrations, job lifecycle recording and
//! the queries used by the admin API.

pub mod inputs;
pub mod queries;
#[cfg(test)]
mod queries_tests;
pub mod runtimes;

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

/// State of a job's current workflow, as seen by the polling loop.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowState {
    /// Written only when the job has no record yet, except for the
    /// environment, which is also corrected on existing records.
    pub job: NewJob,
    pub workflow_name: String,
    /// `submitted`, `running`, `succeeded`, `failed` or `error`.
    pub status: &'static str,
    /// Workflow creation time, used as `submitted_at` when the record has none.
    pub created_at: Option<DateTime<Utc>>,
    /// Argo `status.startedAt`. Argo resets it on retry.
    pub started_at: Option<DateTime<Utc>>,
    /// Argo `status.finishedAt` of a finished workflow.
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatsEvent {
    Created(NewJob),
    Submitted(Box<SubmittedJob>),
    /// The current workflow state of every job in one poll. The records are
    /// set to it, so a wrong record is repaired by the next poll.
    Polled(Vec<WorkflowState>),
    /// Argo retry of `workflow_name`, requested at `retried_at`. Sets
    /// `submitted_at` when it is the job's workflow. The new attempt's state
    /// comes from the next poll.
    Retried {
        id: Uuid,
        workflow_name: String,
        retried_at: DateTime<Utc>,
    },
    /// Argo resubmit of `previous_workflow_name` as `workflow_name`. Sets
    /// `submitted_at`, and points the record to the new workflow unless a
    /// poll already did. Ignored for other workflows.
    Resubmitted {
        id: Uuid,
        previous_workflow_name: String,
        workflow_name: String,
        submitted_at: DateTime<Utc>,
    },
    Deleted {
        id: Uuid,
    },
    /// `workflow_name` is the job's workflow, if any. The record is only
    /// changed when it still points to this workflow.
    Expired {
        id: Uuid,
        workflow_name: Option<String>,
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

    /// Sets the records to the polled workflow states. Missing records are
    /// created. A record is only written when it differs from the polled
    /// state, and records of deleted or expired jobs are never changed.
    async fn apply_polled(&self, states: &[WorkflowState]) -> Result<()> {
        if states.is_empty() {
            return Ok(());
        }
        let column = |value: fn(&WorkflowState) -> Option<String>| -> Vec<Option<String>> {
            states.iter().map(value).collect()
        };
        let ids: Vec<Uuid> = states.iter().map(|state| state.job.id).collect();
        sqlx::query(
            "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, name, created_at, updated_at)
             SELECT id, environment, workflow_kind, api_version, 'submitted', name, created_at, now()
             FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::timestamptz[])
                 AS polled(id, environment, workflow_kind, api_version, name, created_at)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&ids)
        .bind(column(|state| Some(state.job.environment.clone())))
        .bind(column(|state| Some(state.job.workflow_kind.clone())))
        .bind(column(|state| Some(state.job.api_version.clone())))
        .bind(column(|state| state.job.name.clone()))
        .bind(states.iter().map(|state| state.job.created_at).collect::<Vec<_>>())
        .execute(&self.pool)
        .await?;
        let times =
            |value: fn(&WorkflowState) -> Option<DateTime<Utc>>| -> Vec<Option<DateTime<Utc>>> {
                states.iter().map(value).collect()
            };
        sqlx::query(
            "WITH polled AS (
                 SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::timestamptz[],
                                      $5::timestamptz[], $6::timestamptz[], $7::text[], $8::text[])
                     AS polled(id, workflow_name, status, created_at, started_at, finished_at, error_message,
                               environment)
             ), target AS (
                 SELECT jobs.id, polled.workflow_name, polled.status, polled.started_at,
                     polled.finished_at, polled.error_message, polled.environment,
                     COALESCE(jobs.submitted_at, polled.created_at) AS submitted_at,
                     CASE WHEN polled.status IN ('succeeded', 'failed', 'error')
                         THEN polled.status ELSE jobs.outcome END AS outcome
                 FROM jobs JOIN polled ON jobs.id = polled.id
                 WHERE jobs.status NOT IN ('deleted', 'expired')
             ), wanted AS (
                 SELECT target.*,
                     -- GREATEST ignores NULL, so check for missing times first.
                     CASE WHEN started_at IS NOT NULL AND finished_at IS NOT NULL
                         THEN GREATEST(0, EXTRACT(EPOCH FROM (finished_at - started_at)))::double precision
                     END AS runtime_seconds,
                     CASE WHEN started_at >= submitted_at
                         THEN EXTRACT(EPOCH FROM (started_at - submitted_at))::double precision
                     END AS queue_seconds
                 FROM target
             )
             UPDATE jobs SET workflow_name = wanted.workflow_name, status = wanted.status,
                 outcome = wanted.outcome, submitted_at = wanted.submitted_at,
                 started_at = wanted.started_at, finished_at = wanted.finished_at,
                 runtime_seconds = wanted.runtime_seconds, queue_seconds = wanted.queue_seconds,
                 error_message = wanted.error_message, environment = wanted.environment,
                 updated_at = now()
             FROM wanted
             WHERE jobs.id = wanted.id
                 AND (jobs.workflow_name, jobs.status, jobs.outcome, jobs.submitted_at,
                      jobs.started_at, jobs.finished_at, jobs.runtime_seconds,
                      jobs.queue_seconds, jobs.error_message, jobs.environment)
                 IS DISTINCT FROM
                     (wanted.workflow_name, wanted.status, wanted.outcome, wanted.submitted_at,
                      wanted.started_at, wanted.finished_at, wanted.runtime_seconds,
                      wanted.queue_seconds, wanted.error_message, wanted.environment)",
        )
        .bind(&ids)
        .bind(column(|state| Some(state.workflow_name.clone())))
        .bind(column(|state| Some(state.status.to_string())))
        .bind(times(|state| state.created_at))
        .bind(times(|state| state.started_at))
        .bind(times(|state| state.finished_at))
        .bind(column(|state| state.error_message.clone()))
        .bind(column(|state| Some(state.job.environment.clone())))
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
                    "UPDATE jobs SET status = CASE WHEN status = 'created' THEN 'submitted' ELSE status END,
                         workflow_kind = $2, api_version = $3,
                         name = COALESCE(name, $4), genus = $5, species = $6, strain = $7,
                         sequence_count = $8, total_length = $9, config = $10,
                         bakta_version = $11, bakta_db_version = $12,
                         baktfold_version = $13, baktfold_db_version = $14,
                         workflow_name = COALESCE(workflow_name, $15), submitted_at = $16,
                         updated_at = now()
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
            StatsEvent::Polled(states) => self.apply_polled(states).await?,
            StatsEvent::Retried {
                id,
                workflow_name,
                retried_at,
            } => {
                // The polled columns are left to the next poll, which also
                // computes `queue_seconds` from this `submitted_at`.
                sqlx::query(
                    "UPDATE jobs SET submitted_at = $3, updated_at = now()
                     WHERE id = $1 AND (workflow_name IS NULL OR workflow_name = $2)",
                )
                .bind(id)
                .bind(workflow_name)
                .bind(retried_at)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Resubmitted {
                id,
                previous_workflow_name,
                workflow_name,
                submitted_at,
            } => {
                // Once a poll has seen the new workflow, only `submitted_at`
                // is set, so its state is not reset.
                sqlx::query(
                    "UPDATE jobs SET submitted_at = $3, updated_at = now(), workflow_name = $2,
                         status = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN 'submitted' ELSE status END,
                         started_at = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN NULL ELSE started_at END,
                         finished_at = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN NULL ELSE finished_at END,
                         runtime_seconds = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN NULL ELSE runtime_seconds END,
                         queue_seconds = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN NULL ELSE queue_seconds END,
                         error_message = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN NULL ELSE error_message END
                     WHERE id = $1
                         AND (workflow_name IS NULL OR workflow_name = $4 OR workflow_name = $2)",
                )
                .bind(id)
                .bind(workflow_name)
                .bind(submitted_at)
                .bind(previous_workflow_name)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Deleted { id } => {
                sqlx::query("UPDATE jobs SET status = 'deleted', updated_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
            }
            StatsEvent::Expired { id, workflow_name } => {
                sqlx::query(
                    "UPDATE jobs SET status = 'expired', updated_at = now()
                     WHERE id = $1 AND status <> 'deleted'
                         AND ($2::text IS NULL OR workflow_name IS NULL OR workflow_name = $2)",
                )
                .bind(id)
                .bind(workflow_name)
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

    /// Recorder that sends all events to the returned receiver. Only for tests.
    #[cfg(test)]
    pub fn capturing() -> (Self, mpsc::Receiver<StatsEvent>) {
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_SIZE);
        let recorder = Self {
            sender: Some(sender),
            db: None,
            versions: JobVersions::default(),
        };
        (recorder, receiver)
    }

    /// Recorder with a database whose events go to the returned receiver
    /// instead of the database, like a writer that is behind. Only for tests.
    #[cfg(test)]
    pub fn capturing_with_db(db: StatsDb) -> (Self, mpsc::Receiver<StatsEvent>) {
        let (recorder, receiver) = Self::capturing();
        (
            Self {
                db: Some(db),
                ..recorder
            },
            receiver,
        )
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
    use sqlx::{AssertSqlSafe, Connection, PgConnection, postgres::PgPoolOptions};
    use std::sync::{Arc, OnceLock};
    use tokio::sync::{OwnedSemaphorePermit, Semaphore};

    /// Database tests that may run at the same time in one test process. With
    /// the small pools below, several test runs fit into the default
    /// `max_connections` of 100 of one database server.
    const PARALLEL_TESTS: usize = 6;
    const POOL_CONNECTIONS: u32 = 2;

    fn test_slots() -> Arc<Semaphore> {
        static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(PARALLEL_TESTS)))
            .clone()
    }

    /// A migrated schema that belongs to one test. It is dropped with the guard.
    pub struct TestDb {
        db: StatsDb,
        admin_url: String,
        name: String,
        _slot: OwnedSemaphorePermit,
    }

    impl std::ops::Deref for TestDb {
        type Target = StatsDb;

        fn deref(&self) -> &StatsDb {
            &self.db
        }
    }

    impl Drop for TestDb {
        fn drop(&mut self) {
            let admin_url = self.admin_url.clone();
            let name = self.name.clone();
            // A separate thread with its own runtime works inside any test runtime.
            let _ = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async {
                    if let Ok(mut conn) = connect(&admin_url).await {
                        let _ = sqlx::query(AssertSqlSafe(format!(
                            "DROP SCHEMA IF EXISTS \"{name}\" CASCADE"
                        )))
                        .execute(&mut conn)
                        .await;
                        let _ = conn.close().await;
                    }
                });
            })
            .join();
        }
    }

    /// Opens a connection. Retries while the server has too many clients, for
    /// example when several test runs share one database server.
    async fn connect(url: &str) -> Result<PgConnection, sqlx::Error> {
        let mut attempts = 0;
        loop {
            match PgConnection::connect(url).await {
                Err(sqlx::Error::Database(error))
                    if error.code().as_deref() == Some("53300") && attempts < 120 =>
                {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                result => return result,
            }
        }
    }

    /// Returns `url` so that connections use `schema` as their search path.
    fn with_schema(url: &str, schema: &str) -> String {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{url}{separator}options=-c%20search_path%3D{schema}")
    }

    /// Creates a new migrated schema for one test in the `TEST_DATABASE_URL` database.
    /// Returns `None` with a message when the variable is unset.
    pub async fn test_db(test_name: &str) -> Option<TestDb> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("skipping {test_name}: TEST_DATABASE_URL is not set");
            return None;
        };
        let slot = test_slots()
            .acquire_owned()
            .await
            .expect("the test slots are never closed");
        let name = format!("test_{}", uuid::Uuid::new_v4().simple());
        let mut conn = connect(&url)
            .await
            .expect("TEST_DATABASE_URL should be reachable");
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{name}\"")))
            .execute(&mut conn)
            .await
            .expect("the test user should be allowed to create schemas");
        let _ = conn.close().await;
        let pool = PgPoolOptions::new()
            .max_connections(POOL_CONNECTIONS)
            .acquire_timeout(std::time::Duration::from_secs(30))
            .connect_lazy(&with_schema(&url, &name))
            .expect("TEST_DATABASE_URL should be valid");
        let guard = TestDb {
            db: StatsDb { pool },
            admin_url: url,
            name,
            _slot: slot,
        };
        guard.migrate().await.expect("migrations should run");
        Some(guard)
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

    /// A poll that sees `workflow_name` of `job` with the given state.
    fn polled(
        job: &NewJob,
        workflow_name: &str,
        status: &'static str,
        started_at: Option<DateTime<Utc>>,
        finished_at: Option<DateTime<Utc>>,
    ) -> StatsEvent {
        StatsEvent::Polled(vec![WorkflowState {
            job: job.clone(),
            workflow_name: workflow_name.to_string(),
            status,
            created_at: None,
            started_at,
            finished_at,
            error_message: None,
        }])
    }

    #[tokio::test]
    async fn test_missing_times_give_no_runtime_or_queue_time() {
        let Some(db) = test_db("test_missing_times_give_no_runtime_or_queue_time").await else {
            return;
        };
        let environment = unique_environment();
        let t0 = Utc::now() - ChronoDuration::minutes(30);
        let running = new_job(&environment, t0);
        let finished = new_job(&environment, t0);
        let waiting = new_job(&environment, t0);
        for job in [&running, &finished, &waiting] {
            db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
        }
        let started_at = Some(t0 + ChronoDuration::seconds(20));
        db.apply(&polled(&running, "wf-r", "running", started_at, None))
            .await
            .unwrap();
        db.apply(&polled(
            &finished,
            "wf-f",
            "succeeded",
            started_at,
            Some(t0 + ChronoDuration::seconds(120)),
        ))
        .await
        .unwrap();
        db.apply(&polled(&waiting, "wf-w", "submitted", None, None))
            .await
            .unwrap();

        let record = row(&db, running.id).await;
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), None);
        let record = row(&db, waiting.id).await;
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), None);
        assert_eq!(record.get::<Option<f64>, _>("queue_seconds"), None);
        let summary = db
            .summary(&queries::StatsFilter {
                environment: Some(environment),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(summary.runtime_seconds.avg, Some(100.0));
        assert_eq!(summary.runtime_seconds.p50, Some(100.0));
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

        let started_at = Some(t0 + ChronoDuration::seconds(20));
        db.apply(&polled(&job, "wf-1", "running", started_at, None))
            .await
            .unwrap();
        // Polling the same state again changes nothing.
        let updated_at = row(&db, id).await.get::<DateTime<Utc>, _>("updated_at");
        db.apply(&polled(&job, "wf-1", "running", started_at, None))
            .await
            .unwrap();
        assert_eq!(
            row(&db, id).await.get::<DateTime<Utc>, _>("updated_at"),
            updated_at
        );
        let finished_at = Some(t0 + ChronoDuration::seconds(120));
        db.apply(&polled(&job, "wf-1", "succeeded", started_at, finished_at))
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

        db.apply(&StatsEvent::Expired {
            id,
            workflow_name: None,
        })
        .await
        .unwrap();
        db.apply(&polled(&job, "wf-1", "failed", Some(t0), Some(t0)))
            .await
            .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "expired");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").unwrap(),
            "succeeded"
        );

        db.apply(&StatsEvent::Deleted { id }).await.unwrap();
        db.apply(&StatsEvent::Expired {
            id,
            workflow_name: None,
        })
        .await
        .unwrap();
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
        db.apply(&StatsEvent::Polled(vec![WorkflowState {
            job,
            workflow_name: "wf-a".to_string(),
            status: "failed",
            created_at: Some(t0),
            started_at: Some(t0),
            finished_at: Some(t0 + ChronoDuration::seconds(5)),
            error_message: Some("OOMKilled".to_string()),
        }]))
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
            previous_workflow_name: "wf-a".to_string(),
            workflow_name: "wf-b".to_string(),
            submitted_at: t0 + ChronoDuration::seconds(60),
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "submitted");
        // The earlier outcome stays until the new attempt finishes.
        assert_eq!(
            record.get::<Option<String>, _>("outcome").as_deref(),
            Some("failed")
        );
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").unwrap(),
            "wf-b"
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), None);

        let submitted_at = |record: &sqlx::postgres::PgRow| {
            record
                .get::<Option<DateTime<Utc>>, _>("submitted_at")
                .map(|time| time.timestamp_micros())
        };
        // A retry of the superseded workflow changes nothing.
        let before = submitted_at(&row(&db, id).await);
        db.apply(&StatsEvent::Retried {
            id,
            workflow_name: "wf-a".to_string(),
            retried_at: Utc::now(),
        })
        .await
        .unwrap();
        assert_eq!(submitted_at(&row(&db, id).await), before);
        // A retry of the current workflow only sets the request time.
        let retried_at = t0 + ChronoDuration::seconds(90);
        db.apply(&StatsEvent::Retried {
            id,
            workflow_name: "wf-b".to_string(),
            retried_at,
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "submitted");
        assert_eq!(submitted_at(&record), Some(retried_at.timestamp_micros()));
        // Neither does a resubmit of it.
        db.apply(&StatsEvent::Resubmitted {
            id,
            previous_workflow_name: "wf-a".to_string(),
            workflow_name: "wf-c".to_string(),
            submitted_at: t0,
        })
        .await
        .unwrap();
        assert_eq!(
            row(&db, id)
                .await
                .get::<Option<String>, _>("workflow_name")
                .as_deref(),
            Some("wf-b")
        );
    }

    #[tokio::test]
    async fn test_retry_times_measure_the_new_attempt() {
        let Some(db) = test_db("test_retry_times_measure_the_new_attempt").await else {
            return;
        };
        let t0 = Utc::now() - ChronoDuration::days(3);
        let job = new_job(&unique_environment(), t0);
        let id = job.id;
        let status = |status, started_at, finished_at| {
            polled(&job, "wf-a", status, Some(started_at), finished_at)
        };
        db.apply(&status("running", t0 + ChronoDuration::seconds(10), None))
            .await
            .unwrap();
        db.apply(&status(
            "failed",
            t0 + ChronoDuration::seconds(10),
            Some(t0 + ChronoDuration::days(2)),
        ))
        .await
        .unwrap();

        let retried_at = t0 + ChronoDuration::days(2) + ChronoDuration::hours(1);
        db.apply(&StatsEvent::Retried {
            id,
            workflow_name: "wf-a".to_string(),
            retried_at,
        })
        .await
        .unwrap();
        // The state stays until the next poll sees the new attempt.
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "failed");
        assert_eq!(
            record
                .get::<Option<DateTime<Utc>>, _>("submitted_at")
                .map(|time| time.timestamp_micros()),
            Some(retried_at.timestamp_micros())
        );
        // The earlier attempt started before the retry: no queue time.
        assert_eq!(record.get::<Option<f64>, _>("queue_seconds"), None);
        db.apply(&status(
            "running",
            retried_at + ChronoDuration::seconds(60),
            None,
        ))
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "running");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").as_deref(),
            Some("failed")
        );
        assert_eq!(record.get::<Option<DateTime<Utc>>, _>("finished_at"), None);

        // Argo resets startedAt on retry.
        db.apply(&status(
            "succeeded",
            retried_at + ChronoDuration::seconds(60),
            Some(retried_at + ChronoDuration::seconds(360)),
        ))
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(
            record.get::<Option<String>, _>("outcome").as_deref(),
            Some("succeeded")
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), Some(300.0));
        assert_eq!(record.get::<Option<f64>, _>("queue_seconds"), Some(60.0));
    }

    #[tokio::test]
    async fn test_late_retry_reset_keeps_the_newer_outcome() {
        let Some(db) = test_db("test_late_retry_reset_keeps_the_newer_outcome").await else {
            return;
        };
        let t0 = Utc::now() - ChronoDuration::minutes(10);
        let job = new_job(&unique_environment(), t0);
        let id = job.id;
        let status =
            |status, finished_at| polled(&job, "wf-a", status, Some(t0), Some(finished_at));
        db.apply(&status("failed", t0 + ChronoDuration::seconds(30)))
            .await
            .unwrap();
        // The retried attempt finished and was recorded before the reset.
        let retried_at = t0 + ChronoDuration::seconds(60);
        db.apply(&status("succeeded", t0 + ChronoDuration::seconds(90)))
            .await
            .unwrap();
        db.apply(&StatsEvent::Retried {
            id,
            workflow_name: "wf-a".to_string(),
            retried_at,
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "succeeded");
        assert_eq!(
            record.get::<Option<String>, _>("outcome").as_deref(),
            Some("succeeded")
        );
    }

    #[tokio::test]
    async fn test_events_do_not_overwrite_the_polled_state() {
        let Some(db) = test_db("test_events_do_not_overwrite_the_polled_state").await else {
            return;
        };
        let t0 = Utc::now() - ChronoDuration::hours(1);
        let job = new_job(&unique_environment(), t0);
        let id = job.id;
        let started_at = Some(t0 + ChronoDuration::seconds(30));
        // The poll finds the workflow before the start is recorded.
        db.apply(&polled(&job, "wf-a", "running", started_at, None))
            .await
            .unwrap();
        db.apply(&StatsEvent::Submitted(Box::new(SubmittedJob {
            job: job.clone(),
            genus: None,
            species: None,
            strain: None,
            sequence_count: Some(1),
            total_length: Some(10),
            config: None,
            versions: JobVersions::default(),
            workflow_name: "wf-a".to_string(),
            submitted_at: t0 + ChronoDuration::seconds(10),
        })))
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "running");
        assert_eq!(record.get::<Option<i64>, _>("total_length"), Some(10));
        // The next poll computes the queue time from the recorded start request.
        db.apply(&polled(&job, "wf-a", "running", started_at, None))
            .await
            .unwrap();
        assert_eq!(
            row(&db, id).await.get::<Option<f64>, _>("queue_seconds"),
            Some(20.0)
        );

        // The poll sees the resubmitted workflow finish before the resubmit is recorded.
        let started_b = Some(t0 + ChronoDuration::minutes(10));
        let finished_b = Some(t0 + ChronoDuration::minutes(15));
        db.apply(&polled(&job, "wf-b", "succeeded", started_b, finished_b))
            .await
            .unwrap();
        db.apply(&StatsEvent::Resubmitted {
            id,
            previous_workflow_name: "wf-a".to_string(),
            workflow_name: "wf-b".to_string(),
            submitted_at: t0 + ChronoDuration::minutes(9),
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "succeeded");
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").as_deref(),
            Some("wf-b")
        );
        assert_eq!(record.get::<Option<f64>, _>("runtime_seconds"), Some(300.0));
        db.apply(&polled(&job, "wf-b", "succeeded", started_b, finished_b))
            .await
            .unwrap();
        assert_eq!(
            row(&db, id).await.get::<Option<f64>, _>("queue_seconds"),
            Some(60.0)
        );

        // A wrong record is repaired by the next poll.
        sqlx::query("UPDATE jobs SET status = 'running', finished_at = NULL WHERE id = $1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
        db.apply(&polled(&job, "wf-b", "succeeded", started_b, finished_b))
            .await
            .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "succeeded");
        assert!(
            record
                .get::<Option<DateTime<Utc>>, _>("finished_at")
                .is_some()
        );
    }

    #[tokio::test]
    async fn test_poll_corrects_the_environment() {
        let Some(db) = test_db("test_poll_corrects_the_environment").await else {
            return;
        };
        let t0 = Utc::now() - ChronoDuration::hours(1);
        let mut job = new_job("staging", t0);
        let id = job.id;
        db.apply(&polled(&job, "wf-a", "running", Some(t0), None))
            .await
            .unwrap();
        assert_eq!(
            row(&db, id).await.get::<String, _>("environment"),
            "staging"
        );
        job.environment = "production".to_string();
        db.apply(&polled(&job, "wf-a", "running", Some(t0), None))
            .await
            .unwrap();
        assert_eq!(
            row(&db, id).await.get::<String, _>("environment"),
            "production"
        );
    }

    #[tokio::test]
    async fn test_expiry_of_a_superseded_workflow_is_ignored() {
        let Some(db) = test_db("test_expiry_of_a_superseded_workflow_is_ignored").await else {
            return;
        };
        let t0 = Utc::now();
        let job = new_job(&unique_environment(), t0);
        let id = job.id;
        db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
        db.apply(&StatsEvent::Resubmitted {
            id,
            previous_workflow_name: String::new(),
            workflow_name: "wf-b".to_string(),
            submitted_at: t0,
        })
        .await
        .unwrap();
        db.apply(&StatsEvent::Expired {
            id,
            workflow_name: Some("wf-a".to_string()),
        })
        .await
        .unwrap();
        let record = row(&db, id).await;
        assert_eq!(record.get::<String, _>("status"), "submitted");
        assert_eq!(record.get::<Option<String>, _>("outcome"), None);
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").as_deref(),
            Some("wf-b")
        );

        db.apply(&StatsEvent::Expired {
            id,
            workflow_name: Some("wf-b".to_string()),
        })
        .await
        .unwrap();
        assert_eq!(row(&db, id).await.get::<String, _>("status"), "expired");
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

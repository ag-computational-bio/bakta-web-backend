//! Job statistics database: schema migrations, job lifecycle recording and
//! the queries used by the admin API.

pub mod inputs;
pub mod queries;
#[cfg(test)]
mod queries_tests;
pub mod runtimes;
pub mod workflows;

use std::{collections::HashMap, sync::Arc, time::Duration};

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
    /// SHA-256 hash of the job secret, see `secret_hash`.
    pub secret_hash: Option<String>,
    pub result_kind: Option<String>,
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
    /// Sanitized `Origin` header of the start request, `Unknown` when missing.
    pub origin: String,
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
    /// The `origin` label. A missing label keeps the recorded origin.
    pub origin: Option<String>,
    /// Argo uid of the workflow, for the logs of archived workflows.
    pub argo_uid: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatsEvent {
    Created(NewJob),
    Submitted(Box<SubmittedJob>),
    /// The current workflow state of every job in one poll. The records are
    /// set to it, so a wrong record is repaired by the next poll.
    Polled(Vec<WorkflowState>),
    /// Job ID, environment and `origin` label of polled workflows whose jobs
    /// are deleted or expired. Only these two columns of existing records
    /// are corrected, for any status. A missing label keeps the origin.
    PolledMetadata(Vec<(Uuid, String, Option<String>)>),
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

/// Hex SHA-256 hash of a job secret. Only this hash is stored. The secrets
/// are 32 random characters, so a hash without salt is enough.
pub fn secret_hash(secret: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// True when `secret` has the hash `hash`, compared in constant time.
pub fn secret_has_hash(secret: &str, hash: &str) -> bool {
    use subtle::ConstantTimeEq;
    !secret.is_empty() && bool::from(secret_hash(secret).as_bytes().ct_eq(hash.as_bytes()))
}

/// True for job records of a finished workflow, also after expiry. Argo
/// archives finished workflows.
pub fn is_finished_record(status: &str) -> bool {
    is_terminal_status(status) || status == "expired"
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
            "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, name, created_at,
                               secret_hash, result_kind, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(job.id)
        .bind(&job.environment)
        .bind(&job.workflow_kind)
        .bind(&job.api_version)
        .bind(status)
        .bind(&job.name)
        .bind(job.created_at)
        .bind(&job.secret_hash)
        .bind(&job.result_kind)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sets only `environment` and `origin` of existing records, for any
    /// status. A record is only written when one of them differs.
    async fn apply_polled_metadata(
        &self,
        metadata: &[(Uuid, String, Option<String>)],
    ) -> Result<()> {
        if metadata.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE jobs SET environment = polled.environment,
                 origin = COALESCE(polled.origin, jobs.origin)
             FROM UNNEST($1::uuid[], $2::text[], $3::text[]) AS polled(id, environment, origin)
             WHERE jobs.id = polled.id
                 AND (jobs.environment, jobs.origin)
                     IS DISTINCT FROM (polled.environment, COALESCE(polled.origin, jobs.origin))",
        )
        .bind(metadata.iter().map(|(id, _, _)| *id).collect::<Vec<_>>())
        .bind(
            metadata
                .iter()
                .map(|(_, environment, _)| environment.clone())
                .collect::<Vec<_>>(),
        )
        .bind(
            metadata
                .iter()
                .map(|(_, _, origin)| origin.clone())
                .collect::<Vec<_>>(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sets the records to the polled workflow states. Missing records are
    /// created. A record is only written when it differs from the polled
    /// state. Records of deleted or expired jobs only get the environment
    /// and origin.
    async fn apply_polled(&self, states: &[WorkflowState]) -> Result<()> {
        if states.is_empty() {
            return Ok(());
        }
        let column = |value: fn(&WorkflowState) -> Option<String>| -> Vec<Option<String>> {
            states.iter().map(value).collect()
        };
        let ids: Vec<Uuid> = states.iter().map(|state| state.job.id).collect();
        sqlx::query(
            "INSERT INTO jobs (id, environment, workflow_kind, api_version, status, name, created_at, origin,
                               updated_at)
             SELECT id, environment, workflow_kind, api_version, 'submitted', name, created_at, origin, now()
             FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::timestamptz[],
                         $7::text[])
                 AS polled(id, environment, workflow_kind, api_version, name, created_at, origin)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&ids)
        .bind(column(|state| Some(state.job.environment.clone())))
        .bind(column(|state| Some(state.job.workflow_kind.clone())))
        .bind(column(|state| Some(state.job.api_version.clone())))
        .bind(column(|state| state.job.name.clone()))
        .bind(states.iter().map(|state| state.job.created_at).collect::<Vec<_>>())
        .bind(column(|state| state.origin.clone()))
        .execute(&self.pool)
        .await?;
        let times =
            |value: fn(&WorkflowState) -> Option<DateTime<Utc>>| -> Vec<Option<DateTime<Utc>>> {
                states.iter().map(value).collect()
            };
        sqlx::query(
            "WITH polled AS (
                 SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::timestamptz[],
                                      $5::timestamptz[], $6::timestamptz[], $7::text[], $8::text[],
                                      $9::text[], $10::text[], $11::text[], $12::uuid[])
                     AS polled(id, workflow_name, status, created_at, started_at, finished_at, error_message,
                               environment, origin, secret_hash, result_kind, argo_uid)
             ), target AS (
                 SELECT jobs.id, polled.workflow_name, polled.status, polled.started_at,
                     COALESCE(polled.created_at, jobs.workflow_created_at) AS workflow_created_at,
                     polled.finished_at, polled.error_message, polled.environment,
                     COALESCE(polled.origin, jobs.origin) AS origin,
                     COALESCE(jobs.secret_hash, polled.secret_hash) AS secret_hash,
                     COALESCE(jobs.result_kind, polled.result_kind) AS result_kind,
                     COALESCE(polled.argo_uid, jobs.argo_uid) AS argo_uid,
                     COALESCE(jobs.submitted_at, polled.created_at) AS submitted_at,
                     CASE WHEN polled.status IN ('succeeded', 'failed', 'error')
                         THEN polled.status ELSE jobs.outcome END AS outcome
                 FROM jobs JOIN polled ON jobs.id = polled.id
                 WHERE jobs.status NOT IN ('deleted', 'expired')
                     -- An older attempt never replaces a newer recorded one,
                     -- for example from a backend that still has it.
                     AND (jobs.workflow_name IS NULL OR jobs.workflow_name = polled.workflow_name
                          OR jobs.workflow_created_at IS NULL OR polled.created_at IS NULL
                          OR polled.created_at >= jobs.workflow_created_at)
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
                 origin = wanted.origin, secret_hash = wanted.secret_hash,
                 result_kind = wanted.result_kind, argo_uid = wanted.argo_uid,
                 workflow_created_at = wanted.workflow_created_at, updated_at = now()
             FROM wanted
             WHERE jobs.id = wanted.id
                 AND (jobs.workflow_name, jobs.status, jobs.outcome, jobs.submitted_at,
                      jobs.started_at, jobs.finished_at, jobs.runtime_seconds,
                      jobs.queue_seconds, jobs.error_message, jobs.environment, jobs.origin,
                      jobs.secret_hash, jobs.result_kind, jobs.argo_uid, jobs.workflow_created_at)
                 IS DISTINCT FROM
                     (wanted.workflow_name, wanted.status, wanted.outcome, wanted.submitted_at,
                      wanted.started_at, wanted.finished_at, wanted.runtime_seconds,
                      wanted.queue_seconds, wanted.error_message, wanted.environment, wanted.origin,
                      wanted.secret_hash, wanted.result_kind, wanted.argo_uid,
                      wanted.workflow_created_at)",
        )
        .bind(&ids)
        .bind(column(|state| Some(state.workflow_name.clone())))
        .bind(column(|state| Some(state.status.to_string())))
        .bind(times(|state| state.created_at))
        .bind(times(|state| state.started_at))
        .bind(times(|state| state.finished_at))
        .bind(column(|state| state.error_message.clone()))
        .bind(column(|state| Some(state.job.environment.clone())))
        .bind(column(|state| state.origin.clone()))
        .bind(column(|state| state.job.secret_hash.clone()))
        .bind(column(|state| state.job.result_kind.clone()))
        .bind(states.iter().map(|state| state.argo_uid).collect::<Vec<_>>())
        .execute(&self.pool)
        .await?;
        let metadata: Vec<_> = states
            .iter()
            .map(|state| {
                (
                    state.job.id,
                    state.job.environment.clone(),
                    state.origin.clone(),
                )
            })
            .collect();
        self.apply_polled_metadata(&metadata).await
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
                         origin = $17, updated_at = now()
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
                .bind(&submitted.origin)
                .execute(&self.pool)
                .await?;
            }
            StatsEvent::Polled(states) => self.apply_polled(states).await?,
            StatsEvent::PolledMetadata(metadata) => self.apply_polled_metadata(metadata).await?,
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
                         workflow_created_at = CASE WHEN workflow_name IS DISTINCT FROM $2 THEN $3
                             ELSE workflow_created_at END,
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
/// Events are written in order by a single worker, see `Writer`. When
/// recording is disabled, every call is a no-op.
#[derive(Clone, Default)]
pub struct StatsRecorder {
    sink: Option<Sink>,
    db: Option<StatsDb>,
    health: Arc<WriterHealth>,
    pub versions: JobVersions,
}

/// State of the writer for metrics and readiness.
#[derive(Default)]
struct WriterHealth {
    /// Number of parked events.
    parked: std::sync::atomic::AtomicUsize,
    /// Since when the writer retries a write that fails with a transient
    /// error (the database is not reachable), `None` while it works.
    stuck_since: std::sync::Mutex<Option<std::time::Instant>>,
}

#[derive(Clone)]
enum Sink {
    Writer(mpsc::Sender<Queued>),
    /// Tests: events go to a receiver instead of the database.
    #[cfg(test)]
    Capture(mpsc::Sender<StatsEvent>),
}

/// An event and, for `persist`, the confirmation that it was written.
type Queued = (StatsEvent, Option<tokio::sync::oneshot::Sender<()>>);

/// Attempts of a statistics-only event before it is dropped.
const STATS_ONLY_ATTEMPTS: u32 = 3;
/// Attempts of a durable event whose error is neither clearly transient nor
/// clearly permanent before it is parked.
const DURABLE_ATTEMPTS: u32 = 5;
const MIN_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(10);
/// Interval at which parked events are tried again.
const PARKED_RETRY: Duration = Duration::from_secs(300);

/// A write that `persist` could not confirm in time. Answered with `503`.
/// The write stays queued and is still retried.
#[derive(Debug)]
pub struct RecordWriteFailed;

impl std::fmt::Display for RecordWriteFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "The job database is not available, try again later")
    }
}

impl std::error::Error for RecordWriteFailed {}

/// How a failed write is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The database is not reachable or busy: the write is repeated in place.
    Transient,
    /// The write itself is wrong (constraint, data or schema error): it is
    /// parked at once.
    Permanent,
    /// Anything else: repeated up to `DURABLE_ATTEMPTS` times, then parked.
    Other,
}

fn classify(error: &anyhow::Error) -> Failure {
    match error.downcast_ref::<sqlx::Error>() {
        Some(
            sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed,
        ) => Failure::Transient,
        Some(sqlx::Error::Database(error)) => match error.code().as_deref() {
            // Connection, resources, operator intervention, serialization
            // failure, deadlock and lock timeout.
            Some(code)
                if code.starts_with("08")
                    || code.starts_with("53")
                    || code.starts_with("57")
                    || matches!(code, "40001" | "40P01" | "55P03") =>
            {
                Failure::Transient
            }
            // Data exceptions, constraint violations, syntax and schema errors.
            Some(code)
                if code.starts_with("22") || code.starts_with("23") || code.starts_with("42") =>
            {
                Failure::Permanent
            }
            _ => Failure::Other,
        },
        _ => Failure::Other,
    }
}

impl StatsEvent {
    /// The jobs whose records the event writes.
    fn job_ids(&self) -> Vec<Uuid> {
        match self {
            StatsEvent::Created(job) => vec![job.id],
            StatsEvent::Submitted(submitted) => vec![submitted.job.id],
            StatsEvent::Polled(states) => states.iter().map(|state| state.job.id).collect(),
            StatsEvent::PolledMetadata(metadata) => metadata.iter().map(|(id, _, _)| *id).collect(),
            StatsEvent::Retried { id, .. }
            | StatsEvent::Resubmitted { id, .. }
            | StatsEvent::Deleted { id }
            | StatsEvent::Expired { id, .. } => vec![*id],
        }
    }

    /// One event per job of a workflow update of several jobs, so one bad
    /// job does not keep the others from being written.
    fn split(self) -> Vec<StatsEvent> {
        match self {
            StatsEvent::Polled(states) if states.len() > 1 => states
                .into_iter()
                .map(|state| StatsEvent::Polled(vec![state]))
                .collect(),
            event => vec![event],
        }
    }

    /// Events that keep the state of jobs, which the user API serves from
    /// the database. The others only feed the statistics.
    fn is_durable(&self) -> bool {
        matches!(
            self,
            StatsEvent::Created(_)
                | StatsEvent::Polled(_)
                | StatsEvent::Resubmitted { .. }
                | StatsEvent::Deleted { .. }
                | StatsEvent::Expired { .. }
        )
    }
}

impl StatsRecorder {
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Starts the background writer. Must be called inside a Tokio runtime.
    pub fn new(db: StatsDb, versions: JobVersions) -> Self {
        let (sender, receiver) = mpsc::channel::<Queued>(EVENT_QUEUE_SIZE);
        let health = Arc::new(WriterHealth::default());
        let writer = Writer {
            db: db.clone(),
            parked: HashMap::new(),
            health: health.clone(),
        };
        tokio::spawn(writer.run(receiver));
        Self {
            sink: Some(Sink::Writer(sender)),
            db: Some(db),
            health,
            versions,
        }
    }

    /// Number of parked events, see `Writer`.
    pub fn parked_events(&self) -> usize {
        self.health
            .parked
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How long the writer has been retrying a write that fails with a
    /// transient error, `None` while it works.
    pub fn writer_stuck_for(&self) -> Option<Duration> {
        self.health
            .stuck_since
            .lock()
            .expect("writer health lock poisoned")
            .map(|since| since.elapsed())
    }

    /// Recorder that sends all events to the returned receiver. Only for tests.
    #[cfg(test)]
    pub fn capturing() -> (Self, mpsc::Receiver<StatsEvent>) {
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_SIZE);
        let recorder = Self {
            sink: Some(Sink::Capture(sender)),
            db: None,
            health: Default::default(),
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
        let result = match &self.sink {
            None => return,
            Some(Sink::Writer(sender)) => sender.try_send((event, None)).map_err(|e| e.to_string()),
            #[cfg(test)]
            Some(Sink::Capture(sender)) => sender.try_send(event).map_err(|e| e.to_string()),
        };
        if let Err(error) = result {
            tracing::warn!(%error, "stats_event_dropped");
        }
    }

    /// Queues `event` and waits until it is written, at most `wait`. Fails
    /// with `RecordWriteFailed` when it was not written in time; the event
    /// stays queued and is retried. Without a database it succeeds at once.
    pub async fn persist(&self, event: StatsEvent, wait: Duration) -> anyhow::Result<()> {
        let sender = match &self.sink {
            None => return Ok(()),
            Some(Sink::Writer(sender)) => sender,
            #[cfg(test)]
            Some(Sink::Capture(sender)) => {
                // The test reads the events like the writer.
                return sender
                    .send(event)
                    .await
                    .map_err(|_| RecordWriteFailed.into());
            }
        };
        let (written, confirmed) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + wait;
        let sent = tokio::time::timeout_at(deadline, sender.send((event, Some(written)))).await;
        if !matches!(sent, Ok(Ok(()))) {
            tracing::warn!("stats_persist_not_queued");
            return Err(RecordWriteFailed.into());
        }
        match tokio::time::timeout_at(deadline, confirmed).await {
            Ok(Ok(())) => Ok(()),
            _ => {
                tracing::warn!("stats_persist_not_confirmed");
                Err(RecordWriteFailed.into())
            }
        }
    }
}

/// Writes the queued events in order.
///
/// - A write that fails with a transient error (the database is not
///   reachable or busy) is repeated in place with a pause of 1 to 10 seconds,
///   as long as it takes. Later events wait.
/// - A durable event (see `StatsEvent::is_durable`) whose write fails with a
///   permanent error (constraint, data or schema error), or still fails after
///   `DURABLE_ATTEMPTS` attempts, is parked: it is kept and logged once, and
///   later events of the same job are kept behind it, so the order per job
///   stays. Events of other jobs move on. A workflow update of several jobs
///   is split first, so only the bad job is parked. Parked events are tried
///   again every five minutes and whenever a new event of their job arrives.
/// - Statistics-only events are tried `STATS_ONLY_ATTEMPTS` times and then
///   dropped.
struct Writer {
    db: StatsDb,
    /// Parked events by job, oldest first. The first one failed.
    parked: HashMap<Uuid, std::collections::VecDeque<Queued>>,
    health: Arc<WriterHealth>,
}

impl Writer {
    async fn run(mut self, mut receiver: mpsc::Receiver<Queued>) {
        let mut retry = tokio::time::interval(PARKED_RETRY);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        retry.tick().await;
        loop {
            tokio::select! {
                queued = receiver.recv() => match queued {
                    Some((event, written)) => self.handle(event, written).await,
                    None => return,
                },
                _ = retry.tick() => {
                    let jobs: Vec<Uuid> = self.parked.keys().copied().collect();
                    for id in jobs {
                        self.flush(id).await;
                    }
                }
            }
        }
    }

    async fn handle(
        &mut self,
        event: StatsEvent,
        written: Option<tokio::sync::oneshot::Sender<()>>,
    ) {
        let jobs = event.job_ids();
        if jobs.len() == 1 && event.is_durable() {
            return self.handle_one(event, written).await;
        }
        let blocked = event.is_durable() && jobs.iter().any(|id| self.parked.contains_key(id));
        if !blocked {
            match self.write(&event).await {
                Ok(()) => {
                    if let Some(written) = written {
                        let _ = written.send(());
                    }
                    return;
                }
                Err(error) if !event.is_durable() => {
                    tracing::warn!(%error, ?event, "stats_write_failed");
                    return;
                }
                Err(_) => {}
            }
        }
        // The jobs are written or parked one by one. The whole event is not
        // confirmed then.
        for part in event.split() {
            self.handle_one(part, None).await;
        }
    }

    /// Writes or parks an event of one job.
    async fn handle_one(
        &mut self,
        event: StatsEvent,
        written: Option<tokio::sync::oneshot::Sender<()>>,
    ) {
        let Some(id) = event.job_ids().first().copied() else {
            return;
        };
        if self.parked.contains_key(&id) {
            self.park(id, (event, written));
            self.flush(id).await;
            return;
        }
        match self.write(&event).await {
            Ok(()) => {
                if let Some(written) = written {
                    let _ = written.send(());
                }
            }
            Err(error) if event.is_durable() => {
                tracing::warn!(job_id = %id, %error, ?event, "job_record_write_parked");
                self.park(id, (event, written));
            }
            Err(error) => tracing::warn!(%error, ?event, "stats_write_failed"),
        }
    }

    fn park(&mut self, id: Uuid, queued: Queued) {
        self.parked.entry(id).or_default().push_back(queued);
        self.count_parked();
    }

    /// Writes the parked events of a job in order, until one fails again.
    async fn flush(&mut self, id: Uuid) {
        while let Some((event, _)) = self.parked.get(&id).and_then(|events| events.front()) {
            let event = event.clone();
            if let Err(error) = self.write(&event).await {
                tracing::debug!(job_id = %id, %error, "job_record_write_still_parked");
                break;
            }
            if let Some((_, Some(written))) = self
                .parked
                .get_mut(&id)
                .and_then(|events| events.pop_front())
            {
                let _ = written.send(());
            }
            if self.parked.get(&id).is_some_and(|events| events.is_empty()) {
                self.parked.remove(&id);
                tracing::info!(job_id = %id, "job_record_write_unparked");
            }
        }
        self.count_parked();
    }

    fn count_parked(&self) {
        let count = self.parked.values().map(|events| events.len()).sum();
        self.health
            .parked
            .store(count, std::sync::atomic::Ordering::Relaxed);
    }

    /// Writes an event. Transient errors are retried as long as they last,
    /// other errors give up as described at `Writer`.
    async fn write(&self, event: &StatsEvent) -> std::result::Result<(), String> {
        let mut attempt = 0;
        let mut pause = MIN_RETRY;
        loop {
            attempt += 1;
            let (failure, error) =
                match tokio::time::timeout(WRITE_TIMEOUT, self.db.apply(event)).await {
                    Ok(Ok(())) => {
                        self.set_stuck(false);
                        return Ok(());
                    }
                    Ok(Err(error)) => (classify(&error), error.to_string()),
                    Err(_) => (Failure::Transient, "timed out".to_string()),
                };
            if !event.is_durable() && attempt >= STATS_ONLY_ATTEMPTS {
                self.set_stuck(false);
                return Err(error);
            }
            match failure {
                Failure::Transient => self.set_stuck(true),
                Failure::Permanent => {
                    self.set_stuck(false);
                    return Err(error);
                }
                Failure::Other if attempt >= DURABLE_ATTEMPTS => {
                    self.set_stuck(false);
                    return Err(error);
                }
                Failure::Other => {}
            }
            tracing::warn!(%error, attempt, ?failure, "stats_write_retry");
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(MAX_RETRY);
        }
    }

    fn set_stuck(&self, stuck: bool) {
        let mut since = self
            .health
            .stuck_since
            .lock()
            .expect("writer health lock poisoned");
        match (stuck, *since) {
            (true, None) => *since = Some(std::time::Instant::now()),
            (false, Some(_)) => *since = None,
            _ => {}
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
            secret_hash: None,
            result_kind: None,
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
            origin: None,
            argo_uid: None,
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

    #[test]
    fn test_secret_hash_is_sha256_and_never_contains_the_secret() {
        let hash = secret_hash("abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let secret = "Q2xhdWRlU2VjcmV0MTIzNDU2Nzg5MDEy";
        assert_ne!(secret_hash(secret), secret);
        assert!(!secret_hash(secret).contains(secret));
    }

    #[tokio::test]
    async fn test_secret_hash_result_kind_and_uid_are_recorded() {
        let Some(db) = test_db("test_secret_hash_result_kind_and_uid_are_recorded").await else {
            return;
        };
        let job = new_job(&unique_environment(), Utc::now());
        db.apply(&StatsEvent::Created(NewJob {
            secret_hash: Some(secret_hash("first")),
            result_kind: Some("baktfold".to_string()),
            ..job.clone()
        }))
        .await
        .unwrap();
        let record = row(&db, job.id).await;
        assert_eq!(
            record.get::<Option<String>, _>("secret_hash"),
            Some(secret_hash("first"))
        );
        // A poll fills empty columns only and sets the workflow uid.
        let uid = Uuid::new_v4();
        let StatsEvent::Polled(mut states) = polled(&job, "wf-1", "running", None, None) else {
            unreachable!()
        };
        states[0].job.secret_hash = Some(secret_hash("other"));
        states[0].job.result_kind = Some("bakta".to_string());
        states[0].argo_uid = Some(uid);
        db.apply(&StatsEvent::Polled(states)).await.unwrap();
        let record = row(&db, job.id).await;
        assert_eq!(
            record.get::<Option<String>, _>("secret_hash"),
            Some(secret_hash("first"))
        );
        assert_eq!(
            record.get::<Option<String>, _>("result_kind").as_deref(),
            Some("baktfold")
        );
        assert_eq!(record.get::<Option<Uuid>, _>("argo_uid"), Some(uid));
        // A job polled without a record gets the hash of its workflow label.
        let other = new_job(&unique_environment(), Utc::now());
        let StatsEvent::Polled(mut states) = polled(&other, "wf-2", "running", None, None) else {
            unreachable!()
        };
        states[0].job.secret_hash = Some(secret_hash("label"));
        db.apply(&StatsEvent::Polled(states)).await.unwrap();
        assert_eq!(
            row(&db, other.id)
                .await
                .get::<Option<String>, _>("secret_hash"),
            Some(secret_hash("label"))
        );
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
            origin: "Unknown".to_string(),
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
            origin: Some("bakta.computational.bio".to_string()),
            argo_uid: None,
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
            origin: "Unknown".to_string(),
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
    async fn test_origin_comes_from_the_start_and_the_poll() {
        let Some(db) = test_db("test_origin_comes_from_the_start_and_the_poll").await else {
            return;
        };
        let t0 = Utc::now() - ChronoDuration::hours(1);
        let job = new_job(&unique_environment(), t0);
        let id = job.id;
        async fn origin(db: &StatsDb, id: Uuid) -> Option<String> {
            row(db, id).await.get("origin")
        }
        let poll = |origin: Option<&str>| {
            StatsEvent::Polled(vec![WorkflowState {
                job: job.clone(),
                workflow_name: "wf-a".to_string(),
                status: "running",
                created_at: None,
                started_at: Some(t0),
                finished_at: None,
                error_message: None,
                origin: origin.map(str::to_string),
                argo_uid: None,
            }])
        };
        db.apply(&poll(Some("bakta.computational.bio")))
            .await
            .unwrap();
        assert_eq!(
            origin(&db, id).await.as_deref(),
            Some("bakta.computational.bio")
        );
        // A workflow without the label keeps the recorded origin.
        db.apply(&poll(None)).await.unwrap();
        assert_eq!(
            origin(&db, id).await.as_deref(),
            Some("bakta.computational.bio")
        );
        db.apply(&poll(Some("Unknown"))).await.unwrap();
        assert_eq!(origin(&db, id).await.as_deref(), Some("Unknown"));

        let started = new_job(&unique_environment(), t0);
        db.apply(&StatsEvent::Created(started.clone()))
            .await
            .unwrap();
        assert_eq!(origin(&db, started.id).await, None);
        db.apply(&StatsEvent::Submitted(Box::new(SubmittedJob {
            job: started.clone(),
            genus: None,
            species: None,
            strain: None,
            sequence_count: None,
            total_length: None,
            config: None,
            versions: JobVersions::default(),
            workflow_name: "wf-b".to_string(),
            submitted_at: t0,
            origin: "staging.bakta.computational.bio".to_string(),
        })))
        .await
        .unwrap();
        assert_eq!(
            origin(&db, started.id).await.as_deref(),
            Some("staging.bakta.computational.bio")
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

    /// Makes every write of job `id` fail with a constraint error.
    async fn poison_job(db: &StatsDb, id: Uuid) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE OR REPLACE FUNCTION poison_job() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 IF NEW.id = '{id}' THEN
                     RAISE EXCEPTION 'poisoned job' USING ERRCODE = 'check_violation';
                 END IF;
                 RETURN NEW;
             END $$"
        )))
        .execute(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER poison_job BEFORE INSERT OR UPDATE ON jobs
             FOR EACH ROW EXECUTE FUNCTION poison_job()",
        )
        .execute(db.pool())
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_a_poisoned_job_does_not_block_other_jobs() {
        let Some(db) = test_db("test_a_poisoned_job_does_not_block_other_jobs").await else {
            return;
        };
        let environment = unique_environment();
        let (bad, good, other) = (
            new_job(&environment, Utc::now()),
            new_job(&environment, Utc::now()),
            new_job(&environment, Utc::now()),
        );
        for job in [&bad, &good, &other] {
            db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
        }
        poison_job(&db, bad.id).await;
        let recorder = StatsRecorder::new((*db).clone(), JobVersions::default());
        let wait = Duration::from_secs(10);

        // One batch with the poisoned job and a good one, then another job.
        let StatsEvent::Polled(mut states) = polled(&bad, "wf-bad", "succeeded", None, None) else {
            unreachable!()
        };
        let StatsEvent::Polled(good_states) = polled(&good, "wf-good", "succeeded", None, None)
        else {
            unreachable!()
        };
        states.extend(good_states);
        recorder.record(StatsEvent::Polled(states));
        recorder
            .persist(StatsEvent::Deleted { id: other.id }, wait)
            .await
            .expect("other jobs are written");
        assert_eq!(
            row(&db, good.id).await.get::<String, _>("status"),
            "succeeded"
        );
        assert_eq!(
            row(&db, other.id).await.get::<String, _>("status"),
            "deleted"
        );
        // The writer is not stuck: one parked job does not take it down.
        assert_eq!(recorder.parked_events(), 1);
        assert!(recorder.writer_stuck_for().is_none());
        // A later event of the poisoned job waits behind it.
        assert!(
            recorder
                .persist(StatsEvent::Deleted { id: bad.id }, Duration::from_secs(2))
                .await
                .is_err()
        );
        assert_eq!(row(&db, bad.id).await.get::<String, _>("status"), "created");

        // Once the job can be written, a new event writes its kept events in order.
        sqlx::query("DROP TRIGGER poison_job ON jobs")
            .execute(db.pool())
            .await
            .unwrap();
        recorder
            .persist(StatsEvent::Deleted { id: bad.id }, wait)
            .await
            .expect("the kept events are written");
        let record = row(&db, bad.id).await;
        assert_eq!(record.get::<String, _>("status"), "deleted");
        assert_eq!(recorder.parked_events(), 0);
        assert_eq!(
            record.get::<Option<String>, _>("workflow_name").as_deref(),
            Some("wf-bad")
        );
    }
}

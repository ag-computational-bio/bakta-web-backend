//! One-time import of the Bakta Web job history from the Argo workflows
//! into the statistics database. Run with `bakta-web-backend backfill`.
//! Safe to run again: it only inserts missing jobs and fills empty columns.

pub mod db;
pub mod parse;
pub mod summary;
pub mod workflows;

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::task::{JoinError, JoinSet};

use crate::{s3_handler::S3Handler, stats::StatsDb};
use db::{BackfillJob, Fill};

/// Jobs that are written (and read from S3) at the same time.
const CONCURRENCY: usize = 4;
/// Progress is logged after this many jobs.
const PROGRESS_EVERY: usize = 1000;
/// Rows per write to the `workflows` table.
const TABLE_CHUNK: usize = 2000;

#[derive(Debug, Default)]
struct Counts {
    inserted: usize,
    filled: usize,
    skipped: usize,
    s3_missing: usize,
    errors: usize,
}

impl Counts {
    /// Adds the result of one job: how it was written, and whether its
    /// summary file was missing.
    fn add(&mut self, result: Result<Result<(Fill, bool)>, JoinError>) {
        match result {
            Ok(Ok((fill, s3_missing))) => {
                match fill {
                    Fill::Inserted => self.inserted += 1,
                    Fill::Filled => self.filled += 1,
                    Fill::Unchanged => self.skipped += 1,
                }
                self.s3_missing += usize::from(s3_missing);
            }
            Ok(Err(error)) => {
                self.errors += 1;
                tracing::warn!(error = format!("{error:#}"), "backfill_job_failed");
            }
            Err(error) => {
                self.errors += 1;
                tracing::warn!(%error, "backfill_job_failed");
            }
        }
    }
}

/// Only succeeded Bakta runs have a summary file. Bakta Proteins and
/// Baktfold have no genome.
fn needs_summary(job: &BackfillJob) -> bool {
    job.outcome.as_deref() == Some("succeeded")
        && matches!(job.job.workflow_kind.as_str(), "bakta" | "bakta_baktfold")
}

/// Reads the summary when asked to, then writes the job. Returns how the
/// job was written and whether the summary was missing.
async fn fill_job(
    db: StatsDb,
    s3: Arc<S3Handler>,
    mut job: BackfillJob,
    read_summary: bool,
) -> Result<(Fill, bool)> {
    let id = job.job.id;
    let mut s3_missing = false;
    if read_summary {
        match s3
            .read_result_text(&id.to_string(), summary::SUMMARY_FILE)
            .await
        {
            Ok(Some(text)) => {
                let summary = summary::parse_summary(&text);
                job.total_length = summary.total_length;
                job.sequence_count = summary.sequence_count;
                job.versions.bakta_db_version = summary.db_version;
            }
            Ok(None) => s3_missing = true,
            Err(error) => {
                s3_missing = true;
                tracing::warn!(job_id = %id, %error, "backfill_summary_unreadable");
            }
        }
    }
    let fill = db
        .backfill_job(&job)
        .await
        .with_context(|| format!("job {id}"))?;
    Ok((fill, s3_missing))
}

pub async fn run() -> Result<()> {
    let var = |name: &str| dotenvy::var(name).with_context(|| format!("{name} is not set"));
    let db = StatsDb::connect_lazy(&var("STATS_DATABASE_URL")?)?;
    db.migrate().await?;
    let s3 = Arc::new(S3Handler::new(
        var("S3_ACCESS_KEY")?,
        var("S3_SECRET_KEY")?,
        var("S3_BUCKET")?,
        var("S3_ENDPOINT")?,
    ));
    let namespace = dotenvy::var("ARGO_NAMESPACE").unwrap_or_else(|_| "bakta".to_string());

    let observed_at = Utc::now();
    let (listed, unreadable) = workflows::KubeClient::in_cluster()?
        .list_workflows(&namespace)
        .await?;
    let workflow_count = listed.len();
    let table = workflows::table_workflows(&listed);
    let jobs =
        workflows::newest_per_job(listed.into_iter().filter(workflows::is_bakta_web).collect());
    let total = jobs.len();
    tracing::info!(
        workflows = workflow_count,
        unreadable,
        jobs = total,
        "backfill_started"
    );

    let with_summary = db.jobs_with_summary().await?;
    let now = Utc::now();
    let mut counts = Counts {
        errors: unreadable,
        ..Default::default()
    };
    let mut tasks = JoinSet::new();
    for (index, workflow) in jobs.into_iter().enumerate() {
        while tasks.len() >= CONCURRENCY {
            if let Some(result) = tasks.join_next().await {
                counts.add(result);
            }
        }
        match workflows::backfill_job(&workflow, now) {
            Some(job) => {
                let read_summary = needs_summary(&job) && !with_summary.contains(&job.job.id);
                tasks.spawn(fill_job(db.clone(), s3.clone(), job, read_summary));
            }
            None => {
                counts.errors += 1;
                tracing::warn!(workflow = %workflow.item.metadata.name, "backfill_workflow_skipped");
            }
        }
        if (index + 1) % PROGRESS_EVERY == 0 {
            tracing::info!(done = index + 1, total, ?counts, "backfill_progress");
        }
    }
    while let Some(result) = tasks.join_next().await {
        counts.add(result);
    }
    // All Bakta Web workflows go to the `workflows` table, after the job
    // records: the reconcile at startup skips a stored workflow only when
    // its job record is final. A row that the backend changed after
    // `observed_at` is kept, also when it is gone.
    for chunk in table.chunks(TABLE_CHUNK) {
        db.upsert_workflows(&namespace, observed_at, chunk).await?;
    }
    tracing::info!(workflows = table.len(), "backfill_workflow_table_written");
    tracing::info!(
        jobs = total,
        inserted = counts.inserted,
        filled = counts.filled,
        skipped = counts.skipped,
        s3_missing = counts.s3_missing,
        errors = counts.errors,
        "backfill_finished"
    );
    Ok(())
}

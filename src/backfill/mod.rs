//! One-time import of the Bakta Web job history from the Argo workflows
//! into the statistics database. Run with `bakta-web-backend backfill`.
//! Safe to run again: it only inserts missing jobs and fills empty columns.

pub mod db;
pub mod enrich;
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

/// Bakta Proteins and Baktfold have no genome.
pub(crate) fn is_genome_job(job: &BackfillJob) -> bool {
    matches!(job.job.workflow_kind.as_str(), "bakta" | "bakta_baktfold")
}

/// Only succeeded Bakta runs have a summary file.
pub(crate) fn needs_summary(job: &BackfillJob) -> bool {
    job.outcome.as_deref() == Some("succeeded") && is_genome_job(job)
}

/// Reads the summary when asked to, then writes the job. Returns how the
/// job was written and whether the summary was missing.
async fn fill_job(
    db: StatsDb,
    s3: Arc<S3Handler>,
    mut job: BackfillJob,
    read_summary: bool,
    read_input: bool,
) -> Result<(Fill, bool)> {
    let id = job.job.id;
    let s3_missing = read_sizes(&s3, &mut job, read_summary, read_input).await;
    let fill = db
        .backfill_job(&job)
        .await
        .with_context(|| format!("job {id}"))?;
    Ok((fill, s3_missing))
}

/// Sets the genome size and database version of a job from S3: from the
/// summary of a succeeded genome job when `read_summary`, else, for a genome
/// job without both size values and with `read_input`, the missing ones from
/// its input FASTA (failed jobs and jobs without or with a partial summary). The database version only comes
/// from the summary. Returns whether the summary was missing.
pub(crate) async fn read_sizes(
    s3: &S3Handler,
    job: &mut BackfillJob,
    read_summary: bool,
    read_input: bool,
) -> bool {
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
    // A value from the summary is kept, a missing one comes from the input.
    let missing_size = job.total_length.is_none() || job.sequence_count.is_none();
    if read_input && is_genome_job(job) && missing_size {
        match s3
            .count_input_fasta(&id.to_string(), crate::s3_handler::INPUT_READ_LIMIT)
            .await
        {
            Ok(Some(count)) => {
                job.total_length = job.total_length.or(Some(count.bases));
                job.sequence_count = job
                    .sequence_count
                    .or_else(|| i32::try_from(count.sequences).ok());
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(job_id = %id, %error, "backfill_input_unreadable"),
        }
    }
    s3_missing
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
    let with_size = db.jobs_with_size().await?;
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
                // Failed jobs have no summary: their size comes from the input.
                let read_input = !with_size.contains(&job.job.id);
                tasks.spawn(fill_job(
                    db.clone(),
                    s3.clone(),
                    job,
                    read_summary,
                    read_input,
                ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_handler::S3Handler;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    fn job(kind: &str, outcome: &str) -> BackfillJob {
        let mut job = db::tests::backfill_job("production", Utc::now());
        job.job.workflow_kind = kind.to_string();
        job.outcome = Some(outcome.to_string());
        job.total_length = None;
        job.sequence_count = None;
        job.versions.bakta_db_version = None;
        job
    }

    async fn s3_with(files: &[(String, &str)]) -> (MockServer, S3Handler) {
        let server = MockServer::start().await;
        for (key, body) in files {
            Mock::given(method("GET"))
                .and(path(format!("/{key}")))
                .respond_with(ResponseTemplate::new(200).set_body_string(*body))
                .mount(&server)
                .await;
        }
        let handler = S3Handler::new(
            "access".to_string(),
            "secret".to_string(),
            "bucket".to_string(),
            format!("http://s3.test:{}", server.address().port()),
        )
        .with_bucket_host_address(*server.address());
        (server, handler)
    }

    #[tokio::test]
    async fn test_sizes_come_from_the_summary_or_else_from_the_input() {
        let fasta = ">a\nACGT\n>b\nGG\n";
        let summary = "Sequence(s): 3\nLength: 1000\nDatabase: v6.0\n";
        let failed = job("bakta", "failed");
        let succeeded = job("bakta_baktfold", "succeeded");
        let no_summary = job("bakta", "succeeded");
        let proteins = job("bakta_proteins", "failed");
        let input = |job: &BackfillJob| format!("jobs/{}/inputs/fastadata.fasta", job.job.id);
        let (_server, s3) = s3_with(&[
            (input(&failed), fasta),
            (input(&succeeded), fasta),
            (
                format!("jobs/{}/results/result.txt", succeeded.job.id),
                summary,
            ),
            (input(&no_summary), fasta),
            (input(&proteins), fasta),
        ])
        .await;

        // A failed job has no summary: the size comes from its input.
        let mut job = failed;
        assert!(
            !{
                let summary = needs_summary(&job);
                read_sizes(&s3, &mut job, summary, true)
            }
            .await
        );
        assert_eq!((job.total_length, job.sequence_count), (Some(6), Some(2)));
        assert_eq!(job.versions.bakta_db_version, None);
        // The summary wins when it exists.
        let mut job = succeeded;
        assert!(
            !{
                let summary = needs_summary(&job);
                read_sizes(&s3, &mut job, summary, true)
            }
            .await
        );
        assert_eq!(job.total_length, Some(1000));
        // A missing summary falls back to the input.
        let mut job = no_summary;
        assert!(
            {
                let summary = needs_summary(&job);
                read_sizes(&s3, &mut job, summary, true)
            }
            .await
        );
        assert_eq!(job.total_length, Some(6));
        // Jobs without genome, and jobs whose size is recorded, read nothing.
        let mut job = proteins;
        {
            let summary = needs_summary(&job);
            read_sizes(&s3, &mut job, summary, true)
        }
        .await;
        assert_eq!(job.total_length, None);
        let mut job = self::job("bakta", "failed");
        read_sizes(&s3, &mut job, false, false).await;
        assert_eq!(job.total_length, None);
    }

    #[tokio::test]
    async fn test_a_summary_without_count_falls_back_to_the_input_for_the_count() {
        let job = job("bakta", "succeeded");
        let (_server, s3) = s3_with(&[
            (
                format!("jobs/{}/results/result.txt", job.job.id),
                "Length: 1000\nDatabase: v6.0\n",
            ),
            (
                format!("jobs/{}/inputs/fastadata.fasta", job.job.id),
                ">a\nACGT\n>b\nGG\n",
            ),
        ])
        .await;
        let mut job = job;
        read_sizes(&s3, &mut job, true, true).await;
        assert_eq!(job.total_length, Some(1000));
        assert_eq!(job.sequence_count, Some(2));
        assert_eq!(job.versions.bakta_db_version.as_deref(), Some("6.0"));
    }
}

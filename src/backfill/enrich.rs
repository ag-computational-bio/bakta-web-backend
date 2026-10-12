//! Fills the details of finished jobs that the backend did not record at
//! the start (for example jobs of the old production backend): organism,
//! input options and versions from the workflow, and the genome size from
//! S3, with the rules of the backfill. Best effort and in the background:
//! a full queue drops requests, failures are only logged.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use chrono::Utc;
use tokio::{
    sync::{Mutex, mpsc},
    time::Instant,
};

use super::{needs_summary, read_sizes, workflows::Workflow};
use crate::{s3_handler::S3Handler, stats::StatsDb};

/// Requests that can wait. More are dropped.
const QUEUE_SIZE: usize = 256;
/// Requests worked on at the same time.
const WORKERS: usize = 2;
/// Time after the completion before a request is worked on, so the job
/// record of the completion is written first.
const DELAY: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Enricher {
    sender: mpsc::Sender<(Instant, Workflow)>,
}

impl Enricher {
    /// Starts the workers. Must be called inside a Tokio runtime.
    pub fn start(db: StatsDb, s3: S3Handler) -> Self {
        let (sender, receiver) = mpsc::channel::<(Instant, Workflow)>(QUEUE_SIZE);
        let receiver = Arc::new(Mutex::new(receiver));
        let s3 = Arc::new(s3);
        for _ in 0..WORKERS {
            let (db, s3, receiver) = (db.clone(), s3.clone(), receiver.clone());
            tokio::spawn(async move {
                loop {
                    let Some((at, workflow)) = receiver.lock().await.recv().await else {
                        return;
                    };
                    tokio::time::sleep_until(at + DELAY).await;
                    let name = workflow.item.metadata.name.clone();
                    if let Err(error) = enrich(&db, &s3, workflow).await {
                        tracing::warn!(workflow_name = %name, error = format!("{error:#}"), "job_enrichment_failed");
                    }
                }
            });
        }
        Self { sender }
    }

    /// Asks to fill the details of the job of a finished workflow.
    pub fn request(&self, workflow: Workflow) {
        if self.sender.try_send((Instant::now(), workflow)).is_err() {
            tracing::debug!("job_enrichment_dropped");
        }
    }
}

/// Fills the empty details of the job of `workflow`. Jobs without a record
/// or with all details are skipped.
async fn enrich(db: &StatsDb, s3: &S3Handler, workflow: Workflow) -> Result<()> {
    let Some(mut job) = super::workflows::backfill_job(&workflow, Utc::now()) else {
        return Ok(());
    };
    let Some(missing) = db.missing_details(job.job.id).await? else {
        return Ok(());
    };
    let genome = super::is_genome_job(&job);
    let read_summary = needs_summary(&job) && (missing.size || missing.database_version);
    let read_input = genome && missing.size;
    if !(missing.details || read_summary || read_input) {
        return Ok(());
    }
    read_sizes(s3, &mut job, read_summary, read_input).await;
    tokio::time::timeout(WRITE_TIMEOUT, db.backfill_job(&job)).await??;
    tracing::debug!(job_id = %job.job.id, "job_enriched");
    Ok(())
}

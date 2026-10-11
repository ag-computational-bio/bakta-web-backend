//! Runtime statistics of succeeded jobs for the admin API.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    StatsDb,
    queries::{StatsFilter, push_filter},
};

/// Lower bounds of the runtime histogram buckets in seconds.
pub const RUNTIME_BUCKETS: [i64; 9] = [0, 120, 300, 600, 1200, 1800, 3600, 7200, 14400];
/// Lower bounds of the input size buckets in bp.
pub const SIZE_BUCKETS: [i64; 7] = [
    0, 1_000_000, 2_000_000, 4_000_000, 6_000_000, 8_000_000, 12_000_000,
];

/// Builds a Postgres array literal of bucket bounds for `width_bucket`.
pub(super) fn bucket_array(bounds: &[i64]) -> String {
    let values: Vec<String> = bounds.iter().map(i64::to_string).collect();
    format!("ARRAY[{}]::double precision[]", values.join(","))
}

/// Upper bound of bucket `index`, or `None` for the last bucket.
pub(super) fn bucket_max(bounds: &[i64], index: usize) -> Option<i64> {
    bounds.get(index + 1).copied()
}

/// Filter plus an optional workflow kind list.
pub(super) fn push_kind_filter(
    builder: &mut QueryBuilder<Postgres>,
    filter: &StatsFilter,
    workflow_kinds: &[String],
) {
    push_filter(builder, filter);
    if !workflow_kinds.is_empty() {
        builder
            .push(" AND workflow_kind = ANY(")
            .push_bind(workflow_kinds.to_vec())
            .push(")");
    }
}

/// Jobs that count for runtime statistics: succeeded, with a runtime.
const SUCCEEDED: &str = " AND outcome = 'succeeded' AND runtime_seconds IS NOT NULL";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct KindRuntime {
    pub workflow_kind: String,
    pub jobs: i64,
    pub min: Option<f64>,
    pub p10: Option<f64>,
    pub p25: Option<f64>,
    pub p50: Option<f64>,
    pub p75: Option<f64>,
    pub p90: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
    pub stddev: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeBucket {
    pub min_seconds: i64,
    /// Null for the last bucket.
    pub max_seconds: Option<i64>,
    pub jobs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InputSizeRuntime {
    pub min_bp: i64,
    /// Null for the last bucket.
    pub max_bp: Option<i64>,
    pub jobs: i64,
    pub runtime_p50: Option<f64>,
    pub runtime_p90: Option<f64>,
    pub runtime_avg: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct SecondsPerMbp {
    pub p50: Option<f64>,
    pub avg: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct QueueStats {
    pub avg: Option<f64>,
    pub p50: Option<f64>,
    pub p90: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct SlowJob {
    pub id: Uuid,
    pub workflow_kind: String,
    pub organism: Option<String>,
    pub total_length: Option<i64>,
    pub runtime_seconds: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RuntimeStats {
    pub jobs: i64,
    pub by_workflow_kind: Vec<KindRuntime>,
    pub histogram: Vec<RuntimeBucket>,
    pub by_input_size: Vec<InputSizeRuntime>,
    pub seconds_per_mbp: SecondsPerMbp,
    pub queue_seconds: QueueStats,
    pub slowest: Vec<SlowJob>,
}

#[derive(FromRow)]
struct BucketCount {
    bucket: i32,
    jobs: i64,
}

#[derive(FromRow)]
struct SizeRow {
    bucket: i32,
    jobs: i64,
    runtime_p50: Option<f64>,
    runtime_p90: Option<f64>,
    runtime_avg: Option<f64>,
}

impl StatsDb {
    pub async fn runtimes(
        &self,
        filter: &StatsFilter,
        workflow_kinds: &[String],
    ) -> Result<RuntimeStats> {
        let mut builder = QueryBuilder::new(
            "SELECT workflow_kind, count(*) AS jobs, min(runtime_seconds) AS min,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY runtime_seconds) AS p10,
                percentile_cont(0.25) WITHIN GROUP (ORDER BY runtime_seconds) AS p25,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY runtime_seconds) AS p50,
                percentile_cont(0.75) WITHIN GROUP (ORDER BY runtime_seconds) AS p75,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY runtime_seconds) AS p90,
                percentile_cont(0.95) WITHIN GROUP (ORDER BY runtime_seconds) AS p95,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY runtime_seconds) AS p99,
                max(runtime_seconds) AS max, avg(runtime_seconds) AS avg,
                stddev_samp(runtime_seconds) AS stddev
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(SUCCEEDED)
            .push(" GROUP BY workflow_kind ORDER BY workflow_kind");
        let by_workflow_kind: Vec<KindRuntime> =
            builder.build_query_as().fetch_all(self.pool()).await?;

        let mut builder = QueryBuilder::new(format!(
            "SELECT width_bucket(runtime_seconds, {}) AS bucket, count(*) AS jobs FROM jobs",
            bucket_array(&RUNTIME_BUCKETS)
        ));
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder.push(SUCCEEDED).push(" GROUP BY 1");
        let counts: Vec<BucketCount> = builder.build_query_as().fetch_all(self.pool()).await?;
        let histogram = RUNTIME_BUCKETS
            .iter()
            .enumerate()
            .map(|(index, &min_seconds)| RuntimeBucket {
                min_seconds,
                max_seconds: bucket_max(&RUNTIME_BUCKETS, index),
                jobs: counts
                    .iter()
                    .filter(|count| count.bucket as usize == index + 1)
                    .map(|count| count.jobs)
                    .sum(),
            })
            .collect();

        let mut builder = QueryBuilder::new(format!(
            "SELECT width_bucket(total_length::double precision, {}) AS bucket, count(*) AS jobs,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p50,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p90,
                avg(runtime_seconds) AS runtime_avg
             FROM jobs",
            bucket_array(&SIZE_BUCKETS)
        ));
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(SUCCEEDED)
            .push(" AND total_length IS NOT NULL GROUP BY 1");
        let sizes: Vec<SizeRow> = builder.build_query_as().fetch_all(self.pool()).await?;
        let by_input_size = SIZE_BUCKETS
            .iter()
            .enumerate()
            .map(|(index, &min_bp)| {
                let row = sizes.iter().find(|row| row.bucket as usize == index + 1);
                InputSizeRuntime {
                    min_bp,
                    max_bp: bucket_max(&SIZE_BUCKETS, index),
                    jobs: row.map_or(0, |row| row.jobs),
                    runtime_p50: row.and_then(|row| row.runtime_p50),
                    runtime_p90: row.and_then(|row| row.runtime_p90),
                    runtime_avg: row.and_then(|row| row.runtime_avg),
                }
            })
            .collect();

        let mut builder = QueryBuilder::new(
            "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY runtime_seconds / (total_length / 1e6)) AS p50,
                avg(runtime_seconds / (total_length / 1e6)) AS avg
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder.push(SUCCEEDED).push(" AND total_length > 0");
        let seconds_per_mbp = builder.build_query_as().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(
            "SELECT avg(queue_seconds) AS avg,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY queue_seconds) AS p50,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY queue_seconds) AS p90,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY queue_seconds) AS p99,
                max(queue_seconds) AS max
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder.push(" AND queue_seconds IS NOT NULL");
        let queue_seconds = builder.build_query_as().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(
            "SELECT id, workflow_kind, NULLIF(concat_ws(' ', genus, species, strain), '') AS organism,
                total_length, runtime_seconds, created_at
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(SUCCEEDED)
            .push(" ORDER BY runtime_seconds DESC, created_at DESC LIMIT 10");
        let slowest = builder.build_query_as().fetch_all(self.pool()).await?;

        Ok(RuntimeStats {
            jobs: by_workflow_kind.iter().map(|kind| kind.jobs).sum(),
            by_workflow_kind,
            histogram,
            by_input_size,
            seconds_per_mbp,
            queue_seconds,
            slowest,
        })
    }
}

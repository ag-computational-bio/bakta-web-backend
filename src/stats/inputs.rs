//! Input size statistics for the admin API.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    StatsDb,
    queries::StatsFilter,
    runtimes::{SIZE_BUCKETS, bucket_array, bucket_max, push_kind_filter},
};

/// Lower bounds of the sequence count buckets.
pub const SEQUENCE_BUCKETS: [i64; 7] = [1, 2, 6, 21, 101, 501, 2001];

const WITH_LENGTH: &str = " AND total_length IS NOT NULL";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct LengthStats {
    pub min: Option<f64>,
    pub p10: Option<f64>,
    pub p25: Option<f64>,
    pub p50: Option<f64>,
    pub p75: Option<f64>,
    pub p90: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct SequenceCountStats {
    pub min: Option<i64>,
    pub p50: Option<f64>,
    pub p90: Option<f64>,
    pub p99: Option<f64>,
    pub max: Option<i64>,
    pub avg: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SizeBucket {
    pub min_bp: i64,
    /// Null for the last bucket.
    pub max_bp: Option<i64>,
    pub jobs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SequenceBucket {
    pub min_sequences: i64,
    /// Inclusive. Null for the last bucket.
    pub max_sequences: Option<i64>,
    pub jobs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct KindInputs {
    pub workflow_kind: String,
    pub jobs: i64,
    pub total_bp: i64,
    pub p50_bp: Option<f64>,
    pub avg_bp: Option<f64>,
    pub p50_sequences: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct LargeJob {
    pub id: Uuid,
    pub workflow_kind: String,
    pub organism: Option<String>,
    pub total_length: i64,
    pub sequence_count: Option<i32>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InputStats {
    pub jobs: i64,
    pub total_bp: i64,
    pub total_length: LengthStats,
    pub sequence_count: SequenceCountStats,
    pub size_histogram: Vec<SizeBucket>,
    pub sequence_histogram: Vec<SequenceBucket>,
    pub by_workflow_kind: Vec<KindInputs>,
    pub largest: Vec<LargeJob>,
}

#[derive(FromRow)]
struct BucketCount {
    bucket: i32,
    jobs: i64,
}

fn count_for(counts: &[BucketCount], index: usize) -> i64 {
    counts
        .iter()
        .filter(|count| count.bucket as usize == index + 1)
        .map(|count| count.jobs)
        .sum()
}

impl StatsDb {
    pub async fn inputs(
        &self,
        filter: &StatsFilter,
        workflow_kinds: &[String],
    ) -> Result<InputStats> {
        let mut builder = QueryBuilder::new(
            "SELECT min(total_length)::double precision AS min,
                percentile_cont(0.1) WITHIN GROUP (ORDER BY total_length) AS p10,
                percentile_cont(0.25) WITHIN GROUP (ORDER BY total_length) AS p25,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY total_length) AS p50,
                percentile_cont(0.75) WITHIN GROUP (ORDER BY total_length) AS p75,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY total_length) AS p90,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY total_length) AS p99,
                max(total_length)::double precision AS max,
                avg(total_length)::double precision AS avg
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder.push(WITH_LENGTH);
        let total_length: LengthStats = builder.build_query_as().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(
            "SELECT min(sequence_count)::bigint AS min,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY sequence_count) AS p50,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY sequence_count) AS p90,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY sequence_count) AS p99,
                max(sequence_count)::bigint AS max,
                avg(sequence_count)::double precision AS avg
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(WITH_LENGTH)
            .push(" AND sequence_count IS NOT NULL");
        let sequence_count = builder.build_query_as().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(format!(
            "SELECT width_bucket(total_length::double precision, {}) AS bucket, count(*) AS jobs FROM jobs",
            bucket_array(&SIZE_BUCKETS)
        ));
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder.push(WITH_LENGTH).push(" GROUP BY 1");
        let counts: Vec<BucketCount> = builder.build_query_as().fetch_all(self.pool()).await?;
        let size_histogram = SIZE_BUCKETS
            .iter()
            .enumerate()
            .map(|(index, &min_bp)| SizeBucket {
                min_bp,
                max_bp: bucket_max(&SIZE_BUCKETS, index),
                jobs: count_for(&counts, index),
            })
            .collect();

        let mut builder = QueryBuilder::new(format!(
            "SELECT width_bucket(sequence_count::double precision, {}) AS bucket, count(*) AS jobs FROM jobs",
            bucket_array(&SEQUENCE_BUCKETS)
        ));
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(WITH_LENGTH)
            .push(" AND sequence_count IS NOT NULL GROUP BY 1");
        let counts: Vec<BucketCount> = builder.build_query_as().fetch_all(self.pool()).await?;
        let sequence_histogram = SEQUENCE_BUCKETS
            .iter()
            .enumerate()
            .map(|(index, &min_sequences)| SequenceBucket {
                min_sequences,
                max_sequences: bucket_max(&SEQUENCE_BUCKETS, index).map(|next| next - 1),
                jobs: count_for(&counts, index),
            })
            .collect();

        let mut builder = QueryBuilder::new(
            "SELECT workflow_kind, count(*) AS jobs, COALESCE(sum(total_length), 0)::bigint AS total_bp,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY total_length) AS p50_bp,
                avg(total_length)::double precision AS avg_bp,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY sequence_count) AS p50_sequences
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(WITH_LENGTH)
            .push(" GROUP BY workflow_kind ORDER BY workflow_kind");
        let by_workflow_kind: Vec<KindInputs> =
            builder.build_query_as().fetch_all(self.pool()).await?;

        let mut builder = QueryBuilder::new(
            "SELECT id, workflow_kind, NULLIF(concat_ws(' ', genus, species, strain), '') AS organism,
                total_length, sequence_count, status, created_at
             FROM jobs",
        );
        push_kind_filter(&mut builder, filter, workflow_kinds);
        builder
            .push(WITH_LENGTH)
            .push(" ORDER BY total_length DESC, created_at DESC LIMIT 10");
        let largest = builder.build_query_as().fetch_all(self.pool()).await?;

        Ok(InputStats {
            jobs: by_workflow_kind.iter().map(|kind| kind.jobs).sum(),
            total_bp: by_workflow_kind.iter().map(|kind| kind.total_bp).sum(),
            total_length,
            sequence_count,
            size_histogram,
            sequence_histogram,
            by_workflow_kind,
            largest,
        })
    }
}

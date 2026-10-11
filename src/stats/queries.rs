//! Read queries for the admin statistics and job list endpoints.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

use super::StatsDb;

/// Common filter: environment (`None` means all) and a `created_at` range.
#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    pub environment: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

fn push_filter(builder: &mut QueryBuilder<Postgres>, filter: &StatsFilter) {
    builder.push(" WHERE TRUE");
    if let Some(environment) = &filter.environment {
        builder
            .push(" AND environment = ")
            .push_bind(environment.clone());
    }
    if let Some(from) = filter.from {
        builder.push(" AND created_at >= ").push_bind(from);
    }
    if let Some(to) = filter.to {
        builder.push(" AND created_at < ").push_bind(to);
    }
}

const DURATION_COLUMNS: &str = "
    avg(runtime_seconds) AS runtime_avg,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p50,
    percentile_cont(0.9) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p90,
    percentile_cont(0.99) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p99";

const COUNT_COLUMNS: &str = "
    count(*) AS jobs_total,
    count(*) FILTER (WHERE outcome IS NOT NULL) AS jobs_finished,
    count(*) FILTER (WHERE outcome = 'succeeded') AS jobs_succeeded,
    count(*) FILTER (WHERE outcome = 'failed') AS jobs_failed,
    count(*) FILTER (WHERE outcome = 'error') AS jobs_error";

/// Average and percentiles of a duration in seconds. Values are null without data.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DurationStats {
    pub avg: Option<f64>,
    pub p50: Option<f64>,
    pub p90: Option<f64>,
    pub p99: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WorkflowKindStats {
    pub workflow_kind: String,
    pub jobs_total: i64,
    pub jobs_succeeded: i64,
    pub runtime_seconds: DurationStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StatsSummary {
    pub jobs_total: i64,
    pub jobs_finished: i64,
    pub jobs_succeeded: i64,
    pub jobs_failed: i64,
    pub jobs_error: i64,
    pub jobs_active: i64,
    pub success_rate: Option<f64>,
    pub runtime_seconds: DurationStats,
    pub queue_seconds: DurationStats,
    pub total_input_bp: i64,
    pub by_workflow_kind: Vec<WorkflowKindStats>,
}

#[derive(FromRow)]
struct SummaryRow {
    jobs_total: i64,
    jobs_finished: i64,
    jobs_succeeded: i64,
    jobs_failed: i64,
    jobs_error: i64,
    jobs_active: i64,
    runtime_avg: Option<f64>,
    runtime_p50: Option<f64>,
    runtime_p90: Option<f64>,
    runtime_p99: Option<f64>,
    queue_avg: Option<f64>,
    queue_p50: Option<f64>,
    queue_p90: Option<f64>,
    queue_p99: Option<f64>,
    total_input_bp: i64,
}

#[derive(FromRow)]
struct KindRow {
    workflow_kind: String,
    jobs_total: i64,
    jobs_succeeded: i64,
    runtime_avg: Option<f64>,
    runtime_p50: Option<f64>,
    runtime_p90: Option<f64>,
    runtime_p99: Option<f64>,
}

/// Bucket size for the time series.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    Day,
    Week,
    #[default]
    Month,
    Year,
}

impl Interval {
    fn as_str(self) -> &'static str {
        match self {
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
            Interval::Year => "year",
        }
    }
}

/// Column used to split the time series into groups.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    #[default]
    None,
    WorkflowKind,
    Status,
    ApiVersion,
}

impl GroupBy {
    fn column(self) -> &'static str {
        match self {
            GroupBy::None => "NULL::text",
            GroupBy::WorkflowKind => "workflow_kind",
            GroupBy::Status => "status",
            GroupBy::ApiVersion => "api_version",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct TimeseriesBucket {
    pub period_start: DateTime<Utc>,
    pub group: Option<String>,
    pub jobs_total: i64,
    pub jobs_succeeded: i64,
    pub jobs_failed: i64,
    pub jobs_error: i64,
    pub runtime_p50: Option<f64>,
    pub runtime_avg: Option<f64>,
    pub total_input_bp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Timeseries {
    pub interval: Interval,
    pub group_by: GroupBy,
    pub buckets: Vec<TimeseriesBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct VersionUsage {
    pub bakta_version: Option<String>,
    pub bakta_db_version: Option<String>,
    pub baktfold_version: Option<String>,
    pub jobs_total: i64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VersionList {
    pub items: Vec<VersionUsage>,
}

/// Sort order of the job list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobSort {
    column: &'static str,
    descending: bool,
}

impl Default for JobSort {
    fn default() -> Self {
        Self {
            column: "created_at",
            descending: true,
        }
    }
}

impl std::str::FromStr for JobSort {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (prefixed, name) = match value.strip_prefix('-') {
            Some(name) => (true, name),
            None => (false, value),
        };
        // `created_at` lists the newest jobs first, `-created_at` the oldest.
        // The other columns sort ascending, and descending with `-`.
        let (column, descending) = match name {
            "created_at" => ("created_at", !prefixed),
            "runtime_seconds" => ("runtime_seconds", prefixed),
            "total_length" => ("total_length", prefixed),
            _ => return Err(anyhow::anyhow!("Invalid sort value: {value}")),
        };
        Ok(Self { column, descending })
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobListQuery {
    pub filter: StatsFilter,
    pub statuses: Vec<String>,
    pub workflow_kinds: Vec<String>,
    pub q: Option<String>,
    pub limit: i64,
    pub offset: i64,
    pub sort: JobSort,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct JobListItem {
    pub id: Uuid,
    pub environment: String,
    pub workflow_kind: String,
    pub status: String,
    pub outcome: Option<String>,
    pub organism: Option<String>,
    pub sequence_count: Option<i32>,
    pub total_length: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub runtime_seconds: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct JobList {
    pub total: i64,
    pub items: Vec<JobListItem>,
}

/// Full statistics record of one job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema, FromRow)]
pub struct JobRecord {
    pub id: Uuid,
    pub environment: String,
    pub workflow_kind: String,
    pub api_version: String,
    pub status: String,
    pub outcome: Option<String>,
    pub name: Option<String>,
    pub genus: Option<String>,
    pub species: Option<String>,
    pub strain: Option<String>,
    pub sequence_count: Option<i32>,
    pub total_length: Option<i64>,
    #[schema(value_type = Option<Object>)]
    pub config: Option<serde_json::Value>,
    pub bakta_version: Option<String>,
    pub bakta_db_version: Option<String>,
    pub baktfold_version: Option<String>,
    pub baktfold_db_version: Option<String>,
    pub workflow_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub runtime_seconds: Option<f64>,
    pub queue_seconds: Option<f64>,
    pub error_message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn push_job_list_filter(builder: &mut QueryBuilder<Postgres>, query: &JobListQuery) {
    push_filter(builder, &query.filter);
    if !query.statuses.is_empty() {
        builder
            .push(" AND status = ANY(")
            .push_bind(query.statuses.clone())
            .push(")");
    }
    if !query.workflow_kinds.is_empty() {
        builder
            .push(" AND workflow_kind = ANY(")
            .push_bind(query.workflow_kinds.clone())
            .push(")");
    }
    if let Some(q) = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let escaped = escape_like(q);
        let contains = format!("%{escaped}%");
        builder
            .push(" AND (id::text ILIKE ")
            .push_bind(format!("{escaped}%"))
            .push(" OR name ILIKE ")
            .push_bind(contains.clone())
            .push(" OR concat_ws(' ', genus, species, strain) ILIKE ")
            .push_bind(contains)
            .push(")");
    }
}

impl StatsDb {
    pub async fn summary(&self, filter: &StatsFilter) -> Result<StatsSummary> {
        let mut builder = QueryBuilder::new(format!(
            "SELECT {COUNT_COLUMNS},
                count(*) FILTER (WHERE status IN ('submitted', 'running')) AS jobs_active,
                {DURATION_COLUMNS},
                avg(queue_seconds) AS queue_avg,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY queue_seconds) AS queue_p50,
                percentile_cont(0.9) WITHIN GROUP (ORDER BY queue_seconds) AS queue_p90,
                percentile_cont(0.99) WITHIN GROUP (ORDER BY queue_seconds) AS queue_p99,
                COALESCE(sum(total_length), 0)::bigint AS total_input_bp
             FROM jobs"
        ));
        push_filter(&mut builder, filter);
        let row: SummaryRow = builder.build_query_as().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(format!(
            "SELECT workflow_kind, {COUNT_COLUMNS}, {DURATION_COLUMNS} FROM jobs"
        ));
        push_filter(&mut builder, filter);
        builder.push(" GROUP BY workflow_kind ORDER BY workflow_kind");
        let kinds: Vec<KindRow> = builder.build_query_as().fetch_all(self.pool()).await?;

        Ok(StatsSummary {
            jobs_total: row.jobs_total,
            jobs_finished: row.jobs_finished,
            jobs_succeeded: row.jobs_succeeded,
            jobs_failed: row.jobs_failed,
            jobs_error: row.jobs_error,
            jobs_active: row.jobs_active,
            success_rate: (row.jobs_finished > 0)
                .then(|| row.jobs_succeeded as f64 / row.jobs_finished as f64),
            runtime_seconds: DurationStats {
                avg: row.runtime_avg,
                p50: row.runtime_p50,
                p90: row.runtime_p90,
                p99: row.runtime_p99,
            },
            queue_seconds: DurationStats {
                avg: row.queue_avg,
                p50: row.queue_p50,
                p90: row.queue_p90,
                p99: row.queue_p99,
            },
            total_input_bp: row.total_input_bp,
            by_workflow_kind: kinds
                .into_iter()
                .map(|kind| WorkflowKindStats {
                    workflow_kind: kind.workflow_kind,
                    jobs_total: kind.jobs_total,
                    jobs_succeeded: kind.jobs_succeeded,
                    runtime_seconds: DurationStats {
                        avg: kind.runtime_avg,
                        p50: kind.runtime_p50,
                        p90: kind.runtime_p90,
                        p99: kind.runtime_p99,
                    },
                })
                .collect(),
        })
    }

    pub async fn timeseries(
        &self,
        filter: &StatsFilter,
        interval: Interval,
        group_by: GroupBy,
    ) -> Result<Timeseries> {
        let mut builder = QueryBuilder::new("SELECT date_trunc(");
        builder
            .push_bind(interval.as_str())
            .push(", created_at, 'UTC') AS period_start, ")
            .push(group_by.column())
            .push(
                " AS \"group\",
                count(*) AS jobs_total,
                count(*) FILTER (WHERE outcome = 'succeeded') AS jobs_succeeded,
                count(*) FILTER (WHERE outcome = 'failed') AS jobs_failed,
                count(*) FILTER (WHERE outcome = 'error') AS jobs_error,
                percentile_cont(0.5) WITHIN GROUP (ORDER BY runtime_seconds) AS runtime_p50,
                avg(runtime_seconds) AS runtime_avg,
                COALESCE(sum(total_length), 0)::bigint AS total_input_bp
             FROM jobs",
            );
        push_filter(&mut builder, filter);
        builder.push(" GROUP BY 1, 2 ORDER BY 1, 2");
        let buckets = builder.build_query_as().fetch_all(self.pool()).await?;
        Ok(Timeseries {
            interval,
            group_by,
            buckets,
        })
    }

    pub async fn versions(&self, filter: &StatsFilter) -> Result<VersionList> {
        let mut builder = QueryBuilder::new(
            "SELECT bakta_version, bakta_db_version, baktfold_version, count(*) AS jobs_total,
                min(created_at) AS first_seen, max(created_at) AS last_seen
             FROM jobs",
        );
        push_filter(&mut builder, filter);
        builder.push(
            " AND (bakta_version IS NOT NULL OR baktfold_version IS NOT NULL)
             GROUP BY 1, 2, 3 ORDER BY last_seen DESC",
        );
        let items = builder.build_query_as().fetch_all(self.pool()).await?;
        Ok(VersionList { items })
    }

    pub async fn list_jobs(&self, query: &JobListQuery) -> Result<JobList> {
        let mut builder = QueryBuilder::new("SELECT count(*) FROM jobs");
        push_job_list_filter(&mut builder, query);
        let total: i64 = builder.build_query_scalar().fetch_one(self.pool()).await?;

        let mut builder = QueryBuilder::new(
            "SELECT id, environment, workflow_kind, status, outcome,
                NULLIF(concat_ws(' ', genus, species, strain), '') AS organism,
                sequence_count, total_length, created_at, finished_at, runtime_seconds
             FROM jobs",
        );
        push_job_list_filter(&mut builder, query);
        builder
            .push(" ORDER BY ")
            .push(query.sort.column)
            .push(if query.sort.descending {
                " DESC NULLS LAST"
            } else {
                " ASC NULLS LAST"
            })
            .push(", id LIMIT ")
            .push_bind(query.limit)
            .push(" OFFSET ")
            .push_bind(query.offset);
        let items = builder.build_query_as().fetch_all(self.pool()).await?;
        Ok(JobList { total, items })
    }

    pub async fn get_job(&self, id: Uuid) -> Result<Option<JobRecord>> {
        Ok(sqlx::query_as("SELECT * FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?)
    }
}

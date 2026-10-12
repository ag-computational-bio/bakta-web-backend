//! Request and response bodies of the admin API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::stats::queries::{GroupBy, Interval, JobRecord};

/// Error body of all admin endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminErrorResponse {
    pub error: String,
}

/// The logged in admin.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MeResponse {
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub groups: Vec<String>,
}

/// Environment and time range filter used by the statistics endpoints.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatsQuery {
    /// `production` (default), `staging` or `all`.
    pub environment: Option<String>,
    /// RFC 3339 timestamp, inclusive lower bound on `created_at`.
    pub from: Option<DateTime<Utc>>,
    /// RFC 3339 timestamp, exclusive upper bound on `created_at`.
    pub to: Option<DateTime<Utc>>,
}

/// Statistics filter with an optional workflow kind list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct KindStatsQuery {
    /// `production` (default), `staging` or `all`.
    pub environment: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Comma separated list of workflow kinds.
    pub workflow_kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimeseriesQuery {
    /// `production` (default), `staging` or `all`.
    pub environment: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Bucket size, default `month`.
    pub interval: Option<Interval>,
    /// Grouping column, default `none`.
    pub group_by: Option<GroupBy>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JobListParams {
    /// `production` (default), `staging` or `all`.
    pub environment: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Comma separated list of statuses.
    pub status: Option<String>,
    /// Comma separated list of workflow kinds.
    pub workflow_kind: Option<String>,
    /// Comma separated list of live Argo phases.
    pub workflow_phase: Option<String>,
    /// Matches the job ID prefix, organism or name, case insensitive.
    pub q: Option<String>,
    /// Default 50, max 500.
    pub limit: Option<i64>,
    /// Default 0.
    pub offset: Option<i64>,
    /// `created_at` (default, newest first), `-created_at`, `runtime_seconds`,
    /// `-runtime_seconds`, `total_length` or `-total_length`.
    pub sort: Option<String>,
}

/// One node of a live Argo workflow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowNodeView {
    pub name: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub phase: String,
    pub pod_name: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

/// Live state of an Argo workflow.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowView {
    pub name: String,
    pub phase: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
    /// True when resume applies.
    pub suspended: bool,
    pub nodes: Vec<WorkflowNodeView>,
}

/// Database record of a job plus the live Argo state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobDetail {
    pub job: JobRecord,
    /// Null when the workflow no longer exists.
    pub workflow: Option<WorkflowView>,
}

/// Result of a retry, resubmit or stop.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowActionResponse {
    pub workflow_name: String,
    pub phase: String,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteJobParams {
    /// Also delete the job's S3 objects. Default `false`.
    pub purge_data: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeleteJobResponse {
    pub deleted_workflow: bool,
    pub purged_objects: u64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkflowListParams {
    /// Environment, default the backend's own environment, or `all`.
    pub environment: Option<String>,
    /// Comma separated phases: `Pending`, `Running`, `Succeeded`, `Failed`, `Error`.
    pub phase: Option<String>,
    /// Only workflows without a matching job record.
    pub orphans: Option<bool>,
}

/// A live Argo workflow of this backend.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowSummary {
    pub name: String,
    /// Null when the workflow has no `jobid` label.
    pub job_id: Option<String>,
    /// Null when the workflow has no `workflow-kind` label.
    pub workflow_kind: Option<String>,
    pub phase: String,
    pub template: Option<String>,
    /// Environment from the `bakta-environment` label, or derived from the
    /// other labels for workflows without it.
    pub environment: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
    /// True when resume applies.
    pub suspended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowListResponse {
    pub items: Vec<WorkflowSummary>,
}

/// Filter of a bulk action. All fields are optional and combine with AND.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BulkFilter {
    /// Case insensitive substring of the workflow name or job ID.
    pub q: Option<String>,
    pub phases: Option<Vec<String>>,
    pub workflow_kinds: Option<Vec<String>>,
    /// Only workflows that finished more than this many hours ago.
    pub finished_before_hours: Option<f64>,
    /// Default the backend's own environment, or `all`.
    pub environment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkAction {
    Delete,
    Stop,
    Retry,
    Resume,
    Resubmit,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkRequest {
    #[serde(default)]
    pub filter: BulkFilter,
    pub action: BulkAction,
    /// Default `true`: only list the matching workflows.
    pub dry_run: Option<bool>,
    /// When given, only these workflows are processed, if they still match the filter.
    pub workflow_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BulkResult {
    Done,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkItem {
    pub name: String,
    pub job_id: Option<String>,
    pub workflow_kind: Option<String>,
    pub phase: String,
    pub finished_at: Option<DateTime<Utc>>,
    pub applicable: bool,
    /// Null in a dry run.
    pub result: Option<BulkResult>,
    pub error: Option<String>,
    /// Why the action does not apply, null when it applies.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BulkResponse {
    pub dry_run: bool,
    pub action: BulkAction,
    pub items: Vec<BulkItem>,
}

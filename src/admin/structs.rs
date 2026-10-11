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
    /// Environment label, default the backend's own environment, or `all`.
    pub environment: Option<String>,
    /// Comma separated phases: `Pending`, `Running`, `Succeeded`, `Failed`, `Error`.
    pub phase: Option<String>,
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
    pub environment: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowListResponse {
    pub items: Vec<WorkflowSummary>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CleanupRequest {
    /// Default `["Failed", "Error"]`.
    pub phases: Option<Vec<String>>,
    /// Default 24.
    pub older_than_hours: Option<f64>,
    /// Default the backend's own environment, or `all`.
    pub environment: Option<String>,
    /// Default `true`: only list what would be deleted.
    pub dry_run: Option<bool>,
    /// When given, only these workflows are deleted, if they still match the filters.
    pub workflow_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CleanupItem {
    pub name: String,
    pub job_id: Option<String>,
    pub phase: String,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CleanupResponse {
    pub dry_run: bool,
    pub workflows: Vec<CleanupItem>,
}

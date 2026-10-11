//! Request and response bodies of the admin API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::stats::queries::{GroupBy, Interval};

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

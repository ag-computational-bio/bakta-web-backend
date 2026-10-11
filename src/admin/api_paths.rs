//! Handlers of the admin API.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
};

use super::{Admin, AdminError, parse_environment, stats_db, structs::*};
use crate::{
    bakta_handler::BaktaHandler,
    stats::queries::{
        JobList, JobListQuery, JobSort, StatsFilter, StatsSummary, Timeseries, VersionList,
    },
};

const DEFAULT_ENVIRONMENT: &str = "production";
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 500;

fn stats_filter(
    environment: Option<&str>,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<StatsFilter, AdminError> {
    Ok(StatsFilter {
        environment: parse_environment(environment, DEFAULT_ENVIRONMENT)?,
        from,
        to,
    })
}

fn comma_list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// Current user from the access token.
#[utoipa::path(
    get,
    path = "/api/admin/me",
    operation_id = "getAdminMe",
    responses(
        (status = 200, body = MeResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn me(Admin(user): Admin) -> Json<MeResponse> {
    Json(MeResponse {
        username: user.username,
        name: user.name,
        email: user.email,
        groups: user.groups,
    })
}

/// Job counts, success rate, runtimes and input size.
#[utoipa::path(
    get,
    path = "/api/admin/stats/summary",
    operation_id = "getAdminStatsSummary",
    params(StatsQuery),
    responses(
        (status = 200, body = StatsSummary),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stats_summary(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<StatsSummary>, AdminError> {
    let filter = stats_filter(query.environment.as_deref(), query.from, query.to)?;
    let db = stats_db(&state)?;
    Ok(Json(
        db.summary(&filter).await.map_err(AdminError::database)?,
    ))
}

/// Job counts per time bucket, optionally grouped.
#[utoipa::path(
    get,
    path = "/api/admin/stats/timeseries",
    operation_id = "getAdminStatsTimeseries",
    params(TimeseriesQuery),
    responses(
        (status = 200, body = Timeseries),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stats_timeseries(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(query): Query<TimeseriesQuery>,
) -> Result<Json<Timeseries>, AdminError> {
    let filter = stats_filter(query.environment.as_deref(), query.from, query.to)?;
    let db = stats_db(&state)?;
    Ok(Json(
        db.timeseries(
            &filter,
            query.interval.unwrap_or_default(),
            query.group_by.unwrap_or_default(),
        )
        .await
        .map_err(AdminError::database)?,
    ))
}

/// Tool and database versions used by jobs.
#[utoipa::path(
    get,
    path = "/api/admin/stats/versions",
    operation_id = "getAdminStatsVersions",
    params(StatsQuery),
    responses(
        (status = 200, body = VersionList),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stats_versions(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<VersionList>, AdminError> {
    let filter = stats_filter(query.environment.as_deref(), query.from, query.to)?;
    let db = stats_db(&state)?;
    Ok(Json(
        db.versions(&filter).await.map_err(AdminError::database)?,
    ))
}

/// Paged and filtered job list.
#[utoipa::path(
    get,
    path = "/api/admin/jobs",
    operation_id = "listAdminJobs",
    params(JobListParams),
    responses(
        (status = 200, body = JobList),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn list_jobs(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(params): Query<JobListParams>,
) -> Result<Json<JobList>, AdminError> {
    let sort = match params.sort.as_deref().filter(|sort| !sort.is_empty()) {
        Some(sort) => sort
            .parse::<JobSort>()
            .map_err(|e| AdminError::BadRequest(e.to_string()))?,
        None => JobSort::default(),
    };
    let query = JobListQuery {
        filter: stats_filter(params.environment.as_deref(), params.from, params.to)?,
        statuses: comma_list(params.status.as_deref()),
        workflow_kinds: comma_list(params.workflow_kind.as_deref()),
        q: params.q,
        limit: params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        offset: params.offset.unwrap_or(0).max(0),
        sort,
    };
    let db = stats_db(&state)?;
    Ok(Json(
        db.list_jobs(&query).await.map_err(AdminError::database)?,
    ))
}

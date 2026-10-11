//! Handlers of the admin API.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use super::{Admin, AdminError, parse_environment, stats_db, structs::*};
use crate::{
    argo::structs::ArgoWorkflow,
    bakta_handler::{BaktaHandler, argo_v2_pod_name, workflow_kind_from_label_value},
    stats::inputs::InputStats,
    stats::queries::{
        JobList, JobListQuery, JobSort, StatsFilter, StatsSummary, Timeseries, VersionList,
    },
    stats::runtimes::RuntimeStats,
    v2::api_structs::V2LogsResponse,
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

/// Runtime statistics of succeeded jobs.
#[utoipa::path(
    get,
    path = "/api/admin/stats/runtimes",
    operation_id = "getAdminStatsRuntimes",
    params(KindStatsQuery),
    responses(
        (status = 200, body = RuntimeStats),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stats_runtimes(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(query): Query<KindStatsQuery>,
) -> Result<Json<RuntimeStats>, AdminError> {
    let filter = stats_filter(query.environment.as_deref(), query.from, query.to)?;
    let kinds = comma_list(query.workflow_kind.as_deref());
    let db = stats_db(&state)?;
    Ok(Json(
        db.runtimes(&filter, &kinds)
            .await
            .map_err(AdminError::database)?,
    ))
}

/// Input size and sequence count statistics.
#[utoipa::path(
    get,
    path = "/api/admin/stats/inputs",
    operation_id = "getAdminStatsInputs",
    params(KindStatsQuery),
    responses(
        (status = 200, body = InputStats),
        (status = 400, body = AdminErrorResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn stats_inputs(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Query(query): Query<KindStatsQuery>,
) -> Result<Json<InputStats>, AdminError> {
    let filter = stats_filter(query.environment.as_deref(), query.from, query.to)?;
    let kinds = comma_list(query.workflow_kind.as_deref());
    let db = stats_db(&state)?;
    Ok(Json(
        db.inputs(&filter, &kinds)
            .await
            .map_err(AdminError::database)?,
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

pub(crate) fn workflow_view(workflow: ArgoWorkflow) -> WorkflowView {
    let name = workflow.metadata.name;
    let mut nodes: Vec<_> = workflow
        .status
        .nodes
        .into_values()
        .map(|node| WorkflowNodeView {
            pod_name: (node.node_type == "Pod")
                .then(|| argo_v2_pod_name(&name, &node))
                .flatten(),
            name: node.name,
            display_name: node.display_name,
            node_type: node.node_type,
            phase: node.phase,
            started_at: node.started_at,
            finished_at: node.finished_at,
            message: node.message,
        })
        .collect();
    nodes.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    WorkflowView {
        name,
        phase: workflow.status.phase,
        started_at: workflow.status.started_at,
        finished_at: workflow.status.finished_at,
        message: workflow.status.message,
        nodes,
    }
}

/// Workflow name of a job: the in-memory state first, then the database record.
async fn current_workflow_name(
    state: &BaktaHandler,
    id: Uuid,
    fallback: Option<String>,
) -> Option<String> {
    state
        .state_handler
        .job_state
        .read()
        .await
        .get(&id)
        .and_then(|job| job.workflowname.clone())
        .or(fallback)
}

/// Full job record plus the live Argo workflow.
#[utoipa::path(
    get,
    path = "/api/admin/jobs/{id}",
    operation_id = "getAdminJob",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = JobDetail),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 503, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn get_job(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<JobDetail>, AdminError> {
    let db = stats_db(&state)?;
    let job = db
        .get_job(id)
        .await
        .map_err(AdminError::database)?
        .ok_or_else(|| AdminError::NotFound("Job not found".to_string()))?;
    let workflow = match current_workflow_name(&state, id, job.workflow_name.clone()).await {
        Some(name) => match state.state_handler.argo_client.get_workflow(&name).await {
            Ok(workflow) => Some(workflow_view(workflow)),
            Err(error) => {
                tracing::debug!(%error, workflow_name = %name, "admin_workflow_not_available");
                None
            }
        },
        None => None,
    };
    Ok(Json(JobDetail { job, workflow }))
}

/// Structured logs of a job, like the V2 logs endpoint but without the job secret.
#[utoipa::path(
    get,
    path = "/api/admin/jobs/{id}/logs",
    operation_id = "getAdminJobLogs",
    params(("id" = Uuid, Path, description = "Job ID")),
    responses(
        (status = 200, body = V2LogsResponse),
        (status = 401, body = AdminErrorResponse),
        (status = 403, body = AdminErrorResponse),
        (status = 404, body = AdminErrorResponse),
        (status = 502, body = AdminErrorResponse)
    ),
    security(("bearer_auth" = [])),
    tag = "admin",
)]
pub async fn job_logs(
    _admin: Admin,
    State(state): State<Arc<BaktaHandler>>,
    Path(id): Path<Uuid>,
) -> Result<Json<V2LogsResponse>, AdminError> {
    let handler = &state.state_handler;
    let in_memory = handler.job_state.read().await.get(&id).cloned();
    let (kind, workflow_name, status, archived, argo_uid) = match in_memory {
        Some(job) => (
            job.workflow_kind,
            job.workflowname,
            job.status,
            job.archived,
            job.argo_uid,
        ),
        None => {
            let record = match handler.stats.db() {
                Some(db) => db.get_job(id).await.map_err(AdminError::database)?,
                None => None,
            }
            .ok_or_else(|| AdminError::NotFound("Job not found".to_string()))?;
            (
                workflow_kind_from_label_value(Some(&record.workflow_kind)),
                record.workflow_name,
                None,
                false,
                None,
            )
        }
    };
    handler
        .stage_logs(id, kind, workflow_name, status, archived, argo_uid)
        .await
        .map(Json)
        .map_err(|e| AdminError::BadGateway(e.to_string()))
}

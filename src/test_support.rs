//! Shared helpers for handler tests: a router that mirrors main.rs and a
//! handler wired to local mock servers.

use crate::{
    api_structs::ArgoStatus,
    argo::{
        client::ArgoClient,
        structs::{SimpleStatus, SimpleStatusStatus, StatusMetadata},
    },
    bakta_handler::{ApiVersion, BaktaHandler, FullJobState, StateHandler},
    metrics::AppMetrics,
    s3_handler::S3Handler,
    v1, v2,
    v2::api_structs::{ResultKind, V2VersionResponse, WorkflowKind},
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::{delete, get, post},
};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub const TEST_SECRET: &str = "s3cret";

pub fn test_handler(argo_url: &str, s3_url: &str) -> Arc<BaktaHandler> {
    test_handler_with_stats(argo_url, s3_url, crate::stats::StatsRecorder::disabled())
}

pub fn test_handler_with_stats(
    argo_url: &str,
    s3_url: &str,
    stats: crate::stats::StatsRecorder,
) -> Arc<BaktaHandler> {
    test_handler_full(argo_url, s3_url, stats, None)
}

pub fn test_handler_full(
    argo_url: &str,
    s3_url: &str,
    stats: crate::stats::StatsRecorder,
    admin_auth: Option<Arc<crate::admin::auth::AdminAuth>>,
) -> Arc<BaktaHandler> {
    let state_handler = Arc::new(StateHandler {
        job_state: RwLock::new(HashMap::new()),
        argo_client: Arc::new(ArgoClient::new(
            "token".to_string(),
            argo_url.to_string(),
            "bakta".to_string(),
        )),
        metrics: Arc::new(AppMetrics::new()),
        bakta_version: "1.9.0".to_string(),
        baktfold_version: "0.7.0".to_string(),
        starting_jobs: Default::default(),
        unknown_submissions: Default::default(),
        deleted_jobs: Default::default(),
        live_workflows: Default::default(),
        workflows: Default::default(),
        watch_connected: Default::default(),
        ready: Default::default(),
        reconciled: Default::default(),
        lists: Default::default(),
        completions: Default::default(),
        unwritten: Default::default(),
        environment: "production".to_string(),
        stats,
    });
    Arc::new(BaktaHandler {
        admin_auth,
        s3_handler: test_s3_handler(s3_url),
        version: crate::api_structs::VersionResponse {
            tool: "1.9.0".to_string(),
            db: "5.1".to_string(),
            backend: "9.9.9".to_string(),
        },
        version_v2: V2VersionResponse {
            backend_version: "9.9.9".to_string(),
            bakta_version: "1.9.0".to_string(),
            bakta_db_version: "5.1".to_string(),
            baktfold_version: "0.7.0".to_string(),
            baktfold_db_version: "1.0".to_string(),
        },
        state_handler,
        data_retention_days: crate::retention::DEFAULT_DATA_RETENTION_DAYS,
    })
}

/// S3 handler for tests. An `http://s3.test:<port>` endpoint sends the
/// bucket requests to a local mock server on that port.
fn test_s3_handler(s3_url: &str) -> S3Handler {
    let handler = S3Handler::new(
        "access".to_string(),
        "secret".to_string(),
        "bucket".to_string(),
        s3_url.to_string(),
    );
    match s3_url.strip_prefix("http://s3.test:") {
        Some(port) => {
            handler.with_bucket_host_address(([127, 0, 0, 1], port.parse().unwrap()).into())
        }
        None => handler,
    }
}

/// Same routes as main.rs, without swagger, metrics middleware and CORS.
pub fn test_router(handler: Arc<BaktaHandler>) -> Router {
    Router::new()
        .route("/ready", get(crate::metrics::ready))
        .route("/api/v1/job/delete", delete(v1::api_paths::delete_job))
        .route("/api/v1/job/logs", get(v1::api_paths::job_logs))
        .route("/api/v1/job/init", post(v1::api_paths::init_job))
        .route("/api/v1/job/list", post(v1::api_paths::list_jobs))
        .route("/api/v1/job/result", post(v1::api_paths::query_result))
        .route("/api/v1/job/start", post(v1::api_paths::start_job))
        .route("/api/v1/version", get(v1::api_paths::version))
        .route("/api/v2/workflows", get(v2::api_paths::workflows))
        .route("/api/v2/job/delete", delete(v2::api_paths::delete_job))
        .route("/api/v2/job/logs", get(v2::api_paths::job_logs))
        .route("/api/v2/job/init", post(v2::api_paths::init_job))
        .route("/api/v2/job/list", post(v2::api_paths::list_jobs))
        .route("/api/v2/job/result", post(v2::api_paths::query_result))
        .route("/api/v2/job/start", post(v2::api_paths::start_job))
        .route("/api/v2/version", get(v2::api_paths::version))
        .nest("/api/admin", crate::admin::router())
        .with_state(handler)
}

pub async fn call(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
    call_with_headers(router, method, uri, body, &[]).await
}

pub async fn call_with_headers(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    let request = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

pub fn job_state(
    api_version: ApiVersion,
    workflow_kind: WorkflowKind,
    result_kind: ResultKind,
    status: Option<ArgoStatus>,
) -> FullJobState {
    let now = Utc::now();
    FullJobState {
        id: Uuid::new_v4(),
        argo_uid: None,
        argo_ressource_version: None,
        name: "sample".to_string(),
        status,
        started: Some(now),
        updated: Some(now),
        workflowname: None,
        secret: Some(TEST_SECRET.to_string()),
        archived: false,
        api_version,
        workflow_kind,
        result_kind,
    }
}

pub async fn insert_state(handler: &BaktaHandler, state: FullJobState) -> Uuid {
    let id = state.id;
    handler
        .state_handler
        .job_state
        .write()
        .await
        .insert(id, state);
    id
}

pub fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not json: {body}"))
}

pub async fn v2_state(
    handler: &BaktaHandler,
    kind: WorkflowKind,
    status: Option<ArgoStatus>,
) -> Uuid {
    insert_state(
        handler,
        job_state(ApiVersion::V2, kind, ResultKind::Bakta, status),
    )
    .await
}

/// Makes the workflows of an Argo list (`{"items": [...]}`) known like the
/// workflow watch does: in memory and, with a database, in the `workflows`
/// table. The job state is not changed.
pub async fn store_listed_workflows(handler: &BaktaHandler, list: &Value) {
    let mut workflows: Vec<crate::argo::structs::ArgoWorkflow> =
        serde_json::from_value(list["items"].clone()).unwrap();
    // Without creation times, the list order is kept: newest first.
    let now = Utc::now();
    for (index, workflow) in workflows.iter_mut().enumerate() {
        workflow
            .metadata
            .creation_timestamp
            .get_or_insert(now - chrono::Duration::seconds(index as i64));
    }
    handler
        .state_handler
        .workflows
        .write()
        .unwrap()
        .extend(workflows.iter().map(|workflow| {
            let item = simple_status(workflow);
            (item.metadata.name.clone(), item)
        }));
    let Some(db) = handler.state_handler.stats.db() else {
        return;
    };
    db.upsert_workflows("bakta", Utc::now(), &workflows)
        .await
        .unwrap();
}

/// A workflow in the form the watch keeps it.
pub fn simple_status(workflow: &crate::argo::structs::ArgoWorkflow) -> SimpleStatus {
    SimpleStatus {
        metadata: StatusMetadata {
            name: workflow.metadata.name.clone(),
            uid: Uuid::new_v4(),
            resource_version: None,
            creation_timestamp: workflow.metadata.creation_timestamp,
            labels: workflow.metadata.labels.clone(),
        },
        spec: workflow.spec.clone(),
        status: SimpleStatusStatus {
            phase: workflow.status.phase.clone(),
            started_at: workflow
                .status
                .started_at
                .or(workflow.metadata.creation_timestamp)
                .unwrap_or_default(),
            finished_at: workflow.status.finished_at,
            message: workflow.status.message.clone(),
        },
    }
}

/// Answers the single workflow requests of a real bulk run with the
/// workflows of an Argo list (`{"items": [...]}`).
pub async fn mount_workflow_gets(argo: &MockServer, list: &Value) {
    for item in list["items"].as_array().unwrap() {
        let name = item["metadata"]["name"].as_str().unwrap();
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/workflows/bakta/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(item))
            .mount(argo)
            .await;
    }
}

/// Makes every insert and update of job records fail, until dropped.
pub async fn fail_job_writes(db: &crate::stats::StatsDb) {
    sqlx::query(
        "CREATE OR REPLACE FUNCTION fail_job_writes() RETURNS trigger LANGUAGE plpgsql
         AS $$ BEGIN RAISE EXCEPTION 'job records are read only'; END $$",
    )
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_job_writes BEFORE INSERT OR UPDATE ON jobs
         FOR EACH ROW EXECUTE FUNCTION fail_job_writes()",
    )
    .execute(db.pool())
    .await
    .unwrap();
}

pub async fn allow_job_writes(db: &crate::stats::StatsDb) {
    sqlx::query("DROP TRIGGER fail_job_writes ON jobs")
        .execute(db.pool())
        .await
        .unwrap();
}

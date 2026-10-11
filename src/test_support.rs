//! Shared helpers for handler tests: a router that mirrors main.rs and a
//! handler wired to local mock servers.

use crate::{
    api_structs::ArgoStatus,
    argo::client::ArgoClient,
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

pub const TEST_SECRET: &str = "s3cret";

pub fn test_handler(argo_url: &str, s3_url: &str) -> Arc<BaktaHandler> {
    test_handler_with_stats(argo_url, s3_url, crate::stats::StatsRecorder::disabled())
}

pub fn test_handler_with_stats(
    argo_url: &str,
    s3_url: &str,
    stats: crate::stats::StatsRecorder,
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
        environment: "production".to_string(),
        stats,
    });
    Arc::new(BaktaHandler {
        s3_handler: S3Handler::new(
            "access".to_string(),
            "secret".to_string(),
            "bucket".to_string(),
            s3_url.to_string(),
        ),
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
    })
}

/// Same routes as main.rs, without swagger, metrics middleware and CORS.
pub fn test_router(handler: Arc<BaktaHandler>) -> Router {
    Router::new()
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
        secret: TEST_SECRET.to_string(),
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

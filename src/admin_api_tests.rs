//! Router tests for the admin statistics, job list, job detail and logs endpoints.

use std::sync::Arc;

use crate::{
    admin::auth::{AdminAuth, AdminAuthConfig, test_tokens},
    api_structs::ArgoStatus,
    bakta_handler::BaktaHandler,
    stats::{
        JobVersions, NewJob, StatsDb, StatsEvent, StatsRecorder, StatusUpdate, SubmittedJob,
        test_db::{test_db, unique_environment},
    },
    test_support::*,
    v2::api_structs::WorkflowKind,
};
use axum::{Router, http::StatusCode};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub struct AdminSetup {
    pub router: Router,
    pub handler: Arc<BaktaHandler>,
    pub token: String,
    pub issuer: String,
    pub _keycloak: MockServer,
}

/// Router with the admin API enabled, a mock Keycloak and a valid admin token.
pub async fn admin_setup(argo_url: &str, s3_url: &str, db: Option<StatsDb>) -> AdminSetup {
    let keycloak = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/realms/bakta/protocol/openid-connect/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_tokens::jwks()))
        .mount(&keycloak)
        .await;
    let issuer = format!("{}/realms/bakta", keycloak.uri());
    let auth = AdminAuth::new(AdminAuthConfig {
        issuer: issuer.clone(),
        client_id: test_tokens::TEST_CLIENT.to_string(),
        group: test_tokens::TEST_GROUP.to_string(),
    });
    let stats = match db {
        Some(db) => StatsRecorder::new(db, JobVersions::default()),
        None => StatsRecorder::disabled(),
    };
    let handler = test_handler_full(argo_url, s3_url, stats, Some(Arc::new(auth)));
    AdminSetup {
        router: test_router(handler.clone()),
        handler,
        token: test_tokens::token(&test_tokens::claims(&issuer)),
        issuer,
        _keycloak: keycloak,
    }
}

pub async fn admin_call(
    setup: &AdminSetup,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let bearer = format!("Bearer {}", setup.token);
    let (status, text) = call_with_headers(
        &setup.router,
        method,
        uri,
        body,
        &[("authorization", bearer.as_str())],
    )
    .await;
    let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, value)
}

fn new_job(environment: &str, kind: &str, created_minutes_ago: i64) -> NewJob {
    NewJob {
        id: Uuid::new_v4(),
        environment: environment.to_string(),
        workflow_kind: kind.to_string(),
        api_version: "v2".to_string(),
        name: Some("sample".to_string()),
        created_at: Utc::now() - Duration::minutes(created_minutes_ago),
    }
}

/// Inserts a finished job and returns its ID.
async fn finished_job(db: &StatsDb, environment: &str, kind: &str, status: &'static str) -> Uuid {
    let job = new_job(environment, kind, 60);
    let id = job.id;
    db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
    db.apply(&StatsEvent::Submitted(Box::new(SubmittedJob {
        job: job.clone(),
        genus: Some("Escherichia".to_string()),
        species: Some("coli".to_string()),
        strain: None,
        sequence_count: Some(2),
        total_length: Some(1000),
        config: Some(json!({"complete_genome": true})),
        versions: JobVersions {
            bakta_version: Some("1.12.2".to_string()),
            ..Default::default()
        },
        workflow_name: format!("wf-{id}"),
        submitted_at: job.created_at,
    })))
    .await
    .unwrap();
    db.apply(&StatsEvent::Status(StatusUpdate {
        job: job.clone(),
        workflow_name: format!("wf-{id}"),
        status,
        started_at: job.created_at + Duration::seconds(10),
        finished_at: Some(job.created_at + Duration::seconds(110)),
        error_message: None,
    }))
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn admin_api_requires_configuration_and_a_valid_admin_token() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, body) = call(&router, "GET", "/api/admin/me", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("\"error\""), "{body}");

    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", None).await;
    let (status, _) = call(&setup.router, "GET", "/api/admin/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&setup.router, "GET", "/api/admin/stats/summary", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let mut claims = test_tokens::claims(&setup.issuer);
    claims["groups"] = json!(["users"]);
    let bearer = format!("Bearer {}", test_tokens::token(&claims));
    let (status, body) = call_with_headers(
        &setup.router,
        "GET",
        "/api/admin/me",
        None,
        &[("authorization", bearer.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, me) = admin_call(&setup, "GET", "/api/admin/me", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me,
        json!({"username": "jdoe", "name": "Jane Doe", "email": "jdoe@example.org",
               "groups": ["bakta-admin", "other"]})
    );

    for uri in [
        "/api/admin/stats/summary",
        "/api/admin/stats/timeseries",
        "/api/admin/stats/versions",
        "/api/admin/jobs",
        "/api/admin/jobs/00000000-0000-0000-0000-000000000000",
    ] {
        let (status, body) = admin_call(&setup, "GET", uri, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert!(body["error"].as_str().unwrap().contains("not configured"));
    }
}

#[tokio::test]
async fn admin_stats_endpoints_return_the_contract_shapes() {
    let Some(db) = test_db("admin_stats_endpoints_return_the_contract_shapes").await else {
        return;
    };
    let env = unique_environment();
    finished_job(&db, &env, "bakta", "succeeded").await;
    finished_job(&db, &env, "bakta", "failed").await;
    finished_job(&db, &env, "baktfold", "succeeded").await;
    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", Some(db)).await;

    let (status, summary) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/stats/summary?environment={env}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["jobs_total"], 3);
    assert_eq!(summary["jobs_succeeded"], 2);
    assert_eq!(summary["runtime_seconds"]["p50"], 100.0);
    assert_eq!(summary["total_input_bp"], 3000);
    assert_eq!(summary["by_workflow_kind"][0]["workflow_kind"], "bakta");

    let (status, empty) = admin_call(
        &setup,
        "GET",
        "/api/admin/stats/summary?environment=nothing-here",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["success_rate"], Value::Null);
    assert_eq!(empty["runtime_seconds"]["avg"], Value::Null);

    let (status, series) = admin_call(
        &setup,
        "GET",
        &format!(
            "/api/admin/stats/timeseries?environment={env}&interval=year&group_by=workflow_kind"
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{series}");
    assert_eq!(series["interval"], "year");
    assert_eq!(series["group_by"], "workflow_kind");
    assert_eq!(series["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(series["buckets"][0]["group"], "bakta");
    assert_eq!(series["buckets"][0]["jobs_failed"], 1);
    assert!(
        series["buckets"][0]["period_start"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );

    let (status, versions) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/stats/versions?environment={env}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(versions["items"][0]["bakta_version"], "1.12.2");
    assert_eq!(versions["items"][0]["jobs_total"], 3);

    let (status, runtimes) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/stats/runtimes?environment={env}&workflow_kind=bakta"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{runtimes}");
    assert_eq!(runtimes["jobs"], 1);
    assert_eq!(runtimes["by_workflow_kind"][0]["p50"], 100.0);
    assert_eq!(runtimes["histogram"].as_array().unwrap().len(), 9);
    assert_eq!(runtimes["histogram"][0]["jobs"], 1);
    assert_eq!(runtimes["by_input_size"][0]["max_bp"], 1_000_000);
    assert_eq!(runtimes["slowest"][0]["organism"], "Escherichia coli");

    for uri in [
        "/api/admin/stats/summary?environment=bad%20env",
        "/api/admin/stats/timeseries?interval=hour",
        "/api/admin/stats/summary?from=yesterday",
    ] {
        let (status, _) = admin_call(&setup, "GET", uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn admin_job_list_filters_and_pages() {
    let Some(db) = test_db("admin_job_list_filters_and_pages").await else {
        return;
    };
    let env = unique_environment();
    let first = finished_job(&db, &env, "bakta", "succeeded").await;
    finished_job(&db, &env, "bakta", "failed").await;
    finished_job(&db, &env, "baktfold", "error").await;
    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", Some(db)).await;

    let (status, list) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/jobs?environment={env}&status=succeeded,failed&workflow_kind=bakta&limit=1&sort=-runtime_seconds"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 2);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["organism"], "Escherichia coli");
    assert_eq!(list["items"][0]["total_length"], 1000);

    let prefix = &first.to_string()[..8];
    let (_, found) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/jobs?environment=all&q={prefix}"),
        None,
    )
    .await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["id"], first.to_string());

    let (status, _) = admin_call(&setup, "GET", "/api/admin/jobs?sort=name", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_job_detail_includes_the_live_workflow() {
    let Some(db) = test_db("admin_job_detail_includes_the_live_workflow").await else {
        return;
    };
    let env = unique_environment();
    let id = finished_job(&db, &env, "bakta", "succeeded").await;
    let gone = finished_job(&db, &env, "bakta", "failed").await;
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/workflows/bakta/wf-{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": format!("wf-{id}")},
            "status": {"phase": "Succeeded", "startedAt": "2026-01-01T00:00:00Z",
                       "finishedAt": "2026-01-01T00:05:00Z",
                       "nodes": {
                           "root": {"id": "root", "name": format!("wf-{id}"), "displayName": "wf",
                                    "type": "Steps", "phase": "Succeeded",
                                    "startedAt": "2026-01-01T00:00:00Z"},
                           "pod": {"id": "pod", "name": format!("wf-{id}[0].bakta"), "displayName": "bakta",
                                   "templateName": "bakta", "type": "Pod", "phase": "Succeeded",
                                   "startedAt": "2026-01-01T00:00:01Z", "message": "done"}
                       }}
        })))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db)).await;

    let (status, detail) = admin_call(&setup, "GET", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["job"]["id"], id.to_string());
    assert_eq!(detail["job"]["status"], "succeeded");
    assert_eq!(detail["job"]["config"]["complete_genome"], true);
    assert_eq!(detail["job"]["runtime_seconds"], 100.0);
    let workflow = &detail["workflow"];
    assert_eq!(workflow["phase"], "Succeeded");
    assert_eq!(workflow["nodes"][0]["type"], "Steps");
    assert_eq!(workflow["nodes"][0]["pod_name"], Value::Null);
    assert_eq!(workflow["nodes"][1]["display_name"], "bakta");
    assert_eq!(workflow["nodes"][1]["message"], "done");
    let pod_name = workflow["nodes"][1]["pod_name"].as_str().unwrap();
    assert!(
        pod_name.starts_with(&format!("wf-{id}-bakta-")),
        "{pod_name}"
    );

    let (status, detail) =
        admin_call(&setup, "GET", &format!("/api/admin/jobs/{gone}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["workflow"], Value::Null);

    let (status, body) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/jobs/{}", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Job not found");
}

#[tokio::test]
async fn admin_job_logs_do_not_need_the_secret() {
    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", None).await;
    let id = v2_state(
        &setup.handler,
        WorkflowKind::BaktaBaktfold,
        Some(ArgoStatus::Pending),
    )
    .await;
    let (status, logs) =
        admin_call(&setup, "GET", &format!("/api/admin/jobs/{id}/logs"), None).await;
    assert_eq!(status, StatusCode::OK, "{logs}");
    assert_eq!(logs["workflow_kind"], "bakta_baktfold");
    assert_eq!(logs["stages"][1]["stage"], "baktfold");
    assert_eq!(logs["stages"][0]["status"], "pending");

    let (status, _) = admin_call(
        &setup,
        "GET",
        &format!("/api/admin/jobs/{}/logs", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

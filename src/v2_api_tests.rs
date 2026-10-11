//! Router-level tests for the V2 API with mock Argo and S3 servers.

use crate::{
    api_structs::ArgoStatus,
    bakta_handler::ApiVersion,
    test_support::*,
    v2::api_structs::{ResultKind, WorkflowKind},
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn submit_response(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "metadata": {"name": name, "creationTimestamp": "2026-01-01T00:00:00Z"}
    }))
}

#[tokio::test]
async fn workflows_lists_the_catalog() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let router = test_router(handler);
    let (status, body) = call(&router, "GET", "/api/v2/workflows", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = parse(&body);
    let kinds: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["workflow_kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        ["bakta", "bakta_proteins", "bakta_baktfold", "baktfold"]
    );
    assert_eq!(list[0]["uploads"][0]["upload_kind"], "genome_fasta");
    assert_eq!(list[0]["uploads"][0]["required"], true);
    assert_eq!(list[2]["stages"], json!(["bakta", "baktfold"]));
    assert_eq!(list[3]["result_kind"], "baktfold");
}

#[tokio::test]
async fn version_returns_configured_versions() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, body) = call(&router, "GET", "/api/v2/version", None).await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["backend_version"], "9.9.9");
    assert_eq!(v["baktfold_version"], "0.7.0");
}

#[tokio::test]
async fn init_returns_signed_uploads_for_each_workflow() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let router = test_router(handler.clone());
    for (kind, uploads, first) in [
        ("bakta", 6, "fastadata.fasta"),
        ("bakta_baktfold", 6, "fastadata.fasta"),
        ("bakta_proteins", 1, "proteins.fasta"),
        ("baktfold", 1, "bakta.json"),
    ] {
        let (status, body) = call(
            &router,
            "POST",
            "/api/v2/job/init",
            Some(json!({"name": "my sample", "workflow_kind": kind})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kind}: {body}");
        let v = parse(&body);
        assert_eq!(v["workflow_kind"], kind);
        assert_eq!(v["uploads"].as_array().unwrap().len(), uploads);
        let url = v["uploads"][0]["url"].as_str().unwrap();
        assert!(url.starts_with("https://bucket.s3.example.com/"), "{url}");
        assert!(url.contains(&format!(
            "jobs/{}/inputs/{first}",
            v["job"]["job_id"].as_str().unwrap()
        )));
        assert!(url.contains("X-Amz-Signature="));
        assert_eq!(v["job"]["secret"].as_str().unwrap().len(), 32);
    }
    assert_eq!(handler.state_handler.job_state.read().await.len(), 4);
}

#[tokio::test]
async fn init_stores_sanitized_v2_job_state() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let router = test_router(handler.clone());
    let (_, body) = call(
        &router,
        "POST",
        "/api/v2/job/init",
        Some(json!({"name": "my sample!!", "workflow_kind": "baktfold"})),
    )
    .await;
    let id: Uuid = parse(&body)["job"]["job_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let states = handler.state_handler.job_state.read().await;
    let state = states.get(&id).unwrap();
    assert_eq!(state.name, "my_sample");
    assert_eq!(state.api_version, ApiVersion::V2);
    assert_eq!(state.workflow_kind, WorkflowKind::Baktfold);
    assert_eq!(state.result_kind, ResultKind::Baktfold);
    assert!(state.status.is_none());
}

#[tokio::test]
async fn init_rejects_invalid_requests() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/init",
        Some(json!({"name": "x", "workflow_kind": "nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/init",
        Some(json!({"workflow_kind": "bakta"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(&router, "POST", "/api/v2/job/init", None).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn init_fails_with_bad_request_when_signing_fails() {
    let router = test_router(test_handler("http://127.0.0.1:1", "not a url"));
    let (status, body) = call(
        &router,
        "POST",
        "/api/v2/job/init",
        Some(json!({"name": "x", "workflow_kind": "bakta"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Failed to sign V2 upload URL"));
}

#[tokio::test]
async fn list_reports_jobs_and_failures() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let running = v2_state(&handler, WorkflowKind::Bakta, Some(ArgoStatus::Running)).await;
    let done = v2_state(
        &handler,
        WorkflowKind::Baktfold,
        Some(ArgoStatus::Succeeded),
    )
    .await;
    let created = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let missing = Uuid::new_v4();
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v2/job/list",
        Some(json!({"jobs": [
            {"job_id": running, "secret": TEST_SECRET},
            {"job_id": done, "secret": TEST_SECRET},
            {"job_id": done, "secret": "wrong"},
            {"job_id": missing, "secret": TEST_SECRET},
            {"job_id": created, "secret": TEST_SECRET},
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    let jobs = v["jobs"].as_array().unwrap();
    // A job that was never started has no status and is not listed.
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["status"], "running");
    assert_eq!(jobs[1]["status"], "successful");
    assert_eq!(jobs[1]["workflow_kind"], "baktfold");
    let failed = v["failed_jobs"].as_array().unwrap();
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[0]["status"], "unauthorized");
    assert_eq!(failed[1]["status"], "not_found");
}

#[tokio::test]
async fn list_rejects_malformed_job_ids() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/list",
        Some(json!({"jobs": [{"job_id": "not-a-uuid", "secret": "x"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

fn start_body(id: Uuid, secret: &str, kind: &str, config: Value) -> Value {
    json!({
        "job": {"job_id": id, "secret": secret},
        "workflow_kind": kind,
        "config": config,
    })
}

#[tokio::test]
async fn start_submits_workflow_and_updates_state() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(submit_response("bakta-baktfold-job-abc"))
        .expect(1)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::BaktaBaktfold, None).await;
    let router = test_router(handler.clone());
    let (status, body) = call_with_headers(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(start_body(
            id,
            TEST_SECRET,
            "bakta_baktfold",
            json!({"translation_table": 4, "complete_genome": true}),
        )),
        &[("origin", "https://example.org:8080")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let requests = argo.received_requests().await.unwrap();
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["resourceName"], "bakta-baktfold-job-1.9.0-0.7.0");
    assert_eq!(sent["namespace"], "bakta");
    let options = &sent["submitOptions"];
    let labels = options["labels"].as_str().unwrap();
    assert!(labels.contains("workflow-kind=bakta_baktfold"));
    assert!(labels.contains("api-version=v2"));
    assert!(labels.contains(&format!("jobid={id}")));
    assert!(labels.contains("origin=example.org_8080"));
    let params = options["parameters"].as_array().unwrap();
    assert!(params.iter().any(|p| {
        let p = p.as_str().unwrap();
        p.starts_with("parameter=")
            && p.contains("--complete")
            && p.contains("--translation-table 4")
    }));
    assert!(
        options["generateName"]
            .as_str()
            .unwrap()
            .starts_with("bakta-baktfold-job-1.9.0-0.7.0-")
    );
    assert_eq!(
        requests[0].headers.get("authorization").unwrap(),
        "Bearer token"
    );

    let states = handler.state_handler.job_state.read().await;
    let state = states.get(&id).unwrap();
    assert!(matches!(state.status, Some(ArgoStatus::Pending)));
    assert_eq!(
        state.workflowname.as_deref(),
        Some("bakta-baktfold-job-abc")
    );
    assert_eq!(state.result_kind, ResultKind::Bakta);
}

#[tokio::test]
async fn start_uses_unknown_origin_without_header() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(submit_response("wf"))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Baktfold, None).await;
    let router = test_router(handler);
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(start_body(id, TEST_SECRET, "baktfold", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let requests = argo.received_requests().await.unwrap();
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["resourceName"], "baktfold-job-0.7.0");
    assert!(
        sent["submitOptions"]["labels"]
            .as_str()
            .unwrap()
            .contains("origin=Unknown")
    );
}

#[tokio::test]
async fn start_validation_errors_do_not_reach_argo() {
    let argo = MockServer::start().await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let bakta = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let v1_job = insert_state(
        &handler,
        job_state(ApiVersion::V1, WorkflowKind::Bakta, ResultKind::Bakta, None),
    )
    .await;
    let router = test_router(handler);

    let cases = [
        (
            start_body(bakta, "wrong", "bakta", json!({})),
            "Unauthorized",
        ),
        (
            start_body(Uuid::new_v4(), TEST_SECRET, "bakta", json!({})),
            "Job not found",
        ),
        (
            start_body(v1_job, TEST_SECRET, "bakta", json!({})),
            "Wrong API version",
        ),
        (
            start_body(bakta, TEST_SECRET, "baktfold", json!({})),
            "Workflow kind mismatch",
        ),
        (
            start_body(bakta, TEST_SECRET, "bakta", json!({"translation_table": 7})),
            "translation",
        ),
    ];
    for (body, expected) in cases {
        let (status, text) = call(&router, "POST", "/api/v2/job/start", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{expected}: {text}");
        assert!(text.contains(expected), "{expected} not in {text}");
    }
    assert!(argo.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn start_rejects_unknown_workflow_kind_in_body() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(start_body(Uuid::new_v4(), "x", "other", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn start_returns_bad_request_and_keeps_state_when_argo_fails() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let router = test_router(handler.clone());
    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(start_body(id, TEST_SECRET, "bakta", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let states = handler.state_handler.job_state.read().await;
    let state = states.get(&id).unwrap();
    assert!(state.status.is_none());
    assert!(state.workflowname.is_none());
}

#[tokio::test]
async fn start_twice_is_rejected_with_conflict() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(submit_response("wf"))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let router = test_router(handler);
    let body = start_body(id, TEST_SECRET, "bakta", json!({}));
    let (status, _) = call(&router, "POST", "/api/v2/job/start", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, text) = call(&router, "POST", "/api/v2/job/start", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("already started"), "{text}");
    assert_eq!(argo.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn start_does_not_block_other_requests_while_argo_is_slow() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(submit_response("wf").set_delay(std::time::Duration::from_millis(1500)))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let router = test_router(handler);
    let body = start_body(id, TEST_SECRET, "bakta", json!({}));

    let first = {
        let router = router.clone();
        let body = body.clone();
        tokio::spawn(async move { call(&router, "POST", "/api/v2/job/start", Some(body)).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let list = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        call(
            &router,
            "POST",
            "/api/v2/job/list",
            Some(json!({"jobs": [{"job_id": id, "secret": TEST_SECRET}]})),
        ),
    )
    .await;
    assert!(
        list.is_ok(),
        "list request was blocked by the start request"
    );

    let (status, text) = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        call(&router, "POST", "/api/v2/job/start", Some(body)),
    )
    .await
    .expect("second start was blocked by the first one");
    assert_eq!(status, StatusCode::CONFLICT, "{text}");

    let (status, _) = first.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(argo.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn result_rejects_jobs_that_were_never_started() {
    let s3 = MockServer::start().await;
    let handler = test_handler("http://127.0.0.1:1", &s3.uri());
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v2/job/result",
        Some(json!({"job_id": id, "secret": TEST_SECRET})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("Job not finished"), "{body}");
}

async fn submit_lost_argo(id: Uuid, lookup: ResponseTemplate) -> MockServer {
    let argo = MockServer::start().await;
    // Argo creates the workflow, but the response is broken.
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(wiremock::matchers::query_param(
            "listOptions.labelSelector",
            format!("jobid={id}"),
        ))
        .respond_with(lookup)
        .mount(&argo)
        .await;
    argo
}

fn submit_count(requests: &[wiremock::Request]) -> usize {
    requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .count()
}

#[tokio::test]
async fn start_records_a_workflow_created_despite_a_broken_response() {
    let id = Uuid::new_v4();
    let argo = submit_lost_argo(
        id,
        ResponseTemplate::new(200).set_body_json(json!({"items": [{
            "metadata": {"name": "wf-found", "creationTimestamp": "2026-01-01T00:00:00Z",
                         "labels": {"jobid": id.to_string()}}
        }]})),
    )
    .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let state = job_state(ApiVersion::V2, WorkflowKind::Bakta, ResultKind::Bakta, None);
    let id = insert_state(&handler, crate::bakta_handler::FullJobState { id, ..state }).await;
    let router = test_router(handler.clone());
    let body = start_body(id, TEST_SECRET, "bakta", json!({}));

    let (status, text) = call(&router, "POST", "/api/v2/job/start", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let states = handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-found"));
    drop(states);
    let (status, _) = call(&router, "POST", "/api/v2/job/start", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(submit_count(&argo.received_requests().await.unwrap()), 1);
}

#[tokio::test]
async fn start_is_refused_while_the_result_of_a_submit_is_unknown() {
    let id = Uuid::new_v4();
    let argo = submit_lost_argo(id, ResponseTemplate::new(500)).await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let state = job_state(ApiVersion::V2, WorkflowKind::Bakta, ResultKind::Bakta, None);
    insert_state(&handler, crate::bakta_handler::FullJobState { id, ..state }).await;
    let router = test_router(handler.clone());
    let body = start_body(id, TEST_SECRET, "bakta", json!({}));

    for _ in 0..2 {
        let (status, text) = call(&router, "POST", "/api/v2/job/start", Some(body.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert!(text.contains("not known"), "{text}");
    }
    assert_eq!(submit_count(&argo.received_requests().await.unwrap()), 1);

    // A later poll without a workflow for the job resolves it.
    handler
        .state_handler
        .apply_workflow_statuses(vec![], chrono::Utc::now() + chrono::Duration::seconds(1))
        .await;
    let (status, _) = call(&router, "POST", "/api/v2/job/start", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(submit_count(&argo.received_requests().await.unwrap()), 2);
}

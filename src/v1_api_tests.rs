//! Router-level tests for the V1 API with mock Argo and S3 servers.

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

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| panic!("not json: {body}"))
}

fn v1_state(status: Option<ArgoStatus>) -> crate::bakta_handler::FullJobState {
    job_state(
        ApiVersion::V1,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        status,
    )
}

#[tokio::test]
async fn version_returns_configured_versions() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, body) = call(&router, "GET", "/api/v1/version", None).await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["toolVersion"], "1.9.0");
    assert_eq!(v["dbVersion"], "5.1");
    assert_eq!(v["backendVersion"], "9.9.9");
}

#[tokio::test]
async fn init_returns_three_upload_links() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let router = test_router(handler.clone());
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/init",
        Some(json!({"name": "a b", "repliconTableType": "TSV"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = parse(&body);
    let id = v["job"]["jobID"].as_str().unwrap();
    for (field, file) in [
        ("uploadLinkFasta", "fastadata.fasta"),
        ("uploadLinkProdigal", "prodigal.tf"),
        ("uploadLinkReplicons", "replicons.tsv"),
    ] {
        let url = v[field].as_str().unwrap();
        assert!(url.contains(&format!("jobs/{id}/inputs/{file}")), "{url}");
    }
    let states = handler.state_handler.job_state.read().await;
    let state = states.values().next().unwrap();
    assert_eq!(state.name, "a_b");
    assert_eq!(state.api_version, ApiVersion::V1);
}

#[tokio::test]
async fn init_rejects_bad_requests_and_signing_errors() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, _) = call(
        &router,
        "POST",
        "/api/v1/job/init",
        Some(json!({"name": "x", "repliconTableType": "XLS"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let router = test_router(test_handler("http://127.0.0.1:1", "not a url"));
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/init",
        Some(json!({"name": "x", "repliconTableType": "CSV"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Failed to sign URL"));
}

#[tokio::test]
async fn list_maps_statuses_and_failures() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let running = insert_state(&handler, v1_state(Some(ArgoStatus::Running))).await;
    let failed = insert_state(&handler, v1_state(Some(ArgoStatus::Failed))).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/list",
        Some(json!({"jobs": [
            {"jobID": running, "secret": TEST_SECRET},
            {"jobID": failed, "secret": TEST_SECRET},
            {"jobID": failed, "secret": "wrong"},
            {"jobID": Uuid::new_v4(), "secret": "x"},
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["jobs"][0]["jobStatus"], "RUNNING");
    assert_eq!(v["jobs"][1]["jobStatus"], "ERROR");
    assert_eq!(v["failedJobs"][0]["jobStatus"], "UNAUTHORIZED");
    assert_eq!(v["failedJobs"][1]["jobStatus"], "NOT_FOUND");
}

fn v1_start_body(id: Uuid, secret: &str) -> Value {
    json!({
        "job": {"jobID": id, "secret": secret},
        "config": {
            "hasReplicons": false, "translationTable": 11, "completeGenome": false,
            "keepContigHeaders": false, "minContigLength": 1, "compliant": false
        }
    })
}

#[tokio::test]
async fn start_submits_the_v1_template() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": "bakta-job-x", "creationTimestamp": "2026-01-01T00:00:00Z"}
        })))
        .expect(1)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = insert_state(&handler, v1_state(None)).await;
    let router = test_router(handler.clone());
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/start",
        Some(v1_start_body(id, TEST_SECRET)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = argo.received_requests().await.unwrap();
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["resourceName"], "bakta-job-1.9.0");
    assert!(
        sent["submitOptions"]["labels"]
            .as_str()
            .unwrap()
            .contains("api-version=v1")
    );
    let states = handler.state_handler.job_state.read().await;
    assert!(matches!(states[&id].status, Some(ArgoStatus::Pending)));
    assert_eq!(states[&id].workflowname.as_deref(), Some("bakta-job-x"));
}

#[tokio::test]
async fn start_rejects_wrong_secret_and_bad_body() {
    let argo = MockServer::start().await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = insert_state(&handler, v1_state(None)).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/start",
        Some(v1_start_body(id, "wrong")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Unauthorized"));
    let (status, _) = call(
        &router,
        "POST",
        "/api/v1/job/start",
        Some(json!({"job": {"jobID": id, "secret": TEST_SECRET}})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(argo.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn logs_cover_unstarted_error_and_missing_jobs() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let fresh = insert_state(&handler, v1_state(None)).await;
    let errored = insert_state(&handler, v1_state(Some(ArgoStatus::Error))).await;
    let router = test_router(handler);

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v1/job/logs?jobID={fresh}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, ""));

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v1/job/logs?jobID={errored}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Internal server error"));

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v1/job/logs?jobID={}&secret=x", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Job not found"));

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v1/job/logs?jobID={fresh}&secret=wrong"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Unauthorized"));
}

#[tokio::test]
async fn logs_join_entries_from_argo() {
    let argo = MockServer::start().await;
    let line = json!({"result": {"content": "hello", "podName": "p"}}).to_string();
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf/log"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!("{line}\nplain line\n")))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let mut state = v1_state(Some(ArgoStatus::Running));
    state.workflowname = Some("wf".to_string());
    let id = insert_state(&handler, state).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v1/job/logs?jobID={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "hello\nplain line\n");
}

#[tokio::test]
async fn result_checks_secret_and_status_and_signs_all_files() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let running = insert_state(&handler, v1_state(Some(ArgoStatus::Running))).await;
    let done = insert_state(&handler, v1_state(Some(ArgoStatus::Succeeded))).await;
    let router = test_router(handler);

    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/result",
        Some(json!({"jobID": running, "secret": TEST_SECRET})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Job not finished"));

    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/result",
        Some(json!({"jobID": done, "secret": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Unauthorized"));

    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/result",
        Some(json!({"jobID": done, "secret": TEST_SECRET})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = parse(&body);
    assert_eq!(v["name"], "sample");
    let files = v["ResultFiles"].as_object().unwrap();
    assert_eq!(files.len(), 14);
    assert!(
        files
            .values()
            .all(|u| u.as_str().unwrap().contains("X-Amz-Signature="))
    );
}

#[tokio::test]
async fn delete_checks_secret_and_removes_state() {
    // The job has no workflow name, so the delete looks for its workflows.
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .expect(1)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = insert_state(&handler, v1_state(None)).await;
    let router = test_router(handler.clone());
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v1/job/delete?jobID={id}&secret=wrong"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v1/job/delete?jobID={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(handler.state_handler.job_state.read().await.is_empty());
}

#[tokio::test]
async fn start_twice_is_rejected_with_conflict() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": "bakta-job-x", "creationTimestamp": "2026-01-01T00:00:00Z"}
        })))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = insert_state(&handler, v1_state(None)).await;
    let router = test_router(handler);
    let body = v1_start_body(id, TEST_SECRET);
    let (status, _) = call(&router, "POST", "/api/v1/job/start", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, text) = call(&router, "POST", "/api/v1/job/start", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(text.contains("already started"), "{text}");
    assert_eq!(argo.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn start_rejects_jobs_with_non_bakta_workflow_kinds() {
    let argo = MockServer::start().await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let router = test_router(handler.clone());
    for kind in [
        WorkflowKind::BaktaBaktfold,
        WorkflowKind::BaktaProteins,
        WorkflowKind::Baktfold,
    ] {
        let id = insert_state(
            &handler,
            job_state(ApiVersion::V2, kind, ResultKind::Bakta, None),
        )
        .await;
        let (status, text) = call(
            &router,
            "POST",
            "/api/v1/job/start",
            Some(v1_start_body(id, TEST_SECRET)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{kind:?}: {text}");
        assert!(text.contains("not supported by the V1 API"), "{text}");
        let states = handler.state_handler.job_state.read().await;
        assert_eq!(states[&id].workflow_kind, kind);
    }
    assert!(argo.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn result_rejects_jobs_that_were_never_started() {
    let s3 = MockServer::start().await;
    let handler = test_handler("http://127.0.0.1:1", &s3.uri());
    let id = insert_state(&handler, v1_state(None)).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v1/job/result",
        Some(json!({"jobID": id, "secret": TEST_SECRET})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("Job not finished"), "{body}");
}

#[tokio::test]
async fn start_records_a_workflow_created_despite_a_broken_response() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = insert_state(&handler, v1_state(None)).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(wiremock::matchers::query_param(
            "listOptions.labelSelector",
            format!("jobid={id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [{
            "metadata": {"name": "bakta-job-found", "labels": {"jobid": id.to_string()}}
        }]})))
        .mount(&argo)
        .await;
    let router = test_router(handler.clone());
    let body = v1_start_body(id, TEST_SECRET);
    let (status, text) = call(&router, "POST", "/api/v1/job/start", Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let states = handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("bakta-job-found"));
    drop(states);
    let (status, _) = call(&router, "POST", "/api/v1/job/start", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

//! Router-level tests for the V2 job log, result and delete endpoints with mock Argo and S3 servers.

use crate::{
    api_structs::ArgoStatus,
    bakta_handler::ApiVersion,
    test_support::*,
    v2::api_structs::{ResultKind, WorkflowKind},
};
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn logs_for_unstarted_job_list_pending_stages() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::BaktaBaktfold, None).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v2/job/logs?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v = parse(&body);
    assert_eq!(v["workflow_kind"], "bakta_baktfold");
    assert_eq!(v["stages"][0]["stage"], "bakta");
    assert_eq!(v["stages"][0]["status"], "pending");
    assert_eq!(v["stages"][1]["stage"], "baktfold");
    assert_eq!(v["stages"][1]["content"], "");
}

#[tokio::test]
async fn logs_group_pod_output_by_stage() {
    let argo = MockServer::start().await;
    let wf = "wf-1";
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/workflows/bakta/{wf}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": {"nodes": {
                "a": {"id": "a", "name": "wf-1.bakta", "displayName": "bakta",
                      "templateName": "bakta", "type": "Pod", "phase": "Succeeded"},
                "b": {"id": "wf-1-baktfold-pod", "name": "wf-1.baktfold",
                      "displayName": "baktfold", "templateName": "baktfold",
                      "type": "Pod", "phase": "Running"}
            }}
        })))
        .mount(&argo)
        .await;
    let line1 = json!({"result": {"content": "bakta done", "podName": "a"}}).to_string();
    let line2 =
        json!({"result": {"content": "fold busy", "podName": "wf-1-baktfold-pod"}}).to_string();
    let noise = json!({"result": {"content": "argo=true", "podName": "a"}}).to_string();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/workflows/bakta/{wf}/log")))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(format!("{line1}\n{noise}\n\n{line2}\n")),
        )
        .mount(&argo)
        .await;

    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::BaktaBaktfold,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some(wf.to_string());
    let id = insert_state(&handler, state).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v2/job/logs?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v = parse(&body);
    assert_eq!(v["stages"][0]["status"], "succeeded");
    assert_eq!(v["stages"][0]["content"], "bakta done");
    assert_eq!(v["stages"][1]["status"], "running");
    assert_eq!(v["stages"][1]["content"], "fold busy");
}

#[tokio::test]
async fn logs_fail_when_argo_is_unreachable_or_job_is_invalid() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some("wf".to_string());
    let id = insert_state(&handler, state).await;
    let router = test_router(handler);

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v2/job/logs?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Failed to retrieve workflow logs"));

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v2/job/logs?job_id={id}&secret=wrong"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Unauthorized"));

    let (status, body) = call(
        &router,
        "GET",
        &format!("/api/v2/job/logs?job_id={}&secret=x", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Job not found"));

    let (status, _) = call(&router, "GET", "/api/v2/job/logs", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn result_requires_finished_job_and_correct_secret() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let running = v2_state(&handler, WorkflowKind::Bakta, Some(ArgoStatus::Running)).await;
    let router = test_router(handler);
    for (id, secret, expected) in [
        (running, TEST_SECRET, "Job not finished"),
        (running, "wrong", "Unauthorized"),
        (Uuid::new_v4(), TEST_SECRET, "Job not found"),
    ] {
        let (status, body) = call(
            &router,
            "POST",
            "/api/v2/job/result",
            Some(json!({"job_id": id, "secret": secret})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(expected), "{body}");
    }
}

#[tokio::test]
async fn delete_removes_state_and_calls_argo() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-del"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some("wf-del".to_string());
    let id = insert_state(&handler, state).await;
    let router = test_router(handler.clone());

    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret=wrong"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Unauthorized"));
    assert!(
        handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn delete_keeps_state_when_argo_fails() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some("wf".to_string());
    let id = insert_state(&handler, state).await;
    let router = test_router(handler.clone());
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn delete_is_refused_while_a_submission_is_unknown() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    handler
        .state_handler
        .unknown_submissions
        .lock()
        .unwrap()
        .insert(id, chrono::Utc::now());
    let router = test_router(handler.clone());
    for uri in [
        format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        format!("/api/v1/job/delete?jobID={id}&secret={TEST_SECRET}"),
    ] {
        let (status, body) = call(&router, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("not known yet"), "{body}");
    }
    assert!(
        handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn delete_of_unknown_job_succeeds() {
    let router = test_router(test_handler("http://127.0.0.1:1", "https://s3.example.com"));
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={}&secret=x", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

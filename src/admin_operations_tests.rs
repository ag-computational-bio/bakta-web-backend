//! Router tests for the admin workflow operations with mock Argo, Keycloak and S3 servers.

use crate::{
    admin_api_tests::{admin_call, admin_setup, aged_job},
    api_structs::ArgoStatus,
    argo::structs::SimpleStatus,
    stats::{
        NewJob, StatsEvent,
        test_db::{test_db, unique_environment},
    },
    test_support::*,
    v2::api_structs::WorkflowKind,
};
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{any, method, path, query_param},
};

fn workflow(name: &str, phase: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "metadata": {"name": name}, "status": {"phase": phase}
    }))
}

async fn job_with_workflow(
    handler: &crate::bakta_handler::BaktaHandler,
    name: &str,
    status: ArgoStatus,
) -> Uuid {
    let id = v2_state(handler, WorkflowKind::Bakta, Some(status)).await;
    handler
        .state_handler
        .job_state
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .workflowname = Some(name.to_string());
    id
}

#[tokio::test]
async fn retry_only_accepts_failed_workflows() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-failed"))
        .respond_with(workflow("wf-failed", "Failed"))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-running"))
        .respond_with(workflow("wf-running", "Running"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-failed/retry"))
        .respond_with(workflow("wf-failed", "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let failed = job_with_workflow(&setup.handler, "wf-failed", ArgoStatus::Failed).await;
    let running = job_with_workflow(&setup.handler, "wf-running", ArgoStatus::Running).await;
    let gone = job_with_workflow(&setup.handler, "wf-gone", ArgoStatus::Failed).await;
    let unstarted = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;

    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{failed}/retry"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"workflow_name": "wf-failed", "phase": "Running"})
    );
    let states = setup.handler.state_handler.job_state.read().await;
    assert!(matches!(states[&failed].status, Some(ArgoStatus::Running)));
    drop(states);

    for (id, expected) in [
        (running, StatusCode::CONFLICT),
        (unstarted, StatusCode::CONFLICT),
        (gone, StatusCode::NOT_FOUND),
        (Uuid::new_v4(), StatusCode::NOT_FOUND),
    ] {
        let (status, body) =
            admin_call(&setup, "POST", &format!("/api/admin/jobs/{id}/retry"), None).await;
        assert_eq!(status, expected, "{body}");
        assert!(body["error"].is_string());
    }
}

#[tokio::test]
async fn late_retry_reset_does_not_hide_a_finish_seen_by_the_poll() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-1"))
        .respond_with(workflow("wf-1", "Failed"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-1/retry"))
        .respond_with(workflow("wf-1", "Running").set_delay(std::time::Duration::from_millis(800)))
        .mount(&argo)
        .await;
    let setup = std::sync::Arc::new(admin_setup(&argo.uri(), "https://s3.example.com", None).await);
    let id = job_with_workflow(&setup.handler, "wf-1", ArgoStatus::Failed).await;
    setup
        .handler
        .state_handler
        .job_state
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .updated = Some(Utc::now() - Duration::minutes(5));

    let retry = {
        let setup = setup.clone();
        tokio::spawn(async move {
            admin_call(&setup, "POST", &format!("/api/admin/jobs/{id}/retry"), None).await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The retried attempt finishes and the poll sees it before the retry call returns.
    let now = Utc::now();
    let finished: SimpleStatus = serde_json::from_value(json!({
        "metadata": {"name": "wf-1", "uid": Uuid::new_v4(),
                     "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}},
        "status": {"phase": "Succeeded", "startedAt": now.to_rfc3339(), "finishedAt": now.to_rfc3339()}
    }))
    .unwrap();
    setup
        .handler
        .state_handler
        .apply_workflow_statuses(vec![finished], now)
        .await;
    let (status, body) = retry.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert!(matches!(states[&id].status, Some(ArgoStatus::Succeeded)));
}

#[tokio::test]
async fn resubmit_points_the_job_to_the_new_workflow() {
    let argo = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-old/resubmit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": "wf-new", "creationTimestamp": Utc::now().to_rfc3339()},
            "status": {}
        })))
        .expect(1)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{id}/resubmit"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"workflow_name": "wf-new", "phase": "Pending"}));

    // Both workflows carry the job ID. The newer one keeps describing the job.
    let item = |name: &str, phase: &str, started: chrono::DateTime<Utc>| -> SimpleStatus {
        serde_json::from_value(json!({
            "metadata": {"name": name, "uid": Uuid::new_v4(),
                         "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}},
            "status": {"phase": phase, "startedAt": started.to_rfc3339()}
        }))
        .unwrap()
    };
    let now = Utc::now();
    setup
        .handler
        .state_handler
        .apply_workflow_statuses(
            vec![
                item("wf-new", "Running", now),
                item("wf-old", "Failed", now - Duration::hours(1)),
            ],
            now,
        )
        .await;
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-new"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Running)));
}

#[tokio::test]
async fn stop_only_accepts_running_workflows() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-run"))
        .respond_with(workflow("wf-run", "Running"))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-done"))
        .respond_with(workflow("wf-done", "Succeeded"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-run/terminate"))
        .respond_with(workflow("wf-run", "Failed"))
        .expect(1)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let running = job_with_workflow(&setup.handler, "wf-run", ArgoStatus::Running).await;
    let done = job_with_workflow(&setup.handler, "wf-done", ArgoStatus::Succeeded).await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{running}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"workflow_name": "wf-run", "phase": "Failed"}));
    let (status, _) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{done}/stop"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_removes_the_workflow_and_can_purge_data() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListBucketResult><Contents><Key>KEY1</Key></Contents><Contents><Key>KEY2</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>",
        ))
        .mount(&s3)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, None).await;
    let id = job_with_workflow(&setup.handler, "wf-1", ArgoStatus::Succeeded).await;
    // The listing must only return keys of this job.
    s3.reset().await;
    let keys = format!(
        "<ListBucketResult><Contents><Key>jobs/{id}/inputs/a</Key></Contents><Contents><Key>jobs/{id}/results/b</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>"
    );
    Mock::given(method("GET"))
        .and(path("/"))
        .and(query_param("prefix", format!("jobs/{id}/")))
        .respond_with(ResponseTemplate::new(200).set_body_string(keys))
        .mount(&s3)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(2)
        .mount(&s3)
        .await;

    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted_workflow": true, "purged_objects": 2}));
    assert!(
        !setup
            .handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );

    let unstarted = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&argo)
        .await;
    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{unstarted}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"deleted_workflow": false, "purged_objects": 0})
    );
    let (status, _) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{unstarted}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn start_is_refused_while_an_admin_delete_runs() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": "wf", "creationTimestamp": Utc::now().to_rfc3339()}
        })))
        .expect(0)
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
                )
                .set_delay(std::time::Duration::from_millis(1000)),
        )
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = std::sync::Arc::new(admin_setup(&argo.uri(), &s3_url, None).await);
    let id = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;

    let delete = {
        let setup = setup.clone();
        tokio::spawn(async move {
            admin_call(
                &setup,
                "DELETE",
                &format!("/api/admin/jobs/{id}?purge_data=true"),
                None,
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (status, body) = call(
        &setup.router,
        "POST",
        "/api/v2/job/start",
        Some(json!({"job": {"job_id": id, "secret": TEST_SECRET}, "workflow_kind": "bakta", "config": {}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = delete.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");

    // A delete is refused while a start holds the reservation.
    let id = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;
    let _start = setup.handler.state_handler.reserve_start(id).unwrap();
    let (status, _) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn delete_is_refused_while_a_submission_is_unknown() {
    let argo = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, None).await;
    // Argo may have created a workflow, but the lookup failed.
    let id = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;
    setup
        .handler
        .state_handler
        .unknown_submissions
        .lock()
        .unwrap()
        .insert(id, crate::bakta_handler::UnknownSubmission::new(Utc::now()));

    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"],
        "The result of an earlier start request is not known yet, try again later"
    );
    assert!(
        setup
            .handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn delete_marks_database_only_jobs_as_deleted() {
    let Some(db) = test_db("delete_marks_database_only_jobs_as_deleted").await else {
        return;
    };
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&argo)
        .await;
    let id = Uuid::new_v4();
    db.apply(&StatsEvent::Created(NewJob {
        id,
        environment: unique_environment(),
        workflow_kind: "bakta".to_string(),
        api_version: "v1".to_string(),
        name: None,
        created_at: Utc::now(),
        secret_hash: None,
        result_kind: None,
    }))
    .await
    .unwrap();
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    let (status, body) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted_workflow"], false);
    for _ in 0..100 {
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        if status == "deleted" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job was not marked as deleted");
}

#[tokio::test]
async fn delete_removes_a_workflow_the_backend_does_not_know() {
    let Some(db) = test_db("delete_removes_a_workflow_the_backend_does_not_know").await else {
        return;
    };
    // Argo created a workflow, but the backend restarted before a poll found
    // it: the database has a created job without a workflow name.
    let id = Uuid::new_v4();
    db.apply(&StatsEvent::Created(NewJob {
        id,
        environment: unique_environment(),
        workflow_kind: "bakta".to_string(),
        api_version: "v2".to_string(),
        name: None,
        created_at: Utc::now(),
        secret_hash: None,
        result_kind: None,
    }))
    .await
    .unwrap();
    let argo = MockServer::start().await;
    let running = listed_workflow("wf-unknown", "Running", Some(&id.to_string()), None);
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(query_param(
            "listOptions.labelSelector",
            format!("jobid={id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [running]})))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-unknown"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<ListBucketResult><Contents><Key>jobs/{id}/inputs/a</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>"
        )))
        .mount(&s3)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, Some(db.clone())).await;

    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted_workflow": true, "purged_objects": 1}));
}

#[tokio::test]
async fn delete_is_refused_when_the_workflow_lookup_fails() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, None).await;
    let id = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;

    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    for uri in [
        format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        format!("/api/v1/job/delete?jobID={id}&secret={TEST_SECRET}"),
    ] {
        let (status, body) = call(&setup.router, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }
    assert!(
        setup
            .handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

/// Argo mock that lists `wf-a` and `wf-b` for the job and answers the
/// delete of `wf-b` with `wf_b_status`.
async fn two_workflow_argo(id: Uuid, wf_b_status: u16) -> MockServer {
    let argo = MockServer::start().await;
    let job = id.to_string();
    let items = json!({"items": [
        listed_workflow("wf-a", "Running", Some(&job), None),
        listed_workflow("wf-b", "Pending", Some(&job), None),
    ]});
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(query_param(
            "listOptions.labelSelector",
            format!("jobid={id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(items))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-b"))
        .respond_with(ResponseTemplate::new(wf_b_status).set_body_json(json!({})))
        .mount(&argo)
        .await;
    argo
}

fn deleted_workflows(requests: Vec<wiremock::Request>) -> Vec<String> {
    let mut names: Vec<String> = requests
        .iter()
        .filter(|request| request.method.as_str() == "DELETE")
        .map(|request| request.url.path().rsplit('/').next().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn delete_removes_all_workflows_of_the_job() {
    for user in [false, true] {
        let id = Uuid::new_v4();
        let argo = two_workflow_argo(id, 200).await;
        let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
        let job = job_state(
            crate::bakta_handler::ApiVersion::V2,
            WorkflowKind::Bakta,
            crate::v2::api_structs::ResultKind::Bakta,
            None,
        );
        insert_state(
            &setup.handler,
            crate::bakta_handler::FullJobState { id, ..job },
        )
        .await;

        let (status, body) = if user {
            let uri = format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}");
            call(&setup.router, "DELETE", &uri, None).await
        } else {
            let (status, body) =
                admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
            assert_eq!(body, json!({"deleted_workflow": true, "purged_objects": 0}));
            (status, body.to_string())
        };
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            deleted_workflows(argo.received_requests().await.unwrap()),
            ["wf-a", "wf-b"]
        );
        let states = setup.handler.state_handler.job_state.read().await;
        assert!(!states.contains_key(&id));
    }
}

#[tokio::test]
async fn a_partly_failed_delete_keeps_the_job() {
    for user in [false, true] {
        let id = Uuid::new_v4();
        let argo = two_workflow_argo(id, 500).await;
        let s3 = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&s3)
            .await;
        let s3_url = format!("http://s3.test:{}", s3.address().port());
        let setup = admin_setup(&argo.uri(), &s3_url, None).await;
        let job = job_state(
            crate::bakta_handler::ApiVersion::V2,
            WorkflowKind::Bakta,
            crate::v2::api_structs::ResultKind::Bakta,
            None,
        );
        insert_state(
            &setup.handler,
            crate::bakta_handler::FullJobState { id, ..job },
        )
        .await;

        let (status, body) = if user {
            let uri = format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}");
            call(&setup.router, "DELETE", &uri, None).await
        } else {
            let uri = format!("/api/admin/jobs/{id}?purge_data=true");
            let (status, body) = admin_call(&setup, "DELETE", &uri, None).await;
            (status, body.to_string())
        };
        let expected = if user {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        assert_eq!(status, expected, "{body}");
        assert!(
            deleted_workflows(argo.received_requests().await.unwrap())
                .contains(&"wf-b".to_string())
        );
        let states = setup.handler.state_handler.job_state.read().await;
        assert!(states.contains_key(&id));
    }
}

fn listed_workflow(
    name: &str,
    phase: &str,
    job: Option<&str>,
    finished_hours_ago: Option<i64>,
) -> Value {
    let mut labels = json!({"bakta-environment": "production"});
    if let Some(job) = job {
        labels["jobid"] = job.into();
        labels["workflow-kind"] = "bakta".into();
    }
    json!({
        "metadata": {"name": name, "labels": labels},
        "spec": {"workflowTemplateRef": {"name": "bakta-job-1.9.0"}},
        "status": {
            "phase": phase,
            "startedAt": (Utc::now() - Duration::hours(50)).to_rfc3339(),
            "finishedAt": finished_hours_ago.map(|hours| (Utc::now() - Duration::hours(hours)).to_rfc3339()),
            "message": "msg"
        }
    })
}

fn suspended_workflow() -> Value {
    let mut workflow = listed_workflow("paused", "Running", Some("job-5"), None);
    workflow["spec"]["suspend"] = json!(true);
    workflow
}

fn workflow_list() -> Value {
    json!({"items": [
        listed_workflow("old-failed", "Failed", Some("job-1"), Some(48)),
        listed_workflow("old-error", "Error", None, Some(30)),
        listed_workflow("new-failed", "Failed", Some("job-3"), Some(1)),
        listed_workflow("running", "Running", Some("job-4"), None),
        suspended_workflow(),
        listed_workflow("pending", "Pending", Some("job-6"), None),
    ]})
}

async fn workflow_list_argo() -> MockServer {
    let argo = MockServer::start().await;
    mount_workflow_gets(&argo, &workflow_list()).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    for action in ["retry", "resume", "terminate"] {
        Mock::given(method("PUT"))
            .and(wiremock::matchers::path_regex(format!("/{action}$")))
            .respond_with(workflow("any", "Running"))
            .mount(&argo)
            .await;
    }
    // Resubmit of old-error fails, the others succeed.
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/old-error/resubmit"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(wiremock::matchers::path_regex("/resubmit$"))
        .respond_with(workflow("resubmitted", "Pending"))
        .mount(&argo)
        .await;
    argo
}

#[tokio::test]
async fn workflows_lists_live_workflows_of_the_environment() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;
    let (status, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 6);
    assert_eq!(items[3]["suspended"], false);
    assert_eq!(items[4]["suspended"], true);
    assert_eq!(items[0]["name"], "old-failed");
    assert_eq!(items[0]["job_id"], "job-1");
    assert_eq!(items[0]["workflow_kind"], "bakta");
    assert_eq!(items[0]["template"], "bakta-job-1.9.0");
    assert_eq!(items[0]["environment"], "production");
    assert_eq!(items[1]["job_id"], Value::Null);
    assert_eq!(items[1]["workflow_kind"], Value::Null);
    assert_eq!(items[3]["finished_at"], Value::Null);

    let (_, body) = admin_call(
        &setup,
        "GET",
        "/api/admin/workflows?phase=Failed,Error",
        None,
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 3);
    let (_, body) = admin_call(&setup, "GET", "/api/admin/workflows?environment=all", None).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 6);
    let (_, body) = admin_call(
        &setup,
        "GET",
        "/api/admin/workflows?environment=staging",
        None,
    )
    .await;
    assert!(body["items"].as_array().unwrap().is_empty());
    let (status, _) = admin_call(&setup, "GET", "/api/admin/workflows?environment=a,b", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workflows_and_bulk_actions_only_touch_bakta_web_workflows() {
    let argo = MockServer::start().await;
    let mut legacy = listed_workflow("legacy", "Failed", Some("job-1"), Some(48));
    legacy["metadata"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("bakta-environment");
    let mut foreign_job = legacy.clone();
    foreign_job["metadata"]["name"] = "foreign-job".into();
    foreign_job["spec"]["workflowTemplateRef"]["name"] = "other-pipeline".into();
    let foreign = json!({
        "metadata": {"name": "foreign", "labels": {"app": "other"}},
        "spec": {"workflowTemplateRef": {"name": "bakta-job-1.9.0"}},
        "status": {"phase": "Failed", "finishedAt": (Utc::now() - Duration::hours(48)).to_rfc3339()}
    });
    let items = json!({"items": [
        listed_workflow("owned", "Failed", Some("job-2"), Some(48)),
        legacy, foreign_job, foreign,
    ]});
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &items).await;

    let (status, body) =
        admin_call(&setup, "GET", "/api/admin/workflows?environment=all", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["owned", "legacy"]);

    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "delete", "dry_run": false, "filter": {"environment": "all"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["owned", "legacy"]);
    assert_eq!(
        argo_changes(argo.received_requests().await.unwrap()),
        [
            "DELETE /api/v1/workflows/bakta/owned",
            "DELETE /api/v1/workflows/bakta/legacy"
        ]
    );
}

#[tokio::test]
async fn workflows_without_environment_label_use_the_derived_environment() {
    let argo = MockServer::start().await;
    let unlabelled = |name: &str, labels: Value| {
        let mut workflow = listed_workflow(name, "Failed", Some(name), Some(48));
        let all = workflow["metadata"]["labels"].as_object_mut().unwrap();
        all.remove("bakta-environment");
        all.extend(labels.as_object().unwrap().clone());
        workflow
    };
    let items = json!({"items": [
        unlabelled("old-production", json!({"origin": "bakta.computational.bio"})),
        unlabelled("old-staging", json!({"origin": "staging.bakta.computational.bio"})),
        unlabelled("old-v2", json!({"origin": "Unknown", "api-version": "v2"})),
    ]});
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &items).await;

    let (status, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["old-production"]);
    assert_eq!(body["items"][0]["environment"], "production");
    let (_, body) = admin_call(
        &setup,
        "GET",
        "/api/admin/workflows?environment=staging",
        None,
    )
    .await;
    assert_eq!(names(&body), ["old-staging", "old-v2"]);
    assert_eq!(body["items"][0]["environment"], "staging");
    assert!(
        argo.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| !request.url.as_str().contains("labelSelector"))
    );

    // A bulk run only acts on the named workflows of the derived environment.
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({
            "action": "delete", "dry_run": false,
            "filter": {"environment": "staging"},
            "workflow_names": ["old-production", "old-staging"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["old-staging"]);
    assert_eq!(
        argo_changes(argo.received_requests().await.unwrap()),
        ["DELETE /api/v1/workflows/bakta/old-staging"]
    );
}

fn names(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_string())
        .collect()
}

fn argo_changes(requests: Vec<wiremock::Request>) -> Vec<String> {
    requests
        .into_iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect()
}

#[tokio::test]
async fn bulk_dry_run_lists_matches_without_changes() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"filter": {"phases": ["Failed", "Error"], "finished_before_hours": 24}, "action": "delete"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["action"], "delete");
    assert_eq!(names(&body), ["old-failed", "old-error"]);
    let item = &body["items"][0];
    assert_eq!(item["job_id"], "job-1");
    assert_eq!(item["workflow_kind"], "bakta");
    assert_eq!(item["applicable"], true);
    assert_eq!(item["result"], Value::Null);
    assert_eq!(item["error"], Value::Null);

    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"filter": {"q": "JOB-", "workflow_kinds": ["bakta"]}, "action": "stop"})),
    )
    .await;
    assert_eq!(
        names(&body),
        ["old-failed", "new-failed", "running", "paused", "pending"]
    );
    let applicable: Vec<_> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["applicable"].as_bool().unwrap())
        .collect();
    assert_eq!(applicable, [false, false, true, true, true]);

    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "resume"})),
    )
    .await;
    let resumable: Vec<_> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["applicable"] == true)
        .map(|item| item["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(resumable, ["paused"]);
    assert!(argo_changes(argo.received_requests().await.unwrap()).is_empty());

    for body in [
        json!({"action": "explode"}),
        json!({"action": "delete", "filter": {"finished_before_hours": -1}}),
    ] {
        let (status, _) = admin_call(&setup, "POST", "/api/admin/workflows/bulk", Some(body)).await;
        assert!(status.is_client_error(), "{status}");
    }
}

#[tokio::test]
async fn bulk_run_processes_listed_workflows_one_by_one() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({
            "action": "resubmit", "dry_run": false,
            "workflow_names": ["old-error", "old-failed", "running"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], false);
    let results: Vec<_> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["name"].as_str().unwrap().to_string(),
                item["result"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        results,
        [
            ("old-failed".to_string(), "done".to_string()),
            ("old-error".to_string(), "failed".to_string()),
            ("running".to_string(), "skipped".to_string()),
        ]
    );
    assert!(body["items"][1]["error"].as_str().unwrap().contains("500"));
    assert_eq!(
        argo_changes(argo.received_requests().await.unwrap()),
        [
            "PUT /api/v1/workflows/bakta/old-failed/resubmit",
            "PUT /api/v1/workflows/bakta/old-error/resubmit"
        ]
    );

    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "delete", "dry_run": false, "filter": {"phases": ["Error"]}})),
    )
    .await;
    assert_eq!(names(&body), ["old-error"]);
    assert_eq!(body["items"][0]["result"], "done");
}

#[tokio::test]
async fn bulk_delete_skips_pending_and_running_workflows() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;
    for dry_run in [true, false] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            "/api/admin/workflows/bulk",
            Some(json!({"action": "delete", "dry_run": dry_run})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        for name in ["running", "paused", "pending"] {
            let item = bulk_item(&body, name);
            assert_eq!(item["applicable"], false, "{body}");
            assert_eq!(item["reason"], "Still running, stop it first");
            if !dry_run {
                assert_eq!(item["result"], "skipped");
            }
        }
        assert_eq!(bulk_item(&body, "old-failed")["applicable"], true);
    }
    assert_eq!(
        argo_changes(argo.received_requests().await.unwrap()),
        [
            "DELETE /api/v1/workflows/bakta/old-failed",
            "DELETE /api/v1/workflows/bakta/old-error",
            "DELETE /api/v1/workflows/bakta/new-failed",
        ]
    );
}

#[tokio::test]
async fn bulk_retry_and_resubmit_update_the_job_state() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;
    let id = job_with_workflow(&setup.handler, "new-failed", ArgoStatus::Failed).await;
    // Point the listed workflow at the job.
    argo.reset().await;
    let mut listed = listed_workflow("new-failed", "Failed", Some(&id.to_string()), Some(1));
    listed["metadata"]["labels"]["jobid"] = id.to_string().into();
    let items = json!({"items": [listed]});
    store_listed_workflows(&setup.handler, &items).await;
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/new-failed/retry"))
        .respond_with(workflow("new-failed", "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/new-failed/resubmit"))
        .respond_with(workflow("new-failed-2", "Pending"))
        .mount(&argo)
        .await;

    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "retry", "dry_run": false})),
    )
    .await;
    assert_eq!(body["items"][0]["result"], "done", "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("new-failed"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Running)));
    drop(states);

    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "resubmit", "dry_run": false})),
    )
    .await;
    assert_eq!(body["items"][0]["result"], "done", "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("new-failed-2"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Pending)));
}

#[tokio::test]
async fn bulk_retry_and_resubmit_skip_superseded_workflows() {
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = job_with_workflow(&setup.handler, "wf-b", ArgoStatus::Running).await;
    let items = json!({"items": [
        listed_workflow("wf-a", "Failed", Some(&id.to_string()), Some(48)),
        listed_workflow("wf-b", "Running", Some(&id.to_string()), None),
    ]});
    mount_workflow_gets(&argo, &items).await;
    store_listed_workflows(&setup.handler, &items).await;
    Mock::given(method("PUT"))
        .respond_with(workflow("any", "Running"))
        .mount(&argo)
        .await;

    for action in ["retry", "resubmit"] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            "/api/admin/workflows/bulk",
            Some(json!({"action": action, "dry_run": false})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let item = &body["items"][0];
        assert_eq!(item["name"], "wf-a");
        assert_eq!(item["applicable"], false, "{body}");
        assert_eq!(item["result"], "skipped");
        assert!(
            item["reason"]
                .as_str()
                .unwrap()
                .contains("superseded by wf-b"),
            "{body}"
        );
    }
    assert!(argo_changes(argo.received_requests().await.unwrap()).is_empty());
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-b"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Running)));
}

#[tokio::test]
async fn retry_and_resubmit_are_refused_when_the_current_workflow_is_unknown() {
    let Some(db) = test_db("retry_and_resubmit_are_refused_when_the_current_workflow").await else {
        return;
    };
    // After a restart the job is not in memory, and the database fails.
    sqlx::query("DROP TABLE jobs")
        .execute(db.pool())
        .await
        .unwrap();
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    let restarted = Uuid::new_v4();
    // A job in memory whose workflow is not recorded yet.
    let unrecorded = v2_state(&setup.handler, WorkflowKind::Bakta, None).await;
    let items = json!({"items": [
        listed_workflow("wf-restarted", "Failed", Some(&restarted.to_string()), Some(48)),
        listed_workflow("wf-unrecorded", "Failed", Some(&unrecorded.to_string()), Some(48)),
    ]});
    store_listed_workflows(&setup.handler, &items).await;
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("PUT"))
        .respond_with(workflow("any", "Running"))
        .mount(&argo)
        .await;

    for action in ["retry", "resubmit"] {
        for dry_run in [true, false] {
            let (status, body) = admin_call(
                &setup,
                "POST",
                "/api/admin/workflows/bulk",
                Some(json!({"action": action, "dry_run": dry_run})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["items"].as_array().unwrap().len(), 2, "{body}");
            for item in body["items"].as_array().unwrap() {
                assert_eq!(item["applicable"], false, "{body}");
                assert_eq!(
                    item["reason"], "Current workflow unknown, try again later",
                    "{body}"
                );
                if !dry_run {
                    assert_eq!(item["result"], "skipped");
                }
            }
        }
        let (status, body) = admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{restarted}/{action}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }
    assert!(argo_changes(argo.received_requests().await.unwrap()).is_empty());
}

#[tokio::test]
async fn bulk_retry_and_resubmit_wait_for_the_first_poll_without_a_database() {
    let argo = MockServer::start().await;
    // No statistics database, and the job is not in memory after a restart.
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = Uuid::new_v4();
    let items = json!({"items": [
        listed_workflow("wf-a", "Failed", Some(&id.to_string()), Some(1)),
    ]});
    mount_workflow_gets(&argo, &items).await;
    store_listed_workflows(&setup.handler, &items).await;
    Mock::given(method("PUT"))
        .respond_with(workflow("any", "Running"))
        .mount(&argo)
        .await;

    for action in ["retry", "resubmit"] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            "/api/admin/workflows/bulk",
            Some(json!({"action": action, "dry_run": false})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let item = bulk_item(&body, "wf-a");
        assert_eq!(item["applicable"], false, "{body}");
        assert_eq!(item["reason"], "Current workflow unknown, try again later");
        assert_eq!(item["result"], "skipped");
    }
    assert!(argo_changes(argo.received_requests().await.unwrap()).is_empty());

    // Once a poll put the job into memory, its current workflow is known.
    let state = crate::bakta_handler::FullJobState {
        id,
        workflowname: Some("wf-a".to_string()),
        ..job_state(
            crate::bakta_handler::ApiVersion::V2,
            WorkflowKind::Bakta,
            crate::v2::api_structs::ResultKind::Bakta,
            Some(ArgoStatus::Failed),
        )
    };
    insert_state(&setup.handler, state).await;
    let body = bulk_dry_run(&setup, "retry").await;
    assert_eq!(bulk_item(&body, "wf-a")["applicable"], true, "{body}");
}

#[tokio::test]
async fn resume_only_accepts_suspended_workflows() {
    let argo = MockServer::start().await;
    let mut paused = suspended_workflow();
    paused["metadata"]["name"] = "wf-paused".into();
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-paused"))
        .respond_with(ResponseTemplate::new(200).set_body_json(paused))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-spec"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {"name": "wf-spec"}, "spec": {"suspend": true}, "status": {"phase": "Running"}
        })))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-run"))
        .respond_with(workflow("wf-run", "Running"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(wiremock::matchers::path_regex("/resume$"))
        .respond_with(workflow("any", "Running"))
        .expect(2)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    for (name, expected) in [
        ("wf-paused", StatusCode::OK),
        ("wf-spec", StatusCode::OK),
        ("wf-run", StatusCode::CONFLICT),
    ] {
        let id = job_with_workflow(&setup.handler, name, ArgoStatus::Running).await;
        let (status, body) = admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{id}/resume"),
            None,
        )
        .await;
        assert_eq!(status, expected, "{name}: {body}");
        if expected == StatusCode::OK {
            assert_eq!(body, json!({"workflow_name": name, "phase": "Running"}));
        }
    }
}

#[tokio::test]
async fn malformed_requests_get_json_errors() {
    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", None).await;
    let bearer = format!("Bearer {}", setup.token);
    let auth = [("authorization", bearer.as_str())];
    let cases = [
        (
            "POST",
            "/api/admin/workflows/bulk",
            Some(json!({"action": "explode"})),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "GET",
            "/api/admin/stats/summary?from=yesterday",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            "GET",
            "/api/admin/jobs/not-a-uuid",
            None,
            StatusCode::BAD_REQUEST,
        ),
        (
            "DELETE",
            &format!("/api/admin/jobs/{}?purge_data=maybe", Uuid::new_v4()),
            None,
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (method, uri, body, expected) in cases {
        let (status, text) = call_with_headers(&setup.router, method, uri, body, &auth).await;
        assert_eq!(status, expected, "{uri}: {text}");
        let body: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("{uri}: {text}"));
        assert!(body["error"].is_string(), "{uri}: {text}");
    }
    // Malformed JSON.
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/admin/workflows/bulk")
        .header("authorization", bearer.as_str())
        .header("content-type", "application/json")
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let response = tower::ServiceExt::oneshot(setup.router.clone(), request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["error"].is_string());
}

const INPUT_DELETED: &str = "Input data was deleted after the retention period";

/// Like `job_with_workflow`, with the last update `age` ago.
async fn aged_state_job(
    handler: &crate::bakta_handler::BaktaHandler,
    name: &str,
    status: ArgoStatus,
    age: Duration,
) -> Uuid {
    let id = job_with_workflow(handler, name, status).await;
    handler
        .state_handler
        .job_state
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .updated = Some(Utc::now() - age);
    id
}

#[tokio::test]
async fn retry_and_resubmit_are_refused_when_the_input_data_is_gone() {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-fresh"))
        .respond_with(workflow("wf-fresh", "Failed"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-fresh/retry"))
        .respond_with(workflow("wf-fresh", "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    // No other workflow may be changed.
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let old = aged_state_job(
        &setup.handler,
        "wf-old",
        ArgoStatus::Failed,
        Duration::days(30) + Duration::seconds(5),
    )
    .await;
    let fresh = aged_state_job(
        &setup.handler,
        "wf-fresh",
        ArgoStatus::Failed,
        Duration::days(30) - Duration::minutes(5),
    )
    .await;
    for action in ["retry", "resubmit"] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{old}/{action}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": INPUT_DELETED}));
    }
    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{fresh}/retry"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn retry_and_resubmit_use_the_job_record_for_the_retention_time() {
    let Some(db) = test_db("retry_and_resubmit_use_the_job_record_for_the_retention_time").await
    else {
        return;
    };
    let env = unique_environment();
    let now = Utc::now();
    // Finished recently, but created long ago: the finish time counts.
    let finished = aged_job(
        &db,
        &env,
        now - Duration::days(60),
        Some(now - Duration::days(1)),
    )
    .await;
    // Never finished: the creation time counts.
    let unfinished = aged_job(&db, &env, now - Duration::days(31), None).await;
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/workflows/bakta/wf-{finished}")))
        .respond_with(workflow(&format!("wf-{finished}"), "Failed"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/v1/workflows/bakta/wf-{finished}/retry")))
        .respond_with(workflow(&format!("wf-{finished}"), "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;

    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{finished}/retry"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for action in ["retry", "resubmit"] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{unfinished}/{action}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, json!({"error": INPUT_DELETED}));
    }
}

#[tokio::test]
async fn delete_skips_the_s3_purge_when_the_data_is_gone() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, None).await;
    let id = aged_state_job(
        &setup.handler,
        "wf-old",
        ArgoStatus::Succeeded,
        Duration::days(31),
    )
    .await;
    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted_workflow": true, "purged_objects": 0}));
    assert!(
        !setup
            .handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn delete_still_purges_data_just_before_the_expiry() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
        ))
        .expect(1)
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let setup = admin_setup(&argo.uri(), &s3_url, None).await;
    let id = aged_state_job(
        &setup.handler,
        "wf-new",
        ArgoStatus::Succeeded,
        Duration::days(30) - Duration::minutes(5),
    )
    .await;
    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{id}?purge_data=true"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

const BULK_INPUT_DELETED: &str = "Input data deleted after the retention period";

async fn bulk_dry_run(setup: &crate::admin_api_tests::AdminSetup, action: &str) -> Value {
    let (status, body) = admin_call(
        setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": action, "dry_run": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn bulk_item<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("no item {name}: {body}"))
}

#[tokio::test]
async fn bulk_retry_and_resubmit_skip_workflows_without_input_data() {
    let argo = MockServer::start().await;
    let mut unfinished = listed_workflow("unfinished-old", "Failed", None, None);
    unfinished["metadata"]["creationTimestamp"] =
        (Utc::now() - Duration::days(31)).to_rfc3339().into();
    let items = json!({"items": [
        listed_workflow("gone", "Failed", None, Some(31 * 24)),
        listed_workflow("kept", "Failed", None, Some(29 * 24)),
        unfinished,
    ]});
    mount_workflow_gets(&argo, &items).await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &items).await;

    for action in ["retry", "resubmit"] {
        let body = bulk_dry_run(&setup, action).await;
        for name in ["gone", "unfinished-old"] {
            let item = bulk_item(&body, name);
            assert_eq!(item["applicable"], false, "{body}");
            assert_eq!(item["reason"], BULK_INPUT_DELETED);
        }
        let kept = bulk_item(&body, "kept");
        assert_eq!(kept["applicable"], true, "{body}");
        assert_eq!(kept["reason"], Value::Null);
    }
    // Other actions do not need the input data.
    let body = bulk_dry_run(&setup, "delete").await;
    assert!(
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["applicable"] == true)
    );
}

#[tokio::test]
async fn bulk_real_run_does_not_touch_workflows_without_input_data() {
    let argo = MockServer::start().await;
    let items = json!({"items": [
        listed_workflow("gone", "Failed", None, Some(31 * 24)),
        listed_workflow("kept", "Failed", None, Some(29 * 24)),
    ]});
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/kept/retry"))
        .respond_with(workflow("kept", "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &items).await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "retry", "dry_run": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(bulk_item(&body, "gone")["result"], "skipped");
    assert_eq!(bulk_item(&body, "kept")["result"], "done");
}

#[tokio::test]
async fn bulk_uses_the_job_record_for_the_retention_time() {
    let Some(db) = test_db("bulk_uses_the_job_record_for_the_retention_time").await else {
        return;
    };
    let env = unique_environment();
    let now = Utc::now();
    let gone = aged_job(
        &db,
        &env,
        now - Duration::days(40),
        Some(now - Duration::days(31)),
    )
    .await;
    let kept = aged_job(
        &db,
        &env,
        now - Duration::days(40),
        Some(now - Duration::days(29)),
    )
    .await;
    // The workflows finished recently: only the job record says the data is gone.
    let items = json!({"items": [
        listed_workflow("wf-gone", "Failed", Some(&gone.to_string()), Some(1)),
        listed_workflow("wf-kept", "Failed", Some(&kept.to_string()), Some(1)),
    ]});
    let argo = MockServer::start().await;
    mount_workflow_gets(&argo, &items).await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    store_listed_workflows(&setup.handler, &items).await;
    // The records point to these workflows, so they are the current ones.
    for (id, name) in [(gone, "wf-gone"), (kept, "wf-kept")] {
        sqlx::query("UPDATE jobs SET workflow_name = $1 WHERE id = $2")
            .bind(name)
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    for action in ["retry", "resubmit"] {
        let body = bulk_dry_run(&setup, action).await;
        let item = bulk_item(&body, "wf-gone");
        assert_eq!(item["applicable"], false, "{body}");
        assert_eq!(item["reason"], BULK_INPUT_DELETED);
        assert_eq!(bulk_item(&body, "wf-kept")["applicable"], true, "{body}");
    }
}

/// Starts a user delete of the job whose Argo delete call is slow, and waits
/// until the delete holds the job's reservation.
async fn start_slow_user_delete(
    setup: &std::sync::Arc<crate::admin_api_tests::AdminSetup>,
    argo: &MockServer,
    workflow_name: &str,
    id: Uuid,
) -> tokio::task::JoinHandle<(StatusCode, String)> {
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/workflows/bakta/{workflow_name}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(std::time::Duration::from_secs(1)),
        )
        .expect(1)
        .mount(argo)
        .await;
    let delete = tokio::spawn({
        let router = setup.router.clone();
        async move {
            call(
                &router,
                "DELETE",
                &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
                None,
            )
            .await
        }
    });
    let state = &setup.handler.state_handler;
    while !state.starting_jobs.lock().unwrap().contains(&id) {
        tokio::task::yield_now().await;
    }
    delete
}

#[tokio::test]
async fn resubmit_is_refused_while_a_user_delete_runs() {
    let argo = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-old/resubmit"))
        .respond_with(workflow("wf-new", "Pending"))
        .expect(0)
        .mount(&argo)
        .await;
    let setup = std::sync::Arc::new(admin_setup(&argo.uri(), "https://s3.example.com", None).await);
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let delete = start_slow_user_delete(&setup, &argo, "wf-old", id).await;

    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{id}/resubmit"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "Job is busy, try again later");

    let (status, body) = delete.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert!(!states.contains_key(&id));
}

#[tokio::test]
async fn bulk_resubmit_skips_a_job_while_a_user_delete_runs() {
    let argo = MockServer::start().await;
    let setup = std::sync::Arc::new(admin_setup(&argo.uri(), "https://s3.example.com", None).await);
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let listed = listed_workflow("wf-old", "Failed", Some(&id.to_string()), Some(1));
    let items = json!({"items": [listed]});
    store_listed_workflows(&setup.handler, &items).await;
    mount_workflow_gets(&argo, &items).await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-old/resubmit"))
        .respond_with(workflow("wf-new", "Pending"))
        .expect(0)
        .mount(&argo)
        .await;
    let delete = start_slow_user_delete(&setup, &argo, "wf-old", id).await;

    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "resubmit", "dry_run": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = &body["items"][0];
    assert_eq!(item["applicable"], false, "{body}");
    assert_eq!(item["result"], "skipped");
    assert_eq!(item["reason"], "Job is busy, try again later");

    let (status, body) = delete.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert!(!states.contains_key(&id));
}

#[tokio::test]
async fn a_poll_fetched_before_a_delete_does_not_add_the_job_back() {
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let state = &setup.handler.state_handler;
    let by_user = job_with_workflow(&setup.handler, "wf-user", ArgoStatus::Succeeded).await;
    let by_admin = job_with_workflow(&setup.handler, "wf-admin", ArgoStatus::Succeeded).await;
    let now = Utc::now();
    let listed = |id: Uuid, name: &str| {
        json!({
            "metadata": {"name": name, "uid": Uuid::new_v4(),
                         "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}},
            "status": {"phase": "Succeeded", "startedAt": now.to_rfc3339(), "finishedAt": now.to_rfc3339()}
        })
    };
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&argo)
        .await;

    // The watch reports the workflows before the deletes.
    let fetched: Vec<SimpleStatus> = serde_json::from_value(json!([
        listed(by_user, "wf-user"),
        listed(by_admin, "wf-admin"),
    ]))
    .unwrap();
    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={by_user}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{by_admin}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // It is applied after the deletes. The jobs stay gone.
    state.apply_workflow_statuses(fetched, Utc::now()).await;
    let states = state.job_state.read().await;
    assert!(!states.contains_key(&by_user));
    assert!(!states.contains_key(&by_admin));
    drop(states);
    for id in [by_user, by_admin] {
        let (status, body) = call(
            &setup.router,
            "POST",
            "/api/v2/job/list",
            Some(json!({"jobs": [{"job_id": id, "secret": TEST_SECRET}]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["jobs"], json!([]), "{body}");
        assert_eq!(body["failed_jobs"][0]["status"], "not_found", "{body}");
        let (status, body) = call(
            &setup.router,
            "POST",
            "/api/v2/job/result",
            Some(json!({"job_id": id, "secret": TEST_SECRET})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("Job not found"), "{body}");
    }
}

#[tokio::test]
async fn expired_jobs_are_recorded_like_deleted_jobs() {
    let setup = admin_setup("http://127.0.0.1:1", "https://s3.example.com", None).await;
    let state = &setup.handler.state_handler;
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Succeeded).await;
    state.job_state.write().await.get_mut(&id).unwrap().updated =
        Some(Utc::now() - Duration::days(40));
    state.apply_workflow_statuses(vec![], Utc::now()).await;
    assert!(!state.job_state.read().await.contains_key(&id));
    assert!(state.deleted_jobs.lock().unwrap().contains_key(&id));
}

/// A job whose workflow is live and archived at the same time: Argo set the
/// archiving label while the workflow still exists in the cluster.
async fn live_and_archived_job(
    handler: &crate::bakta_handler::BaktaHandler,
    name: &str,
    uid: Uuid,
) -> Uuid {
    let id = job_with_workflow(handler, name, ArgoStatus::Failed).await;
    let mut states = handler.state_handler.job_state.write().await;
    let job = states.get_mut(&id).unwrap();
    job.archived = true;
    job.argo_uid = Some(uid);
    id
}

#[tokio::test]
async fn deletes_remove_the_live_workflow_of_an_archived_job() {
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let mut jobs = Vec::new();
    for name in ["wf-v1", "wf-v2", "wf-admin"] {
        let uid = Uuid::new_v4();
        let id = live_and_archived_job(&setup.handler, name, uid).await;
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/workflows/bakta/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&argo)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/archived-workflows/{uid}")))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&argo)
            .await;
        jobs.push(id);
    }
    // A retry keeps the cached archived flag. The delete still removes the
    // retried live workflow.
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-admin"))
        .respond_with(workflow("wf-admin", "Failed"))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-admin/retry"))
        .respond_with(workflow("wf-admin", "Running"))
        .expect(1)
        .mount(&argo)
        .await;
    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{}/retry", jobs[2]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v1/job/delete?jobID={}&secret={TEST_SECRET}", jobs[0]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={}&secret={TEST_SECRET}", jobs[1]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin_call(
        &setup,
        "DELETE",
        &format!("/api/admin/jobs/{}", jobs[2]),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"deleted_workflow": true, "purged_objects": 0}));
}

#[tokio::test]
async fn deleting_an_archived_job_whose_live_workflow_is_gone_succeeds() {
    let argo = MockServer::start().await;
    let uid = Uuid::new_v4();
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-gone"))
        .respond_with(ResponseTemplate::new(404).set_body_string("{}"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = live_and_archived_job(&setup.handler, "wf-gone", uid).await;
    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !setup
            .handler
            .state_handler
            .job_state
            .read()
            .await
            .contains_key(&id)
    );
}

#[tokio::test]
async fn resubmit_is_refused_while_a_bulk_resubmit_checks_the_job() {
    let Some(db) = test_db("resubmit_is_refused_while_a_bulk_resubmit_checks_the_job").await else {
        return;
    };
    let argo = MockServer::start().await;
    let setup = std::sync::Arc::new(
        admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await,
    );
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let listed = listed_workflow("wf-old", "Failed", Some(&id.to_string()), Some(1));
    let items = json!({"items": [listed]});
    store_listed_workflows(&setup.handler, &items).await;
    mount_workflow_gets(&argo, &items).await;
    // Only the bulk resubmit runs.
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-old/resubmit"))
        .respond_with(workflow("wf-new", "Pending"))
        .expect(1)
        .mount(&argo)
        .await;

    // Holds the bulk resubmit in its retention lookup, after it checked
    // which workflow is current.
    let mut lock = db.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let bulk = tokio::spawn({
        let setup = setup.clone();
        async move {
            admin_call(
                &setup,
                "POST",
                "/api/admin/workflows/bulk",
                Some(json!({"action": "resubmit", "dry_run": false})),
            )
            .await
        }
    });
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity WHERE pg_backend_pid() = ANY(pg_blocking_pids(pid))",
        )
        .fetch_one(&mut *lock)
        .await
        .unwrap();
        if waiting > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // A single resubmit now would publish wf-new, and the bulk resubmit
    // would then resubmit wf-old again.
    let single = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{id}/resubmit"),
            None,
        ),
    )
    .await;
    lock.rollback().await.unwrap();
    let (status, body) = single.expect("the single resubmit must not wait for the bulk one");
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "Job is busy, try again later");

    let (status, body) = bulk.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["result"], "done", "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-new"));
}

/// A polled workflow of a job with a creation time.
fn polled_workflow(
    id: Uuid,
    name: &str,
    phase: &str,
    created: chrono::DateTime<Utc>,
) -> SimpleStatus {
    serde_json::from_value(json!({
        "metadata": {"name": name, "uid": Uuid::new_v4(),
                     "creationTimestamp": created.to_rfc3339(),
                     "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}},
        "status": {"phase": phase, "startedAt": created.to_rfc3339()}
    }))
    .unwrap()
}

const SUBMISSION_UNKNOWN: &str =
    "The result of an earlier start request is not known yet, try again later";

/// Argo creates the new workflow, but its resubmit answer cannot be read.
async fn mount_unreadable_resubmit(argo: &MockServer, name: &str) {
    Mock::given(method("PUT"))
        .and(path(format!("/api/v1/workflows/bakta/{name}/resubmit")))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(argo)
        .await;
}

#[tokio::test]
async fn an_unknown_resubmit_blocks_the_job_until_a_poll_finds_the_new_workflow() {
    let argo = MockServer::start().await;
    mount_unreadable_resubmit(&argo, "wf-old").await;
    // Only the new workflow is deleted in the end.
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let state = &setup.handler.state_handler;
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let resubmit = format!("/api/admin/jobs/{id}/resubmit");
    let requested = Utc::now();
    let (status, body) = admin_call(&setup, "POST", &resubmit, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");

    // Delete, resubmit and retry are refused.
    let (status, body) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], SUBMISSION_UNKNOWN);
    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    for action in ["resubmit", "retry"] {
        let (status, body) = admin_call(
            &setup,
            "POST",
            &format!("/api/admin/jobs/{id}/{action}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], SUBMISSION_UNKNOWN);
    }

    // A poll that only lists the old workflow does not resolve the job.
    let old = polled_workflow(id, "wf-old", "Failed", requested - Duration::hours(1));
    let now = Utc::now() + Duration::seconds(1);
    state.apply_workflow_statuses(vec![old.clone()], now).await;
    assert!(state.is_submission_unknown(&id));
    assert_eq!(
        state.job_state.read().await[&id].workflowname.as_deref(),
        Some("wf-old")
    );

    // A poll that lists the new workflow resolves it.
    let new = polled_workflow(id, "wf-new", "Running", requested);
    state.apply_workflow_statuses(vec![old, new], now).await;
    assert!(!state.is_submission_unknown(&id));
    assert_eq!(
        state.job_state.read().await[&id].workflowname.as_deref(),
        Some("wf-new")
    );
    let (status, body) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn an_unknown_bulk_resubmit_blocks_the_job_until_enough_polls_found_nothing() {
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let state = &setup.handler.state_handler;
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let listed = listed_workflow("wf-old", "Failed", Some(&id.to_string()), Some(1));
    let items = json!({"items": [listed]});
    store_listed_workflows(&setup.handler, &items).await;
    mount_workflow_gets(&argo, &items).await;
    mount_unreadable_resubmit(&argo, "wf-old").await;
    let bulk = json!({"action": "resubmit", "dry_run": false});
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(bulk.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["result"], "failed", "{body}");

    // A second bulk run skips the job.
    let (status, body) = admin_call(&setup, "POST", "/api/admin/workflows/bulk", Some(bulk)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["applicable"], false, "{body}");
    assert_eq!(body["items"][0]["reason"], SUBMISSION_UNKNOWN);
    let (status, _) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Polls that only list the old workflow count as empty.
    let old = polled_workflow(id, "wf-old", "Failed", Utc::now() - Duration::hours(1));
    for minutes in [4, 8, 11] {
        assert!(state.is_submission_unknown(&id));
        state
            .apply_workflow_statuses(vec![old.clone()], Utc::now() + Duration::minutes(minutes))
            .await;
    }
    assert!(!state.is_submission_unknown(&id));
    assert_eq!(
        state.job_state.read().await[&id].workflowname.as_deref(),
        Some("wf-old")
    );
}

const JOB_DELETED: &str = "Job was deleted";

/// Checks that a single and a bulk resubmit (dry and real run) refuse the
/// deleted job `id`, whose workflow `name` Argo still lists.
async fn assert_resubmit_refuses_deleted_job(
    setup: &crate::admin_api_tests::AdminSetup,
    argo: &MockServer,
    id: Uuid,
    name: &str,
) {
    let listed =
        json!({"items": [listed_workflow(name, "Failed", Some(&id.to_string()), Some(1))]});
    store_listed_workflows(&setup.handler, &listed).await;
    mount_workflow_gets(argo, &listed).await;
    Mock::given(method("PUT"))
        .respond_with(workflow("wf-new", "Pending"))
        .expect(0)
        .mount(argo)
        .await;
    let (status, body) = admin_call(
        setup,
        "POST",
        &format!("/api/admin/jobs/{id}/resubmit"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], JOB_DELETED);
    for dry_run in [true, false] {
        let (status, body) = admin_call(
            setup,
            "POST",
            "/api/admin/workflows/bulk",
            Some(json!({"action": "resubmit", "dry_run": dry_run})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"][0]["applicable"], false, "{body}");
        assert_eq!(body["items"][0]["reason"], JOB_DELETED, "{body}");
    }
    let states = setup.handler.state_handler.job_state.read().await;
    assert!(!states.contains_key(&id));
}

#[tokio::test]
async fn resubmit_refuses_a_job_deleted_in_memory() {
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-old"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = job_with_workflow(&setup.handler, "wf-old", ArgoStatus::Failed).await;
    let (status, body) = call(
        &setup.router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_resubmit_refuses_deleted_job(&setup, &argo, id, "wf-old").await;
}

#[tokio::test]
async fn resubmit_refuses_a_job_deleted_in_the_database() {
    let Some(db) = test_db("resubmit_refuses_a_job_deleted_in_the_database").await else {
        return;
    };
    let now = Utc::now();
    let id = aged_job(
        &db,
        &unique_environment(),
        now - Duration::days(1),
        Some(now - Duration::hours(1)),
    )
    .await;
    db.apply(&StatsEvent::Deleted { id }).await.unwrap();
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    assert_resubmit_refuses_deleted_job(&setup, &argo, id, &format!("wf-{id}")).await;
}

#[tokio::test]
async fn a_poll_after_a_restart_does_not_add_jobs_deleted_in_the_database_back() {
    let Some(db) = test_db("a_poll_after_a_restart_does_not_add_jobs_deleted").await else {
        return;
    };
    let env = unique_environment();
    let now = Utc::now();
    let created = now - Duration::days(1);
    let finished = Some(now - Duration::hours(1));
    let deleted = aged_job(&db, &env, created, finished).await;
    db.apply(&StatsEvent::Deleted { id: deleted })
        .await
        .unwrap();
    let recorded = aged_job(&db, &env, created, finished).await;
    let unrecorded = Uuid::new_v4();
    // After a restart nothing is in memory, and Argo still lists all three.
    let setup = admin_setup(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        Some(db.clone()),
    )
    .await;
    let state = &setup.handler.state_handler;
    let items = vec![
        polled_workflow(deleted, &format!("wf-{deleted}"), "Failed", created),
        polled_workflow(recorded, &format!("wf-{recorded}"), "Failed", created),
        polled_workflow(unrecorded, "wf-unrecorded", "Failed", created),
    ];
    state.apply_workflow_statuses(items.clone(), now).await;
    let states = state.job_state.read().await;
    assert!(!states.contains_key(&deleted));
    assert!(states.contains_key(&recorded));
    assert!(states.contains_key(&unrecorded));
    drop(states);
    assert!(state.is_deleted(&deleted));

    // While the database fails, unknown jobs are not added. Known jobs are
    // still updated.
    state.job_state.write().await.remove(&unrecorded);
    sqlx::query("DROP TABLE jobs")
        .execute(db.pool())
        .await
        .unwrap();
    let mut changed = items.clone();
    changed[1] = polled_workflow(recorded, &format!("wf-{recorded}"), "Error", created);
    state.apply_workflow_statuses(changed, now).await;
    let states = state.job_state.read().await;
    assert!(!states.contains_key(&unrecorded));
    assert!(matches!(states[&recorded].status, Some(ArgoStatus::Error)));
}

async fn table_names(db: &crate::stats::StatsDb) -> Vec<String> {
    let mut names: Vec<String> = db
        .live_workflows("bakta", None)
        .await
        .unwrap()
        .into_iter()
        .map(|workflow| workflow.metadata.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn workflow_list_and_bulk_preview_read_the_table() {
    let Some(db) = test_db("workflow_list_and_bulk_preview_read_the_table").await else {
        return;
    };
    let argo = MockServer::start().await;
    mount_workflow_gets(
        &argo,
        &json!({"items": [listed_workflow("argo-only", "Failed", None, Some(48))]}),
    )
    .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    let stored = json!({"items": [listed_workflow("table-only", "Failed", None, Some(48))]});
    store_listed_workflows(&setup.handler, &stored).await;

    let (status, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["table-only"]);
    let body = bulk_dry_run(&setup, "delete").await;
    assert_eq!(names(&body), ["table-only"]);
    assert_eq!(bulk_item(&body, "table-only")["applicable"], true);
    assert!(argo.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn bulk_run_checks_the_workflows_in_argo() {
    let Some(db) = test_db("bulk_run_checks_the_workflows_in_argo").await else {
        return;
    };
    let argo = MockServer::start().await;
    // The table still has an older state: wf-started was started again.
    let stored = json!({"items": [
        listed_workflow("wf-started", "Failed", None, Some(48)),
        listed_workflow("wf-done", "Failed", None, Some(48)),
        listed_workflow("wf-gone", "Failed", None, Some(48)),
        listed_workflow("wf-other", "Failed", None, Some(48)),
    ]});
    // Argo no longer has wf-gone.
    let current = json!({"items": [
        listed_workflow("wf-started", "Running", None, None),
        listed_workflow("wf-done", "Failed", None, Some(48)),
        listed_workflow("wf-other", "Failed", None, Some(48)),
        listed_workflow("wf-new", "Failed", None, Some(48)),
    ]});
    mount_workflow_gets(&argo, &current).await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    store_listed_workflows(&setup.handler, &stored).await;

    let preview = bulk_dry_run(&setup, "delete").await;
    assert_eq!(bulk_item(&preview, "wf-started")["applicable"], true);
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({
            "action": "delete", "dry_run": false,
            "workflow_names": ["wf-started", "wf-done", "wf-gone"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["wf-started", "wf-done"]);
    assert_eq!(bulk_item(&body, "wf-started")["result"], "skipped");
    assert_eq!(bulk_item(&body, "wf-done")["result"], "done");
    let requests = argo.received_requests().await.unwrap();
    // Only the named workflows are fetched, one by one. Nothing is listed.
    let fetched: Vec<_> = requests
        .iter()
        .filter(|request| request.method.as_str() == "GET")
        .map(|request| request.url.path().to_string())
        .collect();
    assert_eq!(
        fetched,
        [
            "/api/v1/workflows/bakta/wf-started",
            "/api/v1/workflows/bakta/wf-done",
            "/api/v1/workflows/bakta/wf-gone"
        ]
    );
    assert!(requests.iter().all(|request| request.url.query().is_none()));
    assert_eq!(
        argo_changes(requests),
        ["DELETE /api/v1/workflows/bakta/wf-done"]
    );
    // The deleted workflow is gone from the table at once. The watch
    // removes wf-gone.
    assert_eq!(
        table_names(&db).await,
        ["wf-gone", "wf-other", "wf-started"]
    );
}

#[tokio::test]
async fn admin_actions_update_the_workflow_table() {
    let Some(db) = test_db("admin_actions_update_the_workflow_table").await else {
        return;
    };
    let argo = MockServer::start().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some(db.clone())).await;
    let id = job_with_workflow(&setup.handler, "wf-1", ArgoStatus::Failed).await;
    let job = id.to_string();
    let failed = listed_workflow("wf-1", "Failed", Some(&job), Some(1));
    let retried = listed_workflow("wf-1", "Running", Some(&job), None);
    store_listed_workflows(&setup.handler, &json!({"items": [failed.clone()]})).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(failed))
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-1/retry"))
        .respond_with(ResponseTemplate::new(200).set_body_json(retried))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;

    let (status, body) =
        admin_call(&setup, "POST", &format!("/api/admin/jobs/{id}/retry"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(names(&body), ["wf-1"]);
    assert_eq!(body["items"][0]["phase"], "Running");
    assert_eq!(body["items"][0]["finished_at"], Value::Null);

    let (status, body) = admin_call(&setup, "DELETE", &format!("/api/admin/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert!(body["items"].as_array().unwrap().is_empty(), "{body}");
}

#[tokio::test]
async fn workflow_list_and_bulk_actions_are_limited_with_totals() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    store_listed_workflows(&setup.handler, &workflow_list()).await;

    let (status, body) =
        admin_call(&setup, "GET", "/api/admin/workflows?limit=2&offset=1", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["old-error", "new-failed"]);
    assert_eq!(body["total"], 6);
    let (_, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 6);
    assert_eq!(body["total"], 6);

    // The preview says when more workflows match than it lists.
    let (status, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "delete", "limit": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(names(&body), ["old-failed", "old-error"]);
    assert_eq!(body["total"], 6);
    assert_eq!(body["truncated"], true);
    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "delete"})),
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 6);
    assert_eq!(body["truncated"], false);

    // A real run acts on at most `limit` workflows too.
    let (_, body) = admin_call(
        &setup,
        "POST",
        "/api/admin/workflows/bulk",
        Some(json!({"action": "delete", "dry_run": false, "limit": 1,
                    "filter": {"phases": ["Failed"]}})),
    )
    .await;
    assert_eq!(names(&body), ["old-failed"]);
    assert_eq!(body["total"], 2);
    assert_eq!(body["truncated"], true);
    assert_eq!(
        argo_changes(argo.received_requests().await.unwrap()),
        ["DELETE /api/v1/workflows/bakta/old-failed"]
    );
}

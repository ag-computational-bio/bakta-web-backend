//! Router tests for the admin workflow operations with mock Argo, Keycloak and S3 servers.

use crate::{
    admin_api_tests::{admin_call, admin_setup},
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
    matchers::{method, path, query_param},
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
async fn delete_marks_database_only_jobs_as_deleted() {
    let Some(db) = test_db("delete_marks_database_only_jobs_as_deleted").await else {
        return;
    };
    let argo = MockServer::start().await;
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
    workflow["status"]["nodes"] = json!({"s": {"id": "s", "name": "paused[0].approve", "type": "Suspend", "phase": "Running"}});
    workflow
}

async fn workflow_list_argo() -> MockServer {
    let argo = MockServer::start().await;
    let items = json!({"items": [
        listed_workflow("old-failed", "Failed", Some("job-1"), Some(48)),
        listed_workflow("old-error", "Error", None, Some(30)),
        listed_workflow("new-failed", "Failed", Some("job-3"), Some(1)),
        listed_workflow("running", "Running", Some("job-4"), None),
        suspended_workflow(),
    ]});
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(query_param(
            "listOptions.labelSelector",
            "bakta-environment=production",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(items))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
        .mount(&argo)
        .await;
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
    let (status, body) = admin_call(&setup, "GET", "/api/admin/workflows", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
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
    assert!(body["items"].as_array().unwrap().is_empty());
    let (status, _) = admin_call(&setup, "GET", "/api/admin/workflows?environment=a,b", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
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
        ["old-failed", "new-failed", "running", "paused"]
    );
    let applicable: Vec<_> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["applicable"].as_bool().unwrap())
        .collect();
    assert_eq!(applicable, [false, false, true, true]);

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
async fn bulk_retry_and_resubmit_update_the_job_state() {
    let argo = workflow_list_argo().await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", None).await;
    let id = job_with_workflow(&setup.handler, "new-failed", ArgoStatus::Failed).await;
    // Point the listed workflow at the job.
    argo.reset().await;
    let mut listed = listed_workflow("new-failed", "Failed", Some(&id.to_string()), Some(1));
    listed["metadata"]["labels"]["jobid"] = id.to_string().into();
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [listed]})))
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
        Some(json!({"action": "resubmit", "dry_run": false})),
    )
    .await;
    assert_eq!(body["items"][0]["result"], "done", "{body}");
    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("new-failed-2"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Pending)));
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

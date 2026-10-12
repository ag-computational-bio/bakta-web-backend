//! Tests for the job secret of workflows without a `secret` label.

use crate::{
    admin_api_tests::{admin_call, admin_setup},
    api_structs::{ArgoStatus, Job, StartRequest},
    argo::structs::SimpleStatus,
    bakta_handler::{BaktaHandler, resubmitted_job_state},
    test_support::*,
    v2::api_structs::{JobReference, V2StartRequest, WorkflowKind},
};
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// A polled workflow of the job without a `secret` label.
fn unlabeled_workflow(id: Uuid, name: &str, phase: &str) -> SimpleStatus {
    let now = Utc::now();
    serde_json::from_value(json!({
        "metadata": {
            "name": name,
            "uid": Uuid::new_v4(),
            "labels": {"jobid": id.to_string(), "api-version": "v2", "bakta-environment": "production"}
        },
        "status": {"phase": phase, "startedAt": now.to_rfc3339(), "finishedAt": now.to_rfc3339()}
    }))
    .unwrap()
}

async fn poll(handler: &BaktaHandler, items: Vec<SimpleStatus>) {
    handler
        .state_handler
        .apply_workflow_statuses(items, Utc::now())
        .await;
}

/// Asserts that every user endpoint denies `secret` for the job.
async fn assert_denied(handler: &BaktaHandler, id: Uuid, secret: &str) {
    let state = &handler.state_handler;
    let job = || Job {
        id,
        secret: secret.to_string(),
    };
    let reference = || JobReference {
        job_id: id,
        secret: secret.to_string(),
    };
    let unauthorized = |result: anyhow::Result<()>, what: &str| {
        let error = result.expect_err(what);
        assert_eq!(error.to_string(), "Unauthorized", "{what}");
    };

    let list = state.get_job_states(vec![job()]).await.unwrap();
    assert!(list.jobs.is_empty(), "v1 list");
    assert_eq!(list.failed.len(), 1, "v1 list");
    let list = state.get_job_states_v2(vec![reference()]).await.unwrap();
    assert!(list.jobs.is_empty(), "v2 list");
    assert_eq!(list.failed_jobs.len(), 1, "v2 list");
    unauthorized(
        state.get_logs((id, secret.to_string())).await.map(drop),
        "v1 logs",
    );
    unauthorized(state.get_logs_v2(reference()).await.map(drop), "v2 logs");
    unauthorized(
        state
            .get_results(job(), &handler.s3_handler)
            .await
            .map(drop),
        "v1 results",
    );
    unauthorized(
        state
            .get_results_v2(reference(), &handler.s3_handler)
            .await
            .map(drop),
        "v2 results",
    );
    let start: StartRequest = serde_json::from_value(json!({
        "job": {"jobID": id, "secret": secret},
        "config": {
            "hasReplicons": false, "translationTable": 11, "completeGenome": false,
            "keepContigHeaders": false, "minContigLength": 1, "compliant": false
        }
    }))
    .unwrap();
    unauthorized(
        state.start_job(start, "1.9.0".to_string(), None).await,
        "v1 start",
    );
    let start: V2StartRequest = serde_json::from_value(json!({
        "job": {"job_id": id, "secret": secret},
        "workflow_kind": "bakta",
        "config": {},
    }))
    .unwrap();
    unauthorized(state.start_job_v2(start, None).await, "v2 start");
    unauthorized(state.delete_job((id, secret.to_string())).await, "delete");
    assert!(state.user_job_state(id, secret).await.unwrap().is_some());
}

async fn assert_allowed(handler: &BaktaHandler, id: Uuid) {
    let list = handler
        .state_handler
        .get_job_states_v2(vec![JobReference {
            job_id: id,
            secret: TEST_SECRET.to_string(),
        }])
        .await
        .unwrap();
    assert!(list.failed_jobs.is_empty(), "owner secret was denied");
    assert_eq!(list.jobs.len(), 1);
}

#[tokio::test]
async fn a_workflow_without_secret_label_denies_every_secret() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let id = Uuid::new_v4();
    poll(&handler, vec![unlabeled_workflow(id, "wf-1", "Succeeded")]).await;
    assert!(
        handler.state_handler.job_state.read().await[&id]
            .secret
            .is_none()
    );

    for secret in ["Unknown", ""] {
        assert_denied(&handler, id, secret).await;
    }
}

#[tokio::test]
async fn a_resubmitted_workflow_without_secret_label_denies_every_secret() {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let state = resubmitted_job_state(id, "wf-new", &HashMap::new(), now, now);
    assert!(state.secret.is_none());
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    insert_state(&handler, state).await;
    poll(
        &handler,
        vec![unlabeled_workflow(id, "wf-new", "Succeeded")],
    )
    .await;

    for secret in ["Unknown", ""] {
        assert_denied(&handler, id, secret).await;
    }
}

#[tokio::test]
async fn the_owner_secret_survives_a_poll_without_secret_label() {
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, Some(ArgoStatus::Pending)).await;
    handler
        .state_handler
        .job_state
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .workflowname = Some("wf-1".to_string());
    poll(&handler, vec![unlabeled_workflow(id, "wf-1", "Succeeded")]).await;

    assert_allowed(&handler, id).await;
    assert_denied(&handler, id, "Unknown").await;
}

#[tokio::test]
async fn the_owner_secret_survives_a_resubmit_without_secret_label() {
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
    let id = v2_state(
        &setup.handler,
        WorkflowKind::Bakta,
        Some(ArgoStatus::Failed),
    )
    .await;
    setup
        .handler
        .state_handler
        .job_state
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .workflowname = Some("wf-old".to_string());

    let (status, body) = admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{id}/resubmit"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_allowed(&setup.handler, id).await;

    poll(
        &setup.handler,
        vec![unlabeled_workflow(id, "wf-new", "Running")],
    )
    .await;
    assert_eq!(
        setup.handler.state_handler.job_state.read().await[&id]
            .workflowname
            .as_deref(),
        Some("wf-new")
    );
    assert_allowed(&setup.handler, id).await;
    assert_denied(&setup.handler, id, "Unknown").await;
}

/// A finished job that is only in the statistics database, with the hash
/// of `TEST_SECRET`, workflow `wf-done` and Argo uid `uid`.
async fn recorded_job(
    db: &crate::stats::StatsDb,
    finished_at: chrono::DateTime<Utc>,
) -> (Uuid, Uuid) {
    use crate::stats::{NewJob, StatsEvent, WorkflowState, secret_hash};
    let id = Uuid::new_v4();
    let uid = Uuid::new_v4();
    let job = NewJob {
        id,
        environment: "production".to_string(),
        workflow_kind: "bakta".to_string(),
        api_version: "v2".to_string(),
        name: Some("genome".to_string()),
        created_at: finished_at - chrono::Duration::minutes(10),
        secret_hash: Some(secret_hash(TEST_SECRET)),
        result_kind: Some("bakta".to_string()),
    };
    db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
    db.apply(&StatsEvent::Polled(vec![WorkflowState {
        job,
        workflow_name: "wf-done".to_string(),
        status: "succeeded",
        created_at: Some(finished_at - chrono::Duration::minutes(10)),
        started_at: Some(finished_at - chrono::Duration::minutes(5)),
        finished_at: Some(finished_at),
        error_message: None,
        origin: None,
        argo_uid: Some(uid),
    }]))
    .await
    .unwrap();
    (id, uid)
}

#[tokio::test]
async fn finished_jobs_are_served_from_the_database_with_their_secret() {
    let Some(db) =
        crate::stats::test_db::test_db("finished_jobs_are_served_from_the_database").await
    else {
        return;
    };
    let argo = MockServer::start().await;
    let (id, uid) = recorded_job(&db, Utc::now() - chrono::Duration::hours(1)).await;
    let (expired, _) = recorded_job(&db, Utc::now() - chrono::Duration::days(40)).await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": {"nodes": {}}})))
        .mount(&argo)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/artifact-files/bakta/archived-workflows/{uid}/wf-done/outputs/main-logs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string("archived log"))
        .mount(&argo)
        .await;
    let s3 = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&s3)
        .await;
    let s3_url = format!("http://s3.test:{}", s3.address().port());
    let (stats, _events) = crate::stats::StatsRecorder::capturing_with_db((*db).clone());
    let handler = test_handler_with_stats(&argo.uri(), &s3_url, stats);
    let state = &handler.state_handler;
    assert!(state.job_state.read().await.is_empty());

    // The owner secret is accepted.
    let list = state
        .get_job_states(vec![Job {
            id,
            secret: TEST_SECRET.to_string(),
        }])
        .await
        .unwrap();
    assert_eq!(list.jobs.len(), 1);
    assert!(matches!(
        list.jobs[0].status,
        crate::api_structs::JobStatusEnum::SUCCESSFUL
    ));
    assert_allowed(&handler, id).await;
    let reference = JobReference {
        job_id: id,
        secret: TEST_SECRET.to_string(),
    };
    let results = state
        .get_results_v2(reference.clone(), &handler.s3_handler)
        .await
        .unwrap();
    assert_eq!(results.name, "genome");
    let logs = state.get_logs((id, TEST_SECRET.to_string())).await.unwrap();
    assert!(logs.contains("archived log"), "{logs}");

    // Any other secret is denied.
    for secret in ["Unknown", "", &crate::stats::secret_hash(TEST_SECRET)] {
        assert_denied(&handler, id, secret).await;
    }

    // Jobs past the 35 day job retention are not found.
    let list = state
        .get_job_states_v2(vec![JobReference {
            job_id: expired,
            secret: TEST_SECRET.to_string(),
        }])
        .await
        .unwrap();
    assert!(list.jobs.is_empty());
    assert!(matches!(
        list.failed_jobs[0].status,
        crate::v2::api_structs::FailedJobStatusKind::NotFound
    ));

    // A delete removes the live and the archived workflow and the job.
    Mock::given(method("DELETE"))
        .and(path("/api/v1/workflows/bakta/wf-done"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&argo)
        .await;
    state
        .delete_job((id, TEST_SECRET.to_string()))
        .await
        .unwrap();
    assert!(
        state
            .user_job_state(id, TEST_SECRET)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_failed_record_lookup_is_a_temporary_error() {
    let unreachable =
        crate::stats::StatsDb::connect_lazy("postgres://bakta@127.0.0.1:1/bakta").unwrap();
    let argo = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&argo)
        .await;
    let handler = test_handler_with_stats(
        &argo.uri(),
        "https://s3.example.com",
        crate::stats::StatsRecorder::capturing_with_db(unreachable).0,
    );
    let router = test_router(handler);
    let id = Uuid::new_v4();
    let reference = json!({"job_id": id, "secret": TEST_SECRET});
    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/job/list".to_string(),
            Some(json!({"jobs": [{"jobID": id, "secret": TEST_SECRET}]})),
        ),
        (
            "POST",
            "/api/v2/job/list".to_string(),
            Some(json!({"jobs": [reference]})),
        ),
        (
            "POST",
            "/api/v2/job/result".to_string(),
            Some(reference.clone()),
        ),
        (
            "GET",
            format!("/api/v2/job/logs?job_id={id}&secret={TEST_SECRET}"),
            None,
        ),
        (
            "DELETE",
            format!("/api/v2/job/delete?job_id={id}&secret={TEST_SECRET}"),
            None,
        ),
        (
            "DELETE",
            format!("/api/v1/job/delete?jobID={id}&secret={TEST_SECRET}"),
            None,
        ),
    ] {
        let (status, body) = call(&router, method, &uri, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}: {body}");
    }
}

#[tokio::test]
async fn a_delete_succeeds_only_when_its_tombstone_is_written() {
    let Some(db) = crate::stats::test_db::test_db("a_delete_succeeds_only_when").await else {
        return;
    };
    let argo = MockServer::start().await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&argo)
        .await;
    let (by_user, _) = recorded_job(&db, Utc::now() - chrono::Duration::hours(1)).await;
    let (by_admin, _) = recorded_job(&db, Utc::now() - chrono::Duration::hours(1)).await;
    let setup = admin_setup(&argo.uri(), "https://s3.example.com", Some((*db).clone())).await;
    let user_delete = format!("/api/v2/job/delete?job_id={by_user}&secret={TEST_SECRET}");
    let admin_delete = format!("/api/admin/jobs/{by_admin}");

    fail_job_writes(&db).await;
    let (status, body) = call(&setup.router, "DELETE", &user_delete, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let (status, body) = admin_call(&setup, "DELETE", &admin_delete, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    allow_job_writes(&db).await;

    // Repeated deletes succeed once the tombstones are written.
    for (id, delete) in [(by_user, &user_delete), (by_admin, &admin_delete)] {
        let mut deleted = false;
        for _ in 0..40 {
            let (status, _) = if *delete == admin_delete {
                admin_call(&setup, "DELETE", delete, None).await
            } else {
                let (status, body) = call(&setup.router, "DELETE", delete, None).await;
                (status, serde_json::Value::String(body))
            };
            if status == StatusCode::OK {
                deleted = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert!(deleted, "{delete}");
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(status, "deleted", "{delete}");
    }
}

/// Runs `request` while the job records are locked, and drops `id` from
/// memory (like the cleanup after a finish) while the request waits for
/// the database.
async fn during_handoff<T>(
    db: &crate::stats::StatsDb,
    handler: &BaktaHandler,
    id: Uuid,
    request: impl std::future::Future<Output = T>,
) -> T {
    let mut lock = db.pool().begin().await.unwrap();
    sqlx::query("LOCK TABLE jobs IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let drop_job = async {
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
        handler.state_handler.job_state.write().await.remove(&id);
        lock.rollback().await.unwrap();
    };
    let (result, ()) = tokio::join!(request, drop_job);
    result
}

#[tokio::test]
async fn a_job_that_leaves_memory_during_a_status_request_is_found() {
    let Some(db) = crate::stats::test_db::test_db("a_job_that_leaves_memory").await else {
        return;
    };
    let (historical, _) = recorded_job(&db, Utc::now() - chrono::Duration::hours(2)).await;
    let (finished, _) = recorded_job(&db, Utc::now() - chrono::Duration::minutes(1)).await;
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        crate::stats::StatsRecorder::capturing_with_db((*db).clone()).0,
    );
    let state = &handler.state_handler;
    let in_memory = state
        .user_job_state(finished, TEST_SECRET)
        .await
        .unwrap()
        .unwrap();

    insert_state(&handler, in_memory.clone()).await;
    let list = during_handoff(
        &db,
        &handler,
        finished,
        state.get_job_states(vec![
            Job {
                id: historical,
                secret: TEST_SECRET.to_string(),
            },
            Job {
                id: finished,
                secret: TEST_SECRET.to_string(),
            },
        ]),
    )
    .await
    .unwrap();
    assert!(list.failed.is_empty(), "v1");
    assert_eq!(list.jobs.len(), 2, "v1");

    insert_state(&handler, in_memory).await;
    let list = during_handoff(
        &db,
        &handler,
        finished,
        state.get_job_states_v2(vec![
            JobReference {
                job_id: historical,
                secret: TEST_SECRET.to_string(),
            },
            JobReference {
                job_id: finished,
                secret: TEST_SECRET.to_string(),
            },
        ]),
    )
    .await
    .unwrap();
    assert!(list.failed_jobs.is_empty(), "v2");
    assert_eq!(list.jobs.len(), 2, "v2");
}

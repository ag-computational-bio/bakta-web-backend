//! Tests for the job statistics hooks in the job lifecycle.

use crate::{
    api_structs::ArgoStatus,
    argo::structs::SimpleStatus,
    bakta_handler::ApiVersion,
    stats::{JobVersions, StatsDb, StatsEvent, StatsRecorder, test_db::test_db},
    test_support::*,
    v2::api_structs::{ResultKind, WorkflowKind},
};
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::Row;
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

/// Waits until the job row matches `check`, because recording runs in the background.
async fn wait_for_row(
    db: &StatsDb,
    id: Uuid,
    check: impl Fn(&sqlx::postgres::PgRow) -> bool,
) -> sqlx::postgres::PgRow {
    for _ in 0..100 {
        if let Some(row) = sqlx::query("SELECT * FROM jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(db.pool())
            .await
            .unwrap()
            && check(&row)
        {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job {id} did not reach the expected state");
}

fn status_of(row: &sqlx::postgres::PgRow) -> String {
    row.get("status")
}

fn workflow_item(
    id: Uuid,
    secret: &str,
    phase: &str,
    finished: bool,
    message: Option<&str>,
) -> SimpleStatus {
    serde_json::from_value(json!({
        "metadata": {
            "name": "wf-1",
            "uid": Uuid::new_v4(),
            "labels": {
                "jobid": id.to_string(), "secret": secret, "name": "sample",
                "api-version": "v2", "workflow-kind": "bakta", "bakta-environment": "production"
            }
        },
        "status": {
            "phase": phase,
            "startedAt": (Utc::now() - Duration::minutes(10)).to_rfc3339(),
            "finishedAt": finished.then(|| Utc::now().to_rfc3339()),
            "message": message,
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn v2_start_accepts_input_stats_and_sets_environment_label() {
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/workflows/bakta/submit"))
        .respond_with(submit_response("wf"))
        .mount(&argo)
        .await;
    let handler = test_handler(&argo.uri(), "https://s3.example.com");
    let id = v2_state(&handler, WorkflowKind::Bakta, None).await;
    let router = test_router(handler);
    let (status, body) = call(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(json!({
            "job": {"job_id": id, "secret": TEST_SECRET},
            "workflow_kind": "bakta",
            "config": {},
            "input_stats": {"sequence_count": 3, "total_length": 5594605}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requests = argo.received_requests().await.unwrap();
    let sent: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let labels = sent["submitOptions"]["labels"].as_str().unwrap();
    assert!(labels.contains("bakta-environment=production"), "{labels}");
}

#[tokio::test]
async fn v2_job_lifecycle_is_recorded() {
    let Some(db) = test_db("v2_job_lifecycle_is_recorded").await else {
        return;
    };
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(submit_response("wf-1"))
        .mount(&argo)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&argo)
        .await;
    let versions = JobVersions {
        bakta_version: Some("1.9.0".to_string()),
        bakta_db_version: Some("5.1".to_string()),
        baktfold_version: Some("0.7.0".to_string()),
        baktfold_db_version: Some("1.0".to_string()),
    };
    let handler = test_handler_with_stats(
        &argo.uri(),
        "https://s3.example.com",
        StatsRecorder::new(db.clone(), versions),
    );
    let router = test_router(handler.clone());

    let (_, body) = call(
        &router,
        "POST",
        "/api/v2/job/init",
        Some(json!({"name": "my sample", "workflow_kind": "bakta"})),
    )
    .await;
    let id: Uuid = parse(&body)["job"]["job_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let secret = parse(&body)["job"]["secret"].as_str().unwrap().to_string();
    let row = wait_for_row(&db, id, |row| status_of(row) == "created").await;
    assert_eq!(row.get::<String, _>("environment"), "production");
    assert_eq!(row.get::<String, _>("api_version"), "v2");

    let (status, _) = call(
        &router,
        "POST",
        "/api/v2/job/start",
        Some(json!({
            "job": {"job_id": id, "secret": secret},
            "workflow_kind": "bakta",
            "config": {"genus": "Escherichia", "species": "coli", "strain": ""},
            "input_stats": {"sequence_count": 3, "total_length": 5594605}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = wait_for_row(&db, id, |row| status_of(row) == "submitted").await;
    assert_eq!(
        row.get::<Option<String>, _>("genus").as_deref(),
        Some("Escherichia")
    );
    assert_eq!(row.get::<Option<String>, _>("strain"), None);
    assert_eq!(row.get::<Option<i32>, _>("sequence_count"), Some(3));
    assert_eq!(row.get::<Option<i64>, _>("total_length"), Some(5_594_605));
    assert_eq!(
        row.get::<Option<String>, _>("workflow_name").as_deref(),
        Some("wf-1")
    );
    assert_eq!(
        row.get::<Option<String>, _>("bakta_version").as_deref(),
        Some("1.9.0")
    );
    assert_eq!(row.get::<Option<String>, _>("baktfold_version"), None);
    let config: Value = row.get::<Option<Value>, _>("config").unwrap();
    assert_eq!(config["genus"], "Escherichia");
    assert!(!config.to_string().contains(&secret));

    let state = &handler.state_handler;
    state
        .apply_workflow_statuses(
            vec![workflow_item(id, &secret, "Running", false, None)],
            Utc::now(),
        )
        .await;
    wait_for_row(&db, id, |row| status_of(row) == "running").await;
    state
        .apply_workflow_statuses(
            vec![workflow_item(
                id,
                &secret,
                "Failed",
                true,
                Some("OOMKilled"),
            )],
            Utc::now(),
        )
        .await;
    let row = wait_for_row(&db, id, |row| status_of(row) == "failed").await;
    assert_eq!(
        row.get::<Option<String>, _>("outcome").as_deref(),
        Some("failed")
    );
    assert_eq!(
        row.get::<Option<String>, _>("error_message").as_deref(),
        Some("OOMKilled")
    );
    assert!(row.get::<Option<f64>, _>("runtime_seconds").is_some());
    // The poll keeps the environment the job was recorded with at start.
    assert_eq!(row.get::<String, _>("environment"), "production");

    let (status, _) = call(
        &router,
        "DELETE",
        &format!("/api/v2/job/delete?job_id={id}&secret={secret}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = wait_for_row(&db, id, |row| status_of(row) == "deleted").await;
    assert_eq!(
        row.get::<Option<String>, _>("outcome").as_deref(),
        Some("failed")
    );
}

#[tokio::test]
async fn v1_start_and_expiry_are_recorded() {
    let Some(db) = test_db("v1_start_and_expiry_are_recorded").await else {
        return;
    };
    let argo = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(submit_response("bakta-job-x"))
        .mount(&argo)
        .await;
    let handler = test_handler_with_stats(
        &argo.uri(),
        "https://s3.example.com",
        StatsRecorder::new(db.clone(), JobVersions::default()),
    );
    let router = test_router(handler.clone());
    let (_, body) = call(
        &router,
        "POST",
        "/api/v1/job/init",
        Some(json!({"name": "v1 job", "repliconTableType": "TSV"})),
    )
    .await;
    let id: Uuid = parse(&body)["job"]["jobID"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let secret = parse(&body)["job"]["secret"].as_str().unwrap().to_string();
    let (status, _) = call(
        &router,
        "POST",
        "/api/v1/job/start",
        Some(json!({
            "job": {"jobID": id, "secret": secret},
            "config": {
                "prodigalTrainingFile": "some-training-file", "hasReplicons": false,
                "translationTable": 11, "completeGenome": false, "keepContigHeaders": false,
                "minContigLength": 1, "compliant": false, "genus": "Bacillus"
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = wait_for_row(&db, id, |row| status_of(row) == "submitted").await;
    let config: Value = row.get::<Option<Value>, _>("config").unwrap();
    assert_eq!(config["hasProdigalTrainingFile"], true);
    assert!(!config.to_string().contains("some-training-file"));
    assert_eq!(
        row.get::<Option<String>, _>("genus").as_deref(),
        Some("Bacillus")
    );

    // A job whose last update is older than the retention time expires.
    let mut old = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Succeeded),
    );
    old.id = id;
    old.started = Some(Utc::now() - Duration::days(60));
    old.updated = Some(Utc::now() - Duration::days(59));
    insert_state(&handler, old).await;
    handler
        .state_handler
        .apply_workflow_statuses(Vec::new(), Utc::now())
        .await;
    wait_for_row(&db, id, |row| status_of(row) == "expired").await;
}

fn job_workflow(
    id: Uuid,
    name: &str,
    phase: &str,
    started: chrono::DateTime<Utc>,
    finished: Option<chrono::DateTime<Utc>>,
) -> SimpleStatus {
    serde_json::from_value(json!({
        "metadata": {
            "name": name,
            "uid": Uuid::new_v4(),
            "creationTimestamp": started.to_rfc3339(),
            "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}
        },
        "status": {
            "phase": phase,
            "startedAt": started.to_rfc3339(),
            "finishedAt": finished.map(|time| time.to_rfc3339()),
        }
    }))
    .unwrap()
}

fn drain(events: &mut tokio::sync::mpsc::Receiver<StatsEvent>) -> Vec<StatsEvent> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    seen
}

#[tokio::test]
async fn expired_old_workflow_does_not_expire_its_resubmission() {
    let (stats, mut events) = StatsRecorder::capturing();
    let handler = test_handler_with_stats("http://127.0.0.1:1", "https://s3.example.com", stats);
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some("wf-b".to_string());
    let id = insert_state(&handler, state).await;
    let now = Utc::now();
    handler
        .state_handler
        .apply_workflow_statuses(
            vec![
                job_workflow(
                    id,
                    "wf-a",
                    "Succeeded",
                    now - Duration::days(41),
                    Some(now - Duration::days(40)),
                ),
                job_workflow(id, "wf-b", "Running", now - Duration::hours(1), None),
            ],
            now,
        )
        .await;

    let seen = drain(&mut events);
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, StatsEvent::Expired { .. })),
        "{seen:?}"
    );
    let states = handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-b"));
    assert!(matches!(states[&id].status, Some(ArgoStatus::Running)));
}

#[tokio::test]
async fn current_workflow_does_not_depend_on_started_at_or_list_order() {
    let now = Utc::now();
    // wf-a was retried, so its startedAt is newer than that of the resubmission wf-b.
    let id = Uuid::new_v4();
    let mut old = job_workflow(id, "wf-a", "Running", now, None);
    old.metadata.creation_timestamp = Some(now - Duration::hours(2));
    let new = job_workflow(id, "wf-b", "Running", now - Duration::hours(1), None);
    // Same creation time: the order is by name.
    let tied_id = Uuid::new_v4();
    let tied_a = job_workflow(tied_id, "wf-a", "Running", now, None);
    let tied_b = job_workflow(tied_id, "wf-b", "Running", now, None);

    for items in [
        vec![old.clone(), new.clone(), tied_a.clone(), tied_b.clone()],
        vec![new.clone(), old.clone(), tied_b.clone(), tied_a.clone()],
    ] {
        let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
        handler
            .state_handler
            .apply_workflow_statuses(items, now)
            .await;
        let states = handler.state_handler.job_state.read().await;
        assert_eq!(states[&id].workflowname.as_deref(), Some("wf-b"));
        assert_eq!(states[&tied_id].workflowname.as_deref(), Some("wf-b"));
        let live = handler.state_handler.live_workflows.read().unwrap();
        assert_eq!(live[&id].name, "wf-b");
    }
}

#[tokio::test]
async fn current_workflow_comes_from_the_database_after_a_restart() {
    let Some(db) = test_db("current_workflow_comes_from_the_database_after_a_restart").await else {
        return;
    };
    let now = Utc::now();
    let id = Uuid::new_v4();
    db.apply(&StatsEvent::Created(crate::stats::NewJob {
        id,
        environment: crate::stats::test_db::unique_environment(),
        workflow_kind: "bakta".to_string(),
        api_version: "v2".to_string(),
        name: None,
        created_at: now,
    }))
    .await
    .unwrap();
    db.apply(&StatsEvent::Resubmitted {
        id,
        previous_workflow_name: String::new(),
        workflow_name: "wf-a".to_string(),
        submitted_at: now,
    })
    .await
    .unwrap();
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        StatsRecorder::new(db.clone(), JobVersions::default()),
    );
    // wf-b is newer by every timestamp, but the database records wf-a.
    let old = job_workflow(id, "wf-a", "Running", now - Duration::hours(2), None);
    let new = job_workflow(id, "wf-b", "Failed", now, Some(now));
    handler
        .state_handler
        .apply_workflow_statuses(vec![new, old], now)
        .await;
    let states = handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-a"));
}

/// A workflow of the job created at `created` whose current attempt started
/// at `started`.
fn attempt(
    id: Uuid,
    name: &str,
    phase: &str,
    created: chrono::DateTime<Utc>,
    started: chrono::DateTime<Utc>,
    finished: Option<chrono::DateTime<Utc>>,
) -> SimpleStatus {
    let mut workflow = job_workflow(id, name, phase, started, finished);
    workflow.metadata.creation_timestamp = Some(created);
    workflow
}

fn micros(row: &sqlx::postgres::PgRow, column: &str) -> Option<i64> {
    row.get::<Option<chrono::DateTime<Utc>>, _>(column)
        .map(|time| time.timestamp_micros())
}

/// Argo mock for an admin retry of a failed `wf-1` that answers after a delay.
async fn slow_retry_argo() -> MockServer {
    let argo = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta/wf-1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"metadata": {"name": "wf-1"}, "status": {"phase": "Failed"}}),
            ),
        )
        .mount(&argo)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-1/retry"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(
                    json!({"metadata": {"name": "wf-1"}, "status": {"phase": "Running"}}),
                )
                .set_delay(std::time::Duration::from_millis(800)),
        )
        .mount(&argo)
        .await;
    argo
}

/// Runs an admin retry of the failed first attempt of `wf-1`. While Argo
/// answers, a poll sees `second`, the retried attempt. A later poll sees it
/// again. Returns the job record after the second poll.
async fn retry_while_the_poll_sees(
    db: &crate::stats::test_db::TestDb,
    second: impl Fn(Uuid) -> SimpleStatus,
    expected_status: &str,
) -> sqlx::postgres::PgRow {
    let argo = slow_retry_argo().await;
    let setup = std::sync::Arc::new(
        crate::admin_api_tests::admin_setup(
            &argo.uri(),
            "https://s3.example.com",
            Some((**db).clone()),
        )
        .await,
    );
    let mut state = job_state(
        ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Running),
    );
    state.workflowname = Some("wf-1".to_string());
    let id = insert_state(&setup.handler, state).await;
    let created = Utc::now() - Duration::minutes(30);
    let first_started = created + Duration::minutes(1);
    let first = attempt(
        id,
        "wf-1",
        "Failed",
        created,
        first_started,
        Some(created + Duration::minutes(5)),
    );
    let poll = |item: SimpleStatus| {
        let setup = setup.clone();
        async move {
            setup
                .handler
                .state_handler
                .apply_workflow_statuses(vec![item], Utc::now())
                .await
        }
    };
    poll(first).await;
    wait_for_row(db, id, |row| status_of(row) == "failed").await;

    let retry = {
        let setup = setup.clone();
        tokio::spawn(async move {
            crate::admin_api_tests::admin_call(
                &setup,
                "POST",
                &format!("/api/admin/jobs/{id}/retry"),
                None,
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    poll(second(id)).await;
    let (status, body) = retry.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    poll(second(id)).await;
    // Background writes: wait until the row describes the retried attempt.
    wait_for_row(db, id, |row| {
        status_of(row) == expected_status
            && micros(row, "started_at") != Some(first_started.timestamp_micros())
    })
    .await
}

#[tokio::test]
async fn quick_retry_that_fails_again_is_recorded_as_failed() {
    let Some(db) = test_db("quick_retry_that_fails_again_is_recorded_as_failed").await else {
        return;
    };
    // The retried attempt fails again at once, so the job stays in the Failed phase.
    let started = Utc::now() + Duration::seconds(1);
    let finished = started + Duration::seconds(2);
    let created = Utc::now() - Duration::minutes(30);
    let row = retry_while_the_poll_sees(
        &db,
        |id| attempt(id, "wf-1", "Failed", created, started, Some(finished)),
        "failed",
    )
    .await;
    assert_eq!(micros(&row, "started_at"), Some(started.timestamp_micros()));
    assert_eq!(
        micros(&row, "finished_at"),
        Some(finished.timestamp_micros())
    );
    assert_eq!(row.get::<Option<f64>, _>("runtime_seconds"), Some(2.0));
}

#[tokio::test]
async fn retried_attempt_seen_before_the_retry_returns_keeps_its_own_times() {
    let Some(db) = test_db("retried_attempt_seen_before_the_retry_returns").await else {
        return;
    };
    let started = Utc::now() + Duration::seconds(1);
    let finished = started + Duration::seconds(90);
    let created = Utc::now() - Duration::minutes(30);
    let row = retry_while_the_poll_sees(
        &db,
        |id| attempt(id, "wf-1", "Succeeded", created, started, Some(finished)),
        "succeeded",
    )
    .await;
    assert_eq!(
        row.get::<Option<String>, _>("outcome").as_deref(),
        Some("succeeded")
    );
    assert_eq!(micros(&row, "started_at"), Some(started.timestamp_micros()));
    assert_eq!(row.get::<Option<f64>, _>("runtime_seconds"), Some(90.0));
}

#[tokio::test]
async fn late_resubmit_does_not_reset_a_finished_workflow_of_a_database_only_job() {
    let Some(db) = test_db("late_resubmit_does_not_reset_a_finished_workflow").await else {
        return;
    };
    let id = Uuid::new_v4();
    let created = Utc::now() - Duration::hours(1);
    let job = crate::stats::NewJob {
        id,
        environment: "production".to_string(),
        workflow_kind: "bakta".to_string(),
        api_version: "v2".to_string(),
        name: None,
        created_at: created,
    };
    db.apply(&StatsEvent::Created(job.clone())).await.unwrap();
    db.apply(&StatsEvent::Submitted(Box::new(
        crate::stats::SubmittedJob {
            job,
            genus: None,
            species: None,
            strain: None,
            sequence_count: None,
            total_length: None,
            config: None,
            versions: JobVersions::default(),
            workflow_name: "wf-a".to_string(),
            submitted_at: created,
        },
    )))
    .await
    .unwrap();
    let now = Utc::now();
    let argo = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-a/resubmit"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "metadata": {"name": "wf-b", "creationTimestamp": now.to_rfc3339()},
                    "status": {}
                }))
                .set_delay(std::time::Duration::from_millis(800)),
        )
        .mount(&argo)
        .await;
    let setup = std::sync::Arc::new(
        crate::admin_api_tests::admin_setup(
            &argo.uri(),
            "https://s3.example.com",
            Some(db.clone()),
        )
        .await,
    );
    // The job has no in-memory state, only its record.
    let resubmit = {
        let setup = setup.clone();
        tokio::spawn(async move {
            crate::admin_api_tests::admin_call(
                &setup,
                "POST",
                &format!("/api/admin/jobs/{id}/resubmit"),
                None,
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // wf-a is gone. The poll sees wf-b finish before the resubmit call returns.
    let finished = attempt(
        id,
        "wf-b",
        "Succeeded",
        now,
        now + Duration::seconds(5),
        Some(now + Duration::seconds(65)),
    );
    let poll = || {
        setup
            .handler
            .state_handler
            .apply_workflow_statuses(vec![finished.clone()], Utc::now())
    };
    poll().await;
    let (status, body) = resubmit.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    poll().await;
    let row = wait_for_row(&db, id, |row| {
        status_of(row) == "succeeded"
            && row.get::<Option<String>, _>("workflow_name").as_deref() == Some("wf-b")
    })
    .await;
    assert_eq!(
        row.get::<Option<String>, _>("outcome").as_deref(),
        Some("succeeded")
    );
    assert_eq!(row.get::<Option<f64>, _>("runtime_seconds"), Some(60.0));
}

#[tokio::test]
async fn resubmit_of_a_database_only_job_is_current_before_the_writer_catches_up() {
    let Some(db) = test_db("resubmit_of_a_database_only_job_is_current").await else {
        return;
    };
    let id = Uuid::new_v4();
    let created = Utc::now() - Duration::hours(1);
    db.apply(&StatsEvent::Created(crate::stats::NewJob {
        id,
        environment: crate::stats::test_db::unique_environment(),
        workflow_kind: "bakta".to_string(),
        api_version: "v2".to_string(),
        name: None,
        created_at: created,
    }))
    .await
    .unwrap();
    db.apply(&StatsEvent::Resubmitted {
        id,
        previous_workflow_name: String::new(),
        workflow_name: "wf-a".to_string(),
        submitted_at: created,
    })
    .await
    .unwrap();
    let now = Utc::now();
    let argo = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/workflows/bakta/wf-a/resubmit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "metadata": {
                "name": "wf-b",
                "creationTimestamp": now.to_rfc3339(),
                "labels": {"jobid": id.to_string(), "secret": TEST_SECRET, "api-version": "v2"}
            },
            "status": {}
        })))
        .mount(&argo)
        .await;
    // The writer is behind: events stay in the queue, the database still names wf-a.
    let (stats, mut events) = StatsRecorder::capturing_with_db(db.clone());
    let setup = crate::admin_api_tests::admin_setup_with_stats(
        &argo.uri(),
        "https://s3.example.com",
        stats,
    )
    .await;
    let (status, body) = crate::admin_api_tests::admin_call(
        &setup,
        "POST",
        &format!("/api/admin/jobs/{id}/resubmit"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let old = attempt(
        id,
        "wf-a",
        "Succeeded",
        created,
        created,
        Some(created + Duration::minutes(5)),
    );
    let new = attempt(id, "wf-b", "Pending", now, now, None);
    setup
        .handler
        .state_handler
        .apply_workflow_statuses(vec![old, new], Utc::now())
        .await;

    let states = setup.handler.state_handler.job_state.read().await;
    assert_eq!(states[&id].workflowname.as_deref(), Some("wf-b"));
    assert_eq!(states[&id].secret.as_deref(), Some(TEST_SECRET));
    drop(states);
    let polled: Vec<String> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            StatsEvent::Polled(states) => Some(states),
            _ => None,
        })
        .flatten()
        .map(|state| state.workflow_name)
        .collect();
    assert_eq!(polled, ["wf-b"]);
}

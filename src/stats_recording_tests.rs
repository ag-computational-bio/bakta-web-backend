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
                "api-version": "v2", "workflow-kind": "bakta", "bakta-environment": "staging"
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

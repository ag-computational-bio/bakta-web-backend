//! Tests of the workflow list and watch against a mocked Kubernetes API.

use crate::{
    informer::{KubeApi, start},
    stats::{JobVersions, StatsRecorder, secret_hash, test_db::test_db},
    test_support::*,
};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param, query_param_is_missing},
};

const WORKFLOWS: &str = "/apis/argoproj.io/v1alpha1/namespaces/bakta/workflows";
const LIVE: &str = "workflows.argoproj.io/completed!=true";
const COMPLETED: &str = "workflows.argoproj.io/completed=true";

/// A Workflow object as the Kubernetes API returns it.
fn kube_workflow(name: &str, job: Uuid, phase: &str, completed: bool, version: u32) -> Value {
    let started = Utc::now() - Duration::minutes(10);
    let mut labels = json!({
        "jobid": job.to_string(), "secret": TEST_SECRET, "api-version": "v2",
        "bakta-environment": "production", "workflow-kind": "bakta",
    });
    if completed {
        labels["workflows.argoproj.io/completed"] = "true".into();
    }
    json!({
        "metadata": {"name": name, "uid": Uuid::new_v4(), "resourceVersion": version.to_string(),
                     "creationTimestamp": started.to_rfc3339(), "labels": labels},
        "spec": {"workflowTemplateRef": {"name": "bakta-job-1.9.0"}, "arguments": {"parameters": []}},
        "status": {"phase": phase, "startedAt": started.to_rfc3339(),
                   "finishedAt": completed.then(|| Utc::now().to_rfc3339()),
                   "nodes": {"ignored": {"phase": phase}}}
    })
}

fn event(kind: &str, object: Value) -> String {
    format!("{}\n", json!({"type": kind, "object": object}))
}

fn page(items: Vec<Value>, next: &str, version: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "metadata": {"continue": next, "resourceVersion": version}, "items": items,
    }))
}

async fn wait_for<F: std::future::Future<Output = bool>>(what: &str, check: impl Fn() -> F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn job_status(db: &crate::stats::StatsDb, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .unwrap()
}

async fn updated_at(db: &crate::stats::StatsDb, id: Uuid) -> chrono::DateTime<Utc> {
    sqlx::query_scalar("SELECT updated_at FROM jobs WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Writes the final state of a completed workflow, as a completion does.
async fn record_final_state(db: &crate::stats::StatsDb, workflow: &Value) {
    let mut item =
        serde_json::from_value::<crate::backfill::workflows::KubeWorkflow>(workflow.clone())
            .unwrap()
            .into_simple_status();
    let secret = item.metadata.labels.remove("secret").unwrap();
    let state = crate::bakta_handler::state_from_simple_status(item.clone()).unwrap();
    let environment = crate::bakta_handler::workflow_environment(&item.metadata.labels);
    let mut polled =
        crate::bakta_handler::polled_workflow_state(&state, environment, &item).unwrap();
    polled.job.secret_hash = Some(secret_hash(&secret));
    db.apply(&crate::stats::StatsEvent::Polled(vec![polled]))
        .await
        .unwrap();
}

async fn live_names(db: &crate::stats::StatsDb) -> Vec<String> {
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
async fn the_watch_follows_live_workflows_and_records_completions_once() {
    let Some(db) = test_db("the_watch_follows_live_workflows").await else {
        return;
    };
    let kube = MockServer::start().await;
    let argo = MockServer::start().await;
    let (finishing, pending, short, done, kept) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let (quick, recorded) = (Uuid::new_v4(), Uuid::new_v4());
    // A completed workflow whose final state the database has already.
    let recorded_workflow = kube_workflow("wf-recorded", recorded, "Succeeded", true, 40);
    record_final_state(&db, &recorded_workflow).await;
    let recorded_at = updated_at(&db, recorded).await;
    // Live workflows, listed in two pages.
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param("limit", "500"))
        .and(query_param_is_missing("continue"))
        .and(query_param_is_missing("watch"))
        .respond_with(page(
            vec![kube_workflow(
                "wf-finishing",
                finishing,
                "Running",
                false,
                90,
            )],
            "page-2",
            "99",
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&kube)
        .await;
    // The second list no longer has the completed workflow.
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param_is_missing("continue"))
        .and(query_param_is_missing("watch"))
        .respond_with(page(Vec::new(), "page-2", "99"))
        .with_priority(2)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param("continue", "page-2"))
        .respond_with(page(
            vec![
                kube_workflow("wf-pending", pending, "Pending", false, 91),
                kube_workflow("wf-kept", kept, "Running", false, 92),
            ],
            "",
            "100",
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param("continue", "page-2"))
        .respond_with(page(
            vec![
                kube_workflow("wf-pending", pending, "Running", false, 102),
                kube_workflow("wf-short", short, "Running", false, 101),
            ],
            "",
            "100",
        ))
        .with_priority(2)
        .mount(&kube)
        .await;
    // Completed workflows, read once after each list.
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .respond_with(page(
            vec![
                kube_workflow("wf-done", done, "Succeeded", true, 50),
                recorded_workflow.clone(),
            ],
            "",
            "100",
        ))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .respond_with(page(
            vec![
                kube_workflow("wf-done", done, "Succeeded", true, 50),
                recorded_workflow.clone(),
                kube_workflow("wf-finishing", finishing, "Succeeded", true, 106),
            ],
            "",
            "120",
        ))
        .with_priority(2)
        .mount(&kube)
        .await;
    // The first watch: a new workflow, a change, a completion, a real
    // delete and a bookmark. Then Kubernetes ends the watch.
    let body = [
        event(
            "ADDED",
            kube_workflow("wf-short", short, "Running", false, 101),
        ),
        event(
            "MODIFIED",
            kube_workflow("wf-pending", pending, "Running", false, 102),
        ),
        // The state before the workflow left the watched set.
        event(
            "DELETED",
            kube_workflow("wf-finishing", finishing, "Running", false, 106),
        ),
        event(
            "DELETED",
            kube_workflow("wf-kept", kept, "Running", false, 107),
        ),
        // A change that already has the completed label needs no fetch.
        event(
            "MODIFIED",
            kube_workflow("wf-quick", quick, "Failed", true, 108),
        ),
        event("BOOKMARK", json!({"metadata": {"resourceVersion": "110"}})),
    ]
    .concat();
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .and(query_param("labelSelector", LIVE))
        .and(query_param("allowWatchBookmarks", "true"))
        .and(query_param("resourceVersion", "100"))
        // Late enough that the list is applied first.
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .set_delay(std::time::Duration::from_millis(500)),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&kube)
        .await;
    // The resumed watch starts at the bookmark and is too old: list again.
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .and(query_param("resourceVersion", "110"))
        .respond_with(ResponseTemplate::new(200).set_body_string(event(
            "ERROR",
            json!({"kind": "Status", "code": 410, "reason": "Expired", "message": "too old"}),
        )))
        .mount(&kube)
        .await;
    // Later watches send nothing.
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .with_priority(5)
        .mount(&kube)
        .await;
    // The completion is fetched once with its final state, the deleted
    // workflow is not found.
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-finishing")))
        .respond_with(ResponseTemplate::new(200).set_body_json(kube_workflow(
            "wf-finishing",
            finishing,
            "Succeeded",
            true,
            106,
        )))
        .expect(1..)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-quick")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-kept")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&kube)
        .await;

    let handler = test_handler_with_stats(
        &argo.uri(),
        "https://s3.example.com",
        StatsRecorder::new((*db).clone(), JobVersions::default()),
    );
    let state = handler.state_handler.clone();
    start(
        state.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );

    wait_for("the completion", || async {
        job_status(&db, finishing).await.as_deref() == Some("succeeded")
    })
    .await;
    wait_for("the completion without fetch", || async {
        job_status(&db, quick).await.as_deref() == Some("failed")
    })
    .await;
    wait_for("the reconciled job", || async {
        job_status(&db, done).await.as_deref() == Some("succeeded")
    })
    .await;
    wait_for("the re-list", || async {
        kube.received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| {
                request
                    .url
                    .query()
                    .unwrap_or("")
                    .contains("continue=page-2")
            })
            .count()
            >= 2
    })
    .await;
    wait_for("the table", || async {
        live_names(&db).await
            == [
                "wf-done",
                "wf-finishing",
                "wf-pending",
                "wf-quick",
                "wf-recorded",
                "wf-short",
            ]
    })
    .await;

    // Only live workflows stay watched.
    let mut watched: Vec<String> = state.workflows.read().unwrap().keys().cloned().collect();
    watched.sort();
    assert_eq!(watched, ["wf-pending", "wf-short"]);
    let job_state = state.job_state.read().await;
    assert!(matches!(
        job_state[&pending].status,
        Some(crate::api_structs::ArgoStatus::Running)
    ));
    // A real delete keeps the last known state of the job.
    assert!(matches!(
        job_state[&kept].status,
        Some(crate::api_structs::ArgoStatus::Running)
    ));
    assert!(job_state.contains_key(&short));
    assert!(!job_state.contains_key(&done));
    drop(job_state);
    let live = state.live_workflows.read().unwrap().clone();
    assert!(!live.contains_key(&kept));
    assert!(live.contains_key(&short));
    // The table has the final state and the reconciled job the secret hash.
    let finished = db
        .live_workflows("bakta", None)
        .await
        .unwrap()
        .into_iter()
        .find(|workflow| workflow.metadata.name == "wf-finishing")
        .unwrap();
    assert_eq!(finished.status.phase, "Succeeded");
    assert!(!finished.metadata.labels.contains_key("secret"));
    let hash: Option<String> = sqlx::query_scalar("SELECT secret_hash FROM jobs WHERE id = $1")
        .bind(done)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(hash, Some(secret_hash(TEST_SECRET)));
    // A job that the database had in its final state is not written again.
    assert_eq!(updated_at(&db, recorded).await, recorded_at);
    // The finished jobs are served from the database.
    let served = state.user_job_state(done, TEST_SECRET).await.unwrap();
    assert!(served.secret_matches(TEST_SECRET));
    assert!(
        state
            .metrics
            .encode()
            .unwrap()
            .contains("workflow_relists_total 1")
    );

    // Nothing was asked from the Argo server, and completed workflows
    // were only listed after the two lists.
    assert!(argo.received_requests().await.unwrap().is_empty());
    let completed_lists = kube
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| {
            request
                .url
                .query_pairs()
                .any(|(key, value)| key == "labelSelector" && value == COMPLETED)
        })
        .count();
    assert_eq!(completed_lists, 2);
}

#[tokio::test]
async fn without_a_database_finished_jobs_stay_in_memory() {
    let kube = MockServer::start().await;
    let id = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param_is_missing("watch"))
        .respond_with(page(Vec::new(), "", "10"))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .respond_with(page(
            vec![kube_workflow("wf-done", id, "Succeeded", true, 5)],
            "",
            "10",
        ))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .mount(&kube)
        .await;
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let state = handler.state_handler.clone();
    start(
        state.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );
    wait_for("the finished job", || async {
        state.job_state.read().await.contains_key(&id)
    })
    .await;
    let job = state.job_state.read().await[&id].clone();
    assert!(job.secret_matches(TEST_SECRET));
    assert!(matches!(
        job.status,
        Some(crate::api_structs::ArgoStatus::Succeeded)
    ));
}

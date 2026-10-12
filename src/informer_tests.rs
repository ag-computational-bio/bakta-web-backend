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
    let row = crate::argo::structs::ArgoWorkflow::from(&item);
    db.upsert_workflows("bakta", Utc::now() - Duration::hours(1), &[row])
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
    // The reconcile fetches only the completed workflow that the table
    // does not have.
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-done")))
        .respond_with(ResponseTemplate::new(200).set_body_json(kube_workflow(
            "wf-done",
            done,
            "Succeeded",
            true,
            50,
        )))
        .expect(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-recorded")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
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
    let served = state
        .user_job_state(done, TEST_SECRET)
        .await
        .unwrap()
        .unwrap();
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

const METADATA_ACCEPT: &str = "application/json;as=PartialObjectMetadataList;g=meta.k8s.io;v=v1";

/// Serves no live workflows, a watch without events and `completed` as the
/// completed workflows: their metadata with `metadata`, else in full. Single
/// fetches must not happen.
async fn reconcile_server(completed: &[Value], metadata: ResponseTemplate) -> MockServer {
    let kube = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param_is_missing("watch"))
        .respond_with(page(Vec::new(), "", "10"))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .and(wiremock::matchers::header("accept", METADATA_ACCEPT))
        .respond_with(metadata)
        .with_priority(1)
        .expect(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .respond_with(page(completed.to_vec(), "", "10"))
        .with_priority(2)
        .expect(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(format!("^{WORKFLOWS}/.+")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&kube)
        .await;
    kube
}

async fn reconcile_with(db: &crate::stats::StatsDb, kube: &MockServer, jobs: &[Uuid]) {
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        StatsRecorder::new((*db).clone(), JobVersions::default()),
    );
    start(
        handler.state_handler.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );
    for id in jobs {
        wait_for("the reconciled jobs", || async {
            job_status(db, *id).await.as_deref() == Some("succeeded")
        })
        .await;
    }
}

fn completed_workflows(count: usize) -> (Vec<Uuid>, Vec<Value>) {
    let jobs: Vec<Uuid> = (0..count).map(|_| Uuid::new_v4()).collect();
    let workflows = jobs
        .iter()
        .enumerate()
        .map(|(index, id)| kube_workflow(&format!("wf-{index}"), *id, "Succeeded", true, 7))
        .collect();
    (jobs, workflows)
}

#[tokio::test]
async fn many_missing_completed_workflows_are_fetched_as_a_full_page() {
    let Some(db) = test_db("many_missing_completed_workflows").await else {
        return;
    };
    let (jobs, workflows) = completed_workflows(60);
    let metadata: Vec<Value> = workflows
        .iter()
        .map(|workflow| json!({"metadata": workflow["metadata"]}))
        .collect();
    let kube = reconcile_server(&workflows, page(metadata, "", "10")).await;
    reconcile_with(&db, &kube, &jobs).await;
    assert_eq!(live_names(&db).await.len(), 60);
}

#[tokio::test]
async fn a_refused_metadata_list_falls_back_to_the_full_list() {
    let Some(db) = test_db("a_refused_metadata_list").await else {
        return;
    };
    let (jobs, workflows) = completed_workflows(2);
    let kube = reconcile_server(&workflows, ResponseTemplate::new(406)).await;
    reconcile_with(&db, &kube, &jobs).await;
    assert_eq!(live_names(&db).await.len(), 2);
}

#[tokio::test]
async fn ready_waits_for_the_first_list_and_the_database() {
    let Some(db) = test_db("ready_waits_for_the_first_list").await else {
        return;
    };
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        StatsRecorder::new((*db).clone(), JobVersions::default()),
    );
    let router = test_router(handler.clone());
    assert_eq!(
        call(&router, "GET", "/ready", None).await.0,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    // The first list is applied.
    let (_, workflows) = completed_workflows(60);
    let metadata: Vec<Value> = workflows
        .iter()
        .map(|workflow| json!({"metadata": workflow["metadata"]}))
        .collect();
    let kube = reconcile_server(&workflows, page(metadata, "", "10")).await;
    start(
        handler.state_handler.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );
    wait_for("readiness", || async {
        call(&router, "GET", "/ready", None).await.0 == axum::http::StatusCode::OK
    })
    .await;

    // A database that does not answer makes the backend not ready.
    let unreachable =
        crate::stats::StatsDb::connect_lazy("postgres://bakta@127.0.0.1:1/bakta").unwrap();
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        StatsRecorder::new(unreachable, JobVersions::default()),
    );
    handler
        .state_handler
        .ready
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        call(&test_router(handler), "GET", "/ready", None).await.0,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}

/// Serves `live` as the only watched workflows, a first watch with `events`,
/// later watches without events, and no completed workflows.
async fn watch_server(live: Vec<Value>, events: String) -> MockServer {
    let kube = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", LIVE))
        .and(query_param_is_missing("watch"))
        .respond_with(page(live, "", "100"))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("labelSelector", COMPLETED))
        .respond_with(page(Vec::new(), "", "100"))
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .and(query_param("resourceVersion", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(events)
                .set_delay(std::time::Duration::from_millis(500)),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(WORKFLOWS))
        .and(query_param("watch", "true"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(60)))
        .with_priority(5)
        .mount(&kube)
        .await;
    kube
}

#[tokio::test]
async fn a_failed_completion_fetch_is_retried_and_not_a_delete() {
    let Some(db) = test_db("a_failed_completion_fetch_is_retried").await else {
        return;
    };
    let id = Uuid::new_v4();
    let kube = watch_server(
        vec![kube_workflow("wf-x", id, "Running", false, 90)],
        event("DELETED", kube_workflow("wf-x", id, "Running", false, 101)),
    )
    .await;
    // Kubernetes fails for a while, then answers with the final state.
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-x")))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(4)
        .with_priority(1)
        .mount(&kube)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-x")))
        .respond_with(ResponseTemplate::new(200).set_body_json(kube_workflow(
            "wf-x",
            id,
            "Succeeded",
            true,
            101,
        )))
        .with_priority(2)
        .mount(&kube)
        .await;
    let handler = test_handler_with_stats(
        "http://127.0.0.1:1",
        "https://s3.example.com",
        StatsRecorder::new((*db).clone(), JobVersions::default()),
    );
    start(
        handler.state_handler.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );
    wait_for("the completion after the failed fetches", || async {
        job_status(&db, id).await.as_deref() == Some("succeeded")
    })
    .await;
    assert_eq!(live_names(&db).await, ["wf-x"]);
}

#[tokio::test]
async fn a_completion_of_a_reserved_job_is_applied_later() {
    let id = Uuid::new_v4();
    let kube = watch_server(
        vec![kube_workflow("wf-y", id, "Running", false, 90)],
        event("DELETED", kube_workflow("wf-y", id, "Running", false, 101)),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{WORKFLOWS}/wf-y")))
        .respond_with(ResponseTemplate::new(200).set_body_json(kube_workflow(
            "wf-y",
            id,
            "Succeeded",
            true,
            101,
        )))
        .mount(&kube)
        .await;
    let handler = test_handler("http://127.0.0.1:1", "https://s3.example.com");
    let state = handler.state_handler.clone();
    insert_state(
        &handler,
        crate::bakta_handler::FullJobState {
            id,
            workflowname: Some("wf-y".to_string()),
            ..job_state(
                crate::bakta_handler::ApiVersion::V2,
                crate::v2::api_structs::WorkflowKind::Bakta,
                crate::v2::api_structs::ResultKind::Bakta,
                Some(crate::api_structs::ArgoStatus::Running),
            )
        },
    )
    .await;
    // A start or delete holds the job while the completion arrives.
    let reservation = state.reserve_start(id).unwrap();
    start(
        state.clone(),
        KubeApi::new(
            reqwest::Client::new(),
            &kube.uri(),
            "token".to_string(),
            "bakta",
        ),
    );
    wait_for("the completion to arrive", || async {
        !state.workflows.read().unwrap().contains_key("wf-y")
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(matches!(
        state.job_state.read().await[&id].status,
        Some(crate::api_structs::ArgoStatus::Running)
    ));
    drop(reservation);
    wait_for("the completion to be applied", || async {
        matches!(
            state.job_state.read().await[&id].status,
            Some(crate::api_structs::ArgoStatus::Succeeded)
        )
    })
    .await;
}

//! Tests for the Argo client against a mock server, plus catalog and status edge cases.

use crate::{
    api_structs::{ArgoStatus, JobStatusEnum},
    argo::client::ArgoClient,
    test_support::*,
    v2::api_structs::{JobStatus, ResultKind, WorkflowKind},
    workflow_catalog::{workflow_descriptors, workflow_template_name},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn client(server: &MockServer) -> ArgoClient {
    ArgoClient::new("tok".to_string(), server.uri(), "bakta".to_string())
}

#[test]
fn template_name_requires_the_right_versions() {
    for kind in [WorkflowKind::Bakta, WorkflowKind::BaktaProteins] {
        assert!(workflow_template_name(kind, "", "0.7.0").is_err());
        assert!(workflow_template_name(kind, "  ", "").is_err());
        assert!(workflow_template_name(kind, "1.9.0", "").is_ok());
    }
    assert!(workflow_template_name(WorkflowKind::Baktfold, "1.9.0", "").is_err());
    assert!(workflow_template_name(WorkflowKind::Baktfold, "", "0.7.0").is_ok());
    assert!(workflow_template_name(WorkflowKind::BaktaBaktfold, "1.9.0", "").is_err());
    assert!(workflow_template_name(WorkflowKind::BaktaBaktfold, "", "0.7.0").is_err());
}

#[test]
fn template_name_trims_whitespace() {
    assert_eq!(
        workflow_template_name(WorkflowKind::Bakta, " 1.9.0\n", "").unwrap(),
        "bakta-job-1.9.0"
    );
}

#[test]
fn every_workflow_requires_at_least_one_upload_and_has_stages() {
    let descriptors = workflow_descriptors();
    assert_eq!(descriptors.len(), 4);
    for descriptor in descriptors {
        assert!(descriptor.uploads.iter().any(|u| u.required));
        assert!(!descriptor.stages.is_empty());
        assert_eq!(
            descriptor.uploads.iter().filter(|u| u.required).count(),
            1,
            "{:?}",
            descriptor.workflow_kind
        );
    }
}

#[test]
fn argo_status_parses_known_phases_only() {
    for (text, ok) in [
        ("Init", true),
        ("Pending", true),
        ("Running", true),
        ("Succeeded", true),
        ("Failed", true),
        ("Error", true),
        ("running", false),
        ("Skipped", false),
        ("", false),
    ] {
        assert_eq!(ArgoStatus::try_from(text.to_string()).is_ok(), ok, "{text}");
    }
}

#[test]
fn argo_status_maps_to_public_statuses() {
    assert!(matches!(
        JobStatusEnum::from(ArgoStatus::Pending),
        JobStatusEnum::INIT
    ));
    assert!(matches!(
        JobStatusEnum::from(ArgoStatus::Failed),
        JobStatusEnum::ERROR
    ));
    assert_eq!(JobStatus::from(ArgoStatus::Pending), JobStatus::Init);
    assert_eq!(JobStatus::from(ArgoStatus::Init), JobStatus::Init);
    assert_eq!(JobStatus::from(ArgoStatus::Running), JobStatus::Running);
    assert_eq!(
        JobStatus::from(ArgoStatus::Succeeded),
        JobStatus::Successful
    );
    assert_eq!(JobStatus::from(ArgoStatus::Failed), JobStatus::Error);
    assert_eq!(JobStatus::from(ArgoStatus::Error), JobStatus::Error);
}

#[tokio::test]
async fn workflow_status_is_parsed_and_sends_bearer_token() {
    let server = MockServer::start().await;
    let uid = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path("/api/v1/workflows/bakta"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "metadata": {
                    "name": "wf", "uid": uid, "resourceVersion": "7",
                    "labels": {"jobid": Uuid::new_v4()}
                },
                "status": {
                    "phase": "Running", "startedAt": "2026-01-01T00:00:00Z",
                    "finishedAt": null
                }
            }]
        })))
        .mount(&server)
        .await;
    let list = client(&server).get_workflow_status().await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!(list.items[0].metadata.uid, uid);
    assert_eq!(list.items[0].status.phase, "Running");
    assert!(list.items[0].status.finished_at.is_none());
}

#[tokio::test]
async fn workflow_status_reports_http_and_parse_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    assert!(client(&server).get_workflow_status().await.is_err());

    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    assert!(client(&server).get_workflow_status().await.is_err());
}

#[tokio::test]
async fn submit_reports_http_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let result = client(&server)
        .submit_from_template("missing".to_string(), None, None, None, None)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delete_workflow_picks_running_or_archived_url() {
    let server = MockServer::start().await;
    let uid = Uuid::new_v4();
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let argo = client(&server);

    let mut archived = job_state(
        crate::bakta_handler::ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        Some(ArgoStatus::Succeeded),
    );
    archived.archived = true;
    archived.argo_uid = Some(uid);
    argo.delete_workflow(&archived).await.unwrap();

    // Without a uid or workflow name there is nothing to delete, so no request is sent.
    archived.argo_uid = None;
    argo.delete_workflow(&archived).await.unwrap();
    let unstarted = job_state(
        crate::bakta_handler::ApiVersion::V2,
        WorkflowKind::Bakta,
        ResultKind::Bakta,
        None,
    );
    argo.delete_workflow(&unstarted).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn archived_details_need_a_uid() {
    let server = MockServer::start().await;
    let uid = Uuid::new_v4();
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .and(query_param("namespace", "bakta"))
        .and(query_param("name", "wf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": {"nodes": {"n": {"id": "n", "phase": "Succeeded"}}}
        })))
        .mount(&server)
        .await;
    let argo = client(&server);
    assert!(argo.get_workflow_details("wf", true, None).await.is_err());
    let details = argo
        .get_workflow_details("wf", true, Some(&uid))
        .await
        .unwrap();
    assert_eq!(details.status.nodes["n"].phase, "Succeeded");
}

#[tokio::test]
async fn archived_logs_fall_back_to_workflow_main_logs() {
    let server = MockServer::start().await;
    let uid = Uuid::new_v4();
    // No workflow details are available, so the combined main-logs artifact is used.
    Mock::given(method("GET"))
        .and(path(format!(
            "/artifact-files/bakta/archived-workflows/{uid}/wf/outputs/main-logs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_string("line one\nline two"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/archived-workflows/{uid}")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let entries = client(&server)
        .get_workflow_log_entries("wf", true, Some(&uid))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "line one\nline two");
}

#[tokio::test]
async fn archived_logs_without_uid_are_empty() {
    let server = MockServer::start().await;
    let entries = client(&server)
        .get_workflow_log_entries("wf", true, None)
        .await
        .unwrap();
    assert!(entries.is_empty());
}

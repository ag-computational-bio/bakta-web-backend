use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;

/// SUBMITWORKFLOW
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitWorkflowTemplate {
    pub namespace: String,
    pub resource_kind: String,
    pub resource_name: String,
    pub submit_options: SubmitOptions,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitOptions {
    pub labels: Option<String>,
    pub parameters: Option<Vec<String>>,
    pub service_account: Option<String>,
    pub generate_name: Option<String>,
}

/// GET WORKFLOWS REQUEST
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowResponse {
    pub metadata: Metadata,
    pub items: Vec<Item>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub resource_version: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub metadata: WorkflowMetadata,
    pub spec: Spec,
    pub status: Status,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowMetadata {
    pub name: String,
    pub generate_name: String,
    pub namespace: String,
    pub uid: String,
    pub resource_version: String,
    pub generation: i64,
    pub creation_timestamp: String,
    pub labels: HashMap<String, String>,
    pub annotations: HashMap<String, String>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub arguments: Arguments,
    pub workflow_template_ref: WorkflowTemplateRef,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Arguments {
    pub parameters: Vec<Parameter>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub name: String,
    pub value: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTemplateRef {
    pub name: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub phase: String,
    pub started_at: String,
    pub finished_at: String,
    pub estimated_duration: i64,
    pub progress: String,
    pub conditions: Vec<Condition>,
    pub resources_duration: ResourcesDuration,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub type_field: String,
    pub status: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDuration {
    pub cpu: i64,
    pub memory: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "items")]
pub struct SimpleStatusList {
    pub items: Vec<SimpleStatus>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleStatus {
    pub metadata: StatusMetadata,
    pub status: SimpleStatusStatus,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusMetadata {
    pub name: String,
    pub labels: HashMap<String, String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleStatusStatus {
    pub phase: String,
    #[serde(rename = "startedAt")]
    pub started_at: chrono::DateTime<Utc>,
    #[serde(rename = "finishedAt")]
    pub finished_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResult {
    pub metadata: SubmitResultMetadata,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResultMetadata {
    pub name: String,
    #[serde(rename = "creationTimestamp")]
    pub creation_timestamp: DateTime<Utc>,
}

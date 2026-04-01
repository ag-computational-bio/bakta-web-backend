use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;
use uuid::Uuid;

use crate::{api_structs::ArgoStatus, bakta_handler::FullJobState};

use super::{
    structs::{
        Content, LogResult, SimpleStatusList, SubmitOptions, SubmitResult, SubmitWorkflowTemplate,
        WorkflowDetails,
    },
    urls::{
        get_delete_url_archived, get_delete_url_running, get_logs_archived_url,
        get_logs_running_url, get_status_url_bakta, get_submit_url, get_workflow_url,
    },
};

pub struct ArgoClient {
    token: String,
    url: String,
    namespace: String,
    client: Client,
}

impl ArgoClient {
    pub fn new(token: String, url: String, namespace: String) -> ArgoClient {
        ArgoClient {
            token,
            url,
            namespace,
            client: reqwest::Client::new(),
        }
    }
}

impl ArgoClient {
    async fn parse_log_entries(&self, response: String) -> Result<Vec<Content>> {
        let mut entries = Vec::new();

        for line in response.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let content = match serde_json::from_str::<LogResult>(line) {
                Ok(content) => content.result,
                Err(_) => Content {
                    content: line.to_string(),
                    pod_name: None,
                },
            };

            if !content.content.contains("argo=true") {
                entries.push(content);
            }
        }

        Ok(entries)
    }

    fn workflow_logs_url(
        &self,
        workflow_name: &str,
        archived: bool,
        argo_uid: Option<&Uuid>,
    ) -> Option<String> {
        if archived {
            argo_uid.map(|argo_uid| {
                get_logs_archived_url(&self.url, &self.namespace, argo_uid, workflow_name)
            })
        } else {
            Some(get_logs_running_url(
                &self.url,
                &self.namespace,
                workflow_name,
            ))
        }
    }

    pub async fn get_workflow_status(&self) -> Result<SimpleStatusList> {
        let response = self
            .client
            .get(get_status_url_bakta(&self.url, &self.namespace))
            .header("Authorization", format!("Bearer {}", &self.token))
            .send()
            .await?
            .error_for_status()?
            .json::<SimpleStatusList>()
            .await?;
        Ok(response)
    }

    pub async fn delete_workflow(&self, state: &FullJobState) -> Result<()> {
        let url = if state.archived {
            if let Some(argo_uid) = &state.argo_uid {
                get_delete_url_archived(&self.url, argo_uid)
            } else {
                return Ok(());
            }
        } else if let Some(wf_name) = &state.workflowname {
            get_delete_url_running(&self.url, &self.namespace, wf_name)
        } else {
            return Ok(());
        };

        self.client
            .delete(url)
            .header("Authorization", format!("Bearer {}", &self.token))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        Ok(())
    }

    pub async fn get_logs(&self, state: &FullJobState) -> Result<String> {
        if let Some(ArgoStatus::Error) = state.status {
            return Ok(
                "Internal server error, please contact the administrator or try again later."
                    .to_string(),
            );
        }

        let Some(workflow_name) = state.workflowname.as_deref() else {
            return Ok(String::new());
        };

        let Some(url) =
            self.workflow_logs_url(workflow_name, state.archived, state.argo_uid.as_ref())
        else {
            return Ok(String::new());
        };

        let result = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", &self.token))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        if state.archived {
            Ok(result)
        } else {
            let mut final_string = String::new();

            for content in self.parse_log_entries(result).await? {
                final_string.push_str(&content.content);
                final_string.push('\n');
            }

            Ok(final_string)
        }
    }

    pub async fn get_workflow_details(&self, workflow_name: &str) -> Result<WorkflowDetails> {
        Ok(self
            .client
            .get(get_workflow_url(&self.url, &self.namespace, workflow_name))
            .header("Authorization", format!("Bearer {}", &self.token))
            .send()
            .await?
            .error_for_status()?
            .json::<WorkflowDetails>()
            .await?)
    }

    pub async fn get_workflow_log_entries(
        &self,
        workflow_name: &str,
        archived: bool,
        argo_uid: Option<&Uuid>,
    ) -> Result<Vec<Content>> {
        let Some(url) = self.workflow_logs_url(workflow_name, archived, argo_uid) else {
            return Ok(Vec::new());
        };

        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", &self.token))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        self.parse_log_entries(response).await
    }

    pub async fn submit_from_template(
        &self,
        templatename: String,
        labels: Option<HashMap<String, String>>,
        parameters: Option<HashMap<String, String>>,
        service_account: Option<String>,
        generate_name: Option<String>,
    ) -> Result<SubmitResult> {
        let labels = labels.map(|some| {
            some.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        });

        let parameters = parameters.map(|some| {
            some.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        });

        let submit_template = SubmitWorkflowTemplate {
            namespace: self.namespace.to_string(),
            resource_kind: "WorkflowTemplate".to_string(),
            resource_name: templatename.to_string(),
            submit_options: SubmitOptions {
                labels,
                parameters,
                service_account,
                generate_name,
            },
        };

        let response = self
            .client
            .post(get_submit_url(&self.url, &self.namespace))
            .header("Authorization", format!("Bearer {}", &self.token))
            .json(&submit_template)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        Ok(serde_json::from_slice(&response)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_workflow_status() {
        let client = ArgoClient::new("foo".to_string(), "bar".to_string(), "bakta".to_string());
        let error = client
            .get_workflow_status()
            .await
            .expect_err("relative URL should fail deterministically");

        assert!(error.to_string().contains("builder error"));
    }

    #[test]
    fn test_workflow_logs_url_uses_running_and_archived_paths() {
        let client = ArgoClient::new(
            "foo".to_string(),
            "https://argo.example".to_string(),
            "bakta".to_string(),
        );
        let uid =
            Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").expect("test uid should parse");

        assert_eq!(
            client.workflow_logs_url("workflow-a", false, None),
            Some(
                "https://argo.example/api/v1/workflows/bakta/workflow-a/log?logOptions.container=main"
                    .to_string()
            )
        );
        assert_eq!(
            client.workflow_logs_url("workflow-a", true, Some(&uid)),
            Some(format!(
                "https://argo.example/artifact-files/bakta/archived-workflows/{uid}/workflow-a/outputs/main-logs"
            ))
        );
        assert_eq!(client.workflow_logs_url("workflow-a", true, None), None);
    }
}

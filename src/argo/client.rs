use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;

use crate::bakta_handler::FullJobState;

use super::{
    structs::{LogResult, SimpleStatusList, SubmitOptions, SubmitResult, SubmitWorkflowTemplate},
    urls::{
        get_delete_url_archived, get_delete_url_running, get_logs_archived_url,
        get_logs_running_url, get_status_url_bakta, get_submit_url,
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
    pub async fn get_workflow_status(&self) -> Result<SimpleStatusList> {
        let response = self
            .client
            .get(get_status_url_bakta(&self.url, &self.namespace))
            .header("Authorization", &self.token)
            .send()
            .await?
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
            .header("Authorization", &self.token)
            .send()
            .await?
            .bytes()
            .await?;
        Ok(())
    }

    pub async fn get_logs(&self, state: &FullJobState) -> Result<String> {
        if state.archived {
            if let Some(argo_uid) = &state.argo_uid {
                let wfname = state.workflowname.clone().unwrap_or("".to_string());
                let url = get_logs_archived_url(&self.url, &self.namespace, argo_uid, wfname);
                Ok(self
                    .client
                    .get(url)
                    .header("Authorization", &self.token)
                    .send()
                    .await?
                    .text()
                    .await?)
            } else {
                Ok(String::new())
            }
        } else if let Some(wf_name) = &state.workflowname {
            let url = get_logs_running_url(&self.url, &self.namespace, wf_name);

            let result = self
                .client
                .get(url)
                .header("Authorization", &self.token)
                .send()
                .await?
                .text()
                .await?;

            let mut final_string = String::new();

            for line in result.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let content: LogResult = serde_json::from_str(line)?;
                if !content.result.content.contains("argo=true") {
                    final_string.push_str(&content.result.content);
                    final_string.push('\n');
                }
            }
            Ok(final_string)
        } else {
            Ok(String::new())
        }
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
            .header("Authorization", &self.token)
            .json(&submit_template)
            .send()
            .await?
            .bytes()
            .await?;

        tracing::trace!("Response: {response:?}");
        Ok(serde_json::from_slice(&response)?)
    }
}

#[cfg(test)]

mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_workflow_status() {
        let client = ArgoClient::new("foo".to_string(), "bar".to_string(), "bakta".to_string());
        let response = client.get_workflow_status().await.unwrap();
        dbg!(response);
    }
}

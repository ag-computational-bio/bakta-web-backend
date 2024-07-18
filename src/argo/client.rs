use std::collections::HashMap;

use reqwest::{Client, Response};

use anyhow::Result;

use super::{
    structs::{SimpleStatusList, SubmitOptions, SubmitWorkflowTemplate},
    urls::{get_status_url, get_status_url_phunter, get_submit_url},
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
            .get(get_status_url(&self.url, &self.namespace))
            .header("Authorization", &self.token)
            .send()
            .await?
            .json::<SimpleStatusList>()
            .await?;
        Ok(response)
    }

    pub async fn get_workflow_status_small(&self) -> Result<SimpleStatusList> {
        let response = self
            .client
            .get(get_status_url_phunter(&self.url, &self.namespace))
            .header("Authorization", &self.token)
            .send()
            .await?
            .json::<SimpleStatusList>()
            .await?;
        Ok(response)
    }

    pub async fn submit_from_template(
        &self,
        templatename: String,
        name: String,
        labels: Option<HashMap<String, String>>,
        parameters: Option<HashMap<String, String>>,
        priority: Option<i64>,
        service_account: Option<String>,
    ) -> Result<Response> {
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
                generate_name: None,
                labels,
                name: Some(name),
                parameters,
                priority,
                service_account,
            },
        };

        let response = self
            .client
            .post(get_submit_url(&self.url, &self.namespace))
            .header("Authorization", &self.token)
            .json(&submit_template)
            .send()
            .await?;
        Ok(response)
    }
}

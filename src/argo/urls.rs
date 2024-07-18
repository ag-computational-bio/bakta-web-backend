use std::fmt::Display;

pub fn get_status_url<T, U>(url: T, namespace: U) -> String
where
    T: Display,
    U: Display,
{
    format!(
        "{url}/api/v1/workflows/{namespace}?fields=\
        -items.metadata.managedFields,\
         items.status.nodes,\
         items.status.storedTemplates,\
         items.status.storedWorkflowTemplateSpec,\
         items.status.artifactRepositoryRef"
    )
}

pub fn get_status_url_phunter<T, U>(url: T, namespace: U) -> String
where
    T: Display,
    U: Display,
{
    format!(
        "{url}/api/v1/workflows/{namespace}?fields=\
        items.metadata.name,\
        items.status.phase\
        &listOptions.labelSelector=accession"
    )
}

pub fn get_submit_url<T, U>(url: T, namespace: U) -> String
where
    T: Display,
    U: Display,
{
    format!("{url}/api/v1/workflows/{namespace}/submit")
}

pub fn get_logs_url<T, U, V, W>(url: T, namespace: U, workflowname: V, podname: W) -> String
where
    T: Display,
    U: Display,
    V: Display,
    W: Display,
{
    format!(
        "{url}/api/v1/workflows/{namespace}/{workflowname}/log?logOptions.container=main&podName={podname}"
    )
}

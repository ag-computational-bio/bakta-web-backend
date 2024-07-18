use std::fmt::Display;

pub fn get_status_url_bakta<T, U>(url: T, namespace: U, recent: bool) -> String
where
    T: Display,
    U: Display,
{
    if recent {
        format!("{url}/api/v1/workflows/{namespace}?fields=items.status.finishedAt,items.status.startedAt,items.metadata.name,items.status.phase,items.metadata.labels&listOptions.labelSelector=workflows.argoproj.io/workflow-archiving-status!=Archived")
    } else {
        format!("{url}/api/v1/workflows/{namespace}?fields=items.status.finishedAt,items.status.startedAt,items.metadata.name,items.status.phase,items.metadata.labels")
    }
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

pub fn get_delete_url<T, U, V>(url: T, namespace: U, workflowname: V) -> String
where
    T: Display,
    U: Display,
    V: Display,
{
    format!("{url}/api/v1/workflows/{namespace}/{workflowname}")
}

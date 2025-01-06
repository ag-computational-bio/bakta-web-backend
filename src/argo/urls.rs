use std::fmt::Display;

pub fn get_status_url_bakta<T, U>(url: T, namespace: U) -> String
where
    T: Display,
    U: Display,
{
    format!("{url}/api/v1/workflows/{namespace}?fields=items.status.finishedAt,items.status.startedAt,items.metadata.name,items.status.phase,items.metadata.labels")
}

pub fn get_submit_url<T, U>(url: T, namespace: U) -> String
where
    T: Display,
    U: Display,
{
    format!("{url}/api/v1/workflows/{namespace}/submit")
}

pub fn get_logs_archived_url<T, U, V, W>(
    url: T,
    namespace: U,
    workflowname: V,
    podname: W,
) -> String
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

pub fn get_logs_running_url<T, U, V, W>(url: T, namespace: U, workflowname: V, podname: W) -> String
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

pub fn get_delete_url_archived<T, U, V>(url: T, namespace: U, workflowname: V) -> String
where
    T: Display,
    U: Display,
    V: Display,
{
    format!("{url}/api/v1/workflows/{namespace}/{workflowname}")
}

pub fn get_delete_url_running<T, U, V>(url: T, namespace: U, workflowname: V) -> String
where
    T: Display,
    U: Display,
    V: Display,
{
    format!("{url}/api/v1/workflows/{namespace}/{workflowname}")
}

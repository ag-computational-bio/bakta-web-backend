use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{MatchedPath, State},
    http::{Request, StatusCode, header::CONTENT_TYPE},
    middleware::Next,
    response::{IntoResponse, Response},
};
use prometheus_client::{
    encoding::{EncodeLabelSet, text::encode},
    metrics::{
        counter::Counter,
        family::Family,
        gauge::Gauge,
        histogram::{Histogram, exponential_buckets},
    },
    registry::Registry,
};

use crate::bakta_handler::BaktaHandler;

const ACTIVE_WORKFLOW_KINDS: [&str; 4] = ["bakta", "bakta_proteins", "bakta_baktfold", "baktfold"];
const ACTIVE_JOB_STATUSES: [&str; 4] = ["created", "pending", "init", "running"];

fn build_job_runtime_histogram() -> Histogram {
    Histogram::new(exponential_buckets(1.0, 2.0, 16))
}

fn build_http_request_histogram() -> Histogram {
    Histogram::new(exponential_buckets(0.005, 2.0, 16))
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct SubmissionLabels {
    pub api_version: String,
    pub workflow_kind: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct WorkflowStatusLabels {
    pub workflow_kind: String,
    pub status: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct WorkflowLabels {
    pub workflow_kind: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpRequestLabels {
    pub route: String,
    pub method: String,
    pub status: String,
}

pub struct AppMetrics {
    registry: Mutex<Registry>,
    jobs_submitted_total: Family<SubmissionLabels, Counter>,
    jobs_completed_total: Family<WorkflowStatusLabels, Counter>,
    jobs_active: Family<WorkflowStatusLabels, Gauge>,
    job_runtime_seconds: Family<WorkflowLabels, Histogram>,
    http_request_duration_seconds: Family<HttpRequestLabels, Histogram>,
    argo_submit_errors_total: Family<WorkflowLabels, Counter>,
    argo_poll_errors_total: Counter,
}

impl AppMetrics {
    pub fn new() -> Self {
        let jobs_submitted_total = Family::default();
        let jobs_completed_total = Family::default();
        let jobs_active = Family::default();
        let job_runtime_seconds =
            Family::new_with_constructor(build_job_runtime_histogram as fn() -> Histogram);
        let http_request_duration_seconds =
            Family::new_with_constructor(build_http_request_histogram as fn() -> Histogram);
        let argo_submit_errors_total = Family::default();
        let argo_poll_errors_total = Counter::default();

        let mut registry = Registry::default();
        registry.register(
            "jobs_submitted_total",
            "Number of job submissions by API version and workflow kind.",
            jobs_submitted_total.clone(),
        );
        registry.register(
            "jobs_completed_total",
            "Number of jobs that reached a terminal status by workflow kind.",
            jobs_completed_total.clone(),
        );
        registry.register(
            "jobs_active",
            "Number of currently active jobs by workflow kind and status.",
            jobs_active.clone(),
        );
        registry.register(
            "job_runtime_seconds",
            "Observed runtime in seconds for jobs that reached a terminal status.",
            job_runtime_seconds.clone(),
        );
        registry.register(
            "http_request_duration_seconds",
            "HTTP request duration in seconds by route, method, and status.",
            http_request_duration_seconds.clone(),
        );
        registry.register(
            "argo_submit_errors_total",
            "Number of Argo submission failures by workflow kind.",
            argo_submit_errors_total.clone(),
        );
        registry.register(
            "argo_poll_errors_total",
            "Number of Argo workflow status polling failures.",
            argo_poll_errors_total.clone(),
        );

        Self {
            registry: Mutex::new(registry),
            jobs_submitted_total,
            jobs_completed_total,
            jobs_active,
            job_runtime_seconds,
            http_request_duration_seconds,
            argo_submit_errors_total,
            argo_poll_errors_total,
        }
    }

    pub fn record_job_submitted(&self, api_version: &str, workflow_kind: &str) {
        self.jobs_submitted_total
            .get_or_create(&SubmissionLabels {
                api_version: api_version.to_string(),
                workflow_kind: workflow_kind.to_string(),
            })
            .inc();
    }

    pub fn record_job_completed(
        &self,
        workflow_kind: &str,
        status: &str,
        runtime: Option<Duration>,
    ) {
        self.jobs_completed_total
            .get_or_create(&WorkflowStatusLabels {
                workflow_kind: workflow_kind.to_string(),
                status: status.to_string(),
            })
            .inc();

        if let Some(runtime) = runtime {
            self.job_runtime_seconds
                .get_or_create(&WorkflowLabels {
                    workflow_kind: workflow_kind.to_string(),
                })
                .observe(runtime.as_secs_f64());
        }
    }

    pub fn record_submit_error(&self, workflow_kind: &str) {
        self.argo_submit_errors_total
            .get_or_create(&WorkflowLabels {
                workflow_kind: workflow_kind.to_string(),
            })
            .inc();
    }

    pub fn record_poll_error(&self) {
        self.argo_poll_errors_total.inc();
    }

    pub fn observe_http_request(
        &self,
        route: String,
        method: String,
        status: String,
        duration: Duration,
    ) {
        self.http_request_duration_seconds
            .get_or_create(&HttpRequestLabels {
                route,
                method,
                status,
            })
            .observe(duration.as_secs_f64());
    }

    pub fn update_active_jobs(&self, snapshot: &HashMap<(String, String), i64>) {
        for workflow_kind in ACTIVE_WORKFLOW_KINDS {
            for status in ACTIVE_JOB_STATUSES {
                let value = snapshot
                    .get(&(workflow_kind.to_string(), status.to_string()))
                    .copied()
                    .unwrap_or_default();
                self.jobs_active
                    .get_or_create(&WorkflowStatusLabels {
                        workflow_kind: workflow_kind.to_string(),
                        status: status.to_string(),
                    })
                    .set(value);
            }
        }
    }

    pub fn encode(&self) -> Result<String, std::fmt::Error> {
        let mut output = String::new();
        let registry = self
            .registry
            .lock()
            .expect("metrics registry lock poisoned");
        encode(&mut output, &registry)?;
        Ok(output)
    }
}

/// Prometheus metrics endpoint for workflow and HTTP-level service telemetry.
#[utoipa::path(
    get,
    path = "/metrics",
    operation_id = "getPrometheusMetrics",
    summary = "Fetch Prometheus metrics",
    description = "Returns Prometheus-formatted counters, gauges, and histograms for job submissions, active jobs, completions, runtimes, HTTP request durations, and Argo polling or submission errors.",
    responses(
        (status = 200, body = String, content_type = "text/plain", description = "Prometheus metrics output."),
        (status = 500, body = String, description = "Metrics could not be rendered.")
    ),
    tag = "observability",
)]
pub async fn metrics(State(state): State<Arc<BaktaHandler>>) -> impl IntoResponse {
    match state.state_handler.render_metrics().await {
        Ok(output) => (
            StatusCode::OK,
            [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            output,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub async fn track_http_metrics(
    State(state): State<Arc<BaktaHandler>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let method = request.method().to_string();
    let started_at = Instant::now();

    let response = next.run(request).await;

    state.state_handler.metrics.observe_http_request(
        route,
        method,
        response.status().as_u16().to_string(),
        started_at.elapsed(),
    );

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_encode_contains_recorded_values() {
        let metrics = AppMetrics::new();
        let mut snapshot = HashMap::new();
        snapshot.insert(("bakta".to_string(), "running".to_string()), 2);

        metrics.record_job_submitted("v2", "bakta");
        metrics.record_job_completed("bakta", "succeeded", Some(Duration::from_secs(42)));
        metrics.record_submit_error("bakta");
        metrics.record_poll_error();
        metrics.observe_http_request(
            "/api/v2/job/start".to_string(),
            "POST".to_string(),
            "200".to_string(),
            Duration::from_millis(250),
        );
        metrics.update_active_jobs(&snapshot);

        let encoded = metrics.encode().expect("metrics should encode");
        assert!(encoded.contains("jobs_submitted_total"));
        assert!(encoded.contains("workflow_kind=\"bakta\""));
        assert!(encoded.contains("jobs_active"));
        assert!(encoded.contains("http_request_duration_seconds"));
    }
}

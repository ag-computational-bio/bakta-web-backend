//! Admin API: Keycloak authentication, statistics and Argo operations.

pub mod api_paths;
pub mod auth;
pub mod structs;
#[cfg(test)]
mod test_keys;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::FromRequestParts,
    http::{StatusCode, header::AUTHORIZATION, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{bakta_handler::BaktaHandler, stats::StatsDb};
use auth::{AdminUser, AuthError};
use structs::AdminErrorResponse;

/// Routes below `/api/admin`.
pub fn router() -> Router<Arc<BaktaHandler>> {
    Router::new()
        .route("/me", get(api_paths::me))
        .route("/stats/summary", get(api_paths::stats_summary))
        .route("/stats/timeseries", get(api_paths::stats_timeseries))
        .route("/stats/versions", get(api_paths::stats_versions))
        .route("/jobs", get(api_paths::list_jobs))
        .route("/jobs/{id}", get(api_paths::get_job))
        .route("/jobs/{id}/logs", get(api_paths::job_logs))
}

#[derive(Debug)]
pub enum AdminError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden,
    NotFound(String),
    // Used by the workflow operations, added in the next step.
    #[allow(dead_code)]
    Conflict(String),
    Unavailable(String),
    BadGateway(String),
}

impl From<AuthError> for AdminError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Unauthorized(message) => AdminError::Unauthorized(message),
            AuthError::Forbidden => AdminError::Forbidden,
            AuthError::Unavailable(message) => AdminError::Unavailable(message),
        }
    }
}

impl AdminError {
    /// Maps a statistics database error. The details are only logged.
    pub fn database(error: anyhow::Error) -> Self {
        tracing::warn!(%error, "admin_stats_query_failed");
        AdminError::Unavailable("Statistics database error".to_string())
    }

    // Used by the workflow operations, added in the next step.
    #[allow(dead_code)]
    /// Maps an Argo API error to 404, 409 or 502.
    pub fn argo(error: anyhow::Error) -> Self {
        let status = error
            .downcast_ref::<reqwest::Error>()
            .and_then(reqwest::Error::status);
        match status {
            Some(reqwest::StatusCode::NOT_FOUND) => {
                AdminError::NotFound("Workflow not found".to_string())
            }
            Some(reqwest::StatusCode::CONFLICT) => {
                AdminError::Conflict(format!("Argo rejected the request: {error}"))
            }
            _ => {
                tracing::warn!(%error, "admin_argo_request_failed");
                AdminError::BadGateway(format!("Argo request failed: {error}"))
            }
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            AdminError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            AdminError::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
            AdminError::Forbidden => (
                StatusCode::FORBIDDEN,
                "Missing admin group membership".to_string(),
            ),
            AdminError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            AdminError::Conflict(message) => (StatusCode::CONFLICT, message),
            AdminError::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            AdminError::BadGateway(message) => (StatusCode::BAD_GATEWAY, message),
        };
        (status, Json(AdminErrorResponse { error })).into_response()
    }
}

/// Extractor for an authenticated admin. Answers 503 when the admin API is
/// not configured, 401 for a missing or invalid token and 403 without the
/// admin group.
pub struct Admin(pub AdminUser);

impl FromRequestParts<Arc<BaktaHandler>> for Admin {
    type Rejection = AdminError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<BaktaHandler>,
    ) -> Result<Self, Self::Rejection> {
        let Some(auth) = &state.admin_auth else {
            return Err(AdminError::Unavailable(
                "Admin API is not configured".to_string(),
            ));
        };
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok());
        Ok(Admin(auth.authenticate(header).await?))
    }
}

/// The statistics database, or 503 when it is not configured.
pub fn stats_db(state: &BaktaHandler) -> Result<&StatsDb, AdminError> {
    state
        .state_handler
        .stats
        .db()
        .ok_or_else(|| AdminError::Unavailable("Statistics database is not configured".to_string()))
}

/// Parses the `environment` query value. `all` means no filter.
pub fn parse_environment(value: Option<&str>, default: &str) -> Result<Option<String>, AdminError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    match value.unwrap_or(default) {
        "all" => Ok(None),
        environment if is_valid_environment(environment) => Ok(Some(environment.to_string())),
        environment => Err(AdminError::BadRequest(format!(
            "Invalid environment: {environment}"
        ))),
    }
}

pub fn is_valid_environment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

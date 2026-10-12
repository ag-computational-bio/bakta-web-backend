//! Request extractors that answer rejections with the admin JSON error body
//! (`{ "error": "..." }`) instead of plain text.

use axum::{
    extract::{FromRequest, FromRequestParts, Request},
    http::request::Parts,
    response::IntoResponse,
};
use serde::de::DeserializeOwned;

use super::AdminError;

fn rejected(rejection: impl IntoResponse + std::fmt::Display) -> AdminError {
    let message = rejection.to_string();
    AdminError::Rejected(rejection.into_response().status(), message)
}

/// Path parameters, like `axum::extract::Path`.
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(value)| Path(value))
            .map_err(rejected)
    }
}

/// Query parameters, like `axum::extract::Query`.
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(value)| Query(value))
            .map_err(rejected)
    }
}

/// JSON request body, like `axum::Json`.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(request, state)
            .await
            .map(|axum::Json(value)| Json(value))
            .map_err(rejected)
    }
}

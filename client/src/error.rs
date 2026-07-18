use crate::protocol::new_id;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("resource not found")]
    NotFound,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_request", message),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "resource not found".into(),
            ),
            Self::Internal(error) => {
                tracing::error!(error=?error,"local API error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal server error".into(),
                )
            }
        };
        (status,Json(json!({"error":{"code":code,"message":message,"requestId":new_id("req"),"retryable":false,"details":{}}}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(value: sqlx::Error) -> Self {
        Self::Internal(value.into())
    }
}

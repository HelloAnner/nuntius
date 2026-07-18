use crate::protocol::{ApiErrorDetail, ErrorBody, new_id};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("resource not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("target device is offline")]
    DeviceOffline,
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn internal(error: impl Into<anyhow::Error>) -> Self {
        Self::Internal(error.into())
    }

    fn parts(&self) -> (StatusCode, &'static str, bool, String) {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                false,
                message.clone(),
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                false,
                self.to_string(),
            ),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", false, self.to_string()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", false, self.to_string()),
            Self::Conflict(message) => (StatusCode::CONFLICT, "conflict", false, message.clone()),
            Self::DeviceOffline => (
                StatusCode::SERVICE_UNAVAILABLE,
                "device_offline",
                true,
                self.to_string(),
            ),
            Self::Unavailable(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                true,
                message.clone(),
            ),
            Self::Internal(error) => {
                tracing::error!(error = ?error, "internal API error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    false,
                    "internal server error".into(),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, retryable, message) = self.parts();
        let body = ErrorBody {
            error: ApiErrorDetail {
                code: code.into(),
                message,
                request_id: new_id("req"),
                retryable,
                details: Value::Null,
            },
        };
        (status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(value: sqlx::Error) -> Self {
        Self::Internal(anyhow::Error::new(value))
    }
}

pub fn json_message(message: &str) -> Json<Value> {
    Json(json!({"message": message}))
}

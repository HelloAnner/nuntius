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
    #[error("client version {client_version} does not match server version {server_version}")]
    VersionMismatch {
        client_version: String,
        server_version: String,
    },
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

    fn parts(&self) -> (StatusCode, &'static str, bool, String, Value) {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                "invalid_request",
                false,
                message.clone(),
                Value::Null,
            ),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                false,
                self.to_string(),
                Value::Null,
            ),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                false,
                self.to_string(),
                Value::Null,
            ),
            Self::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                false,
                self.to_string(),
                Value::Null,
            ),
            Self::Conflict(message) => (
                StatusCode::CONFLICT,
                "conflict",
                false,
                message.clone(),
                Value::Null,
            ),
            Self::VersionMismatch {
                client_version,
                server_version,
            } => (
                StatusCode::CONFLICT,
                "client_version_mismatch",
                false,
                self.to_string(),
                json!({
                    "clientVersion":client_version,
                    "serverVersion":server_version,
                }),
            ),
            Self::DeviceOffline => (
                StatusCode::SERVICE_UNAVAILABLE,
                "device_offline",
                true,
                self.to_string(),
                Value::Null,
            ),
            Self::Unavailable(message) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                true,
                message.clone(),
                Value::Null,
            ),
            Self::Internal(error) => {
                tracing::error!(error = ?error, "internal API error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    false,
                    "internal server error".into(),
                    Value::Null,
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, retryable, message, details) = self.parts();
        let body = ErrorBody {
            error: ApiErrorDetail {
                code: code.into(),
                message,
                request_id: new_id("req"),
                retryable,
                details,
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

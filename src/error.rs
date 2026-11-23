//! API errors rendered as GOWA envelopes. Codes and messages mirror GOWA
//! `pkg/error` (see docs/error-codes.md).

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

use crate::http::response::Envelope;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub results: Option<Value>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            results: None,
        }
    }

    pub fn with_results(mut self, results: impl Serialize) -> Self {
        self.results = serde_json::to_value(results).ok().filter(|v| !v.is_null());
        self
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }

    /// GOWA `ValidationError`.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", message)
    }

    /// Non-GOWA errors fall through Recovery as 500 `INTERNAL_SERVER_ERROR`.
    pub fn internal(message: impl ToString) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_SERVER_ERROR",
            message.to_string(),
        )
    }

    pub fn already_logged_in() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "ALREADY_LOGGED_IN",
            "you are already logged in.",
        )
    }

    pub fn not_connected() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_ERROR",
            "you are not connect to services server, please reconnect",
        )
    }

    pub fn not_logged_in() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_ERROR",
            "you are not logged in",
        )
    }

    pub fn session_saved() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SESSION_SAVED_ERROR",
            "your session have been saved, please wait to connect 2 second and refresh again",
        )
    }

    pub fn invalid_jid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "INVALID_JID", message)
    }

    pub fn device_not_found(device_id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "DEVICE_NOT_FOUND",
            "device not found; create a device first from /api/devices or provide a valid X-Device-Id",
        )
        .with_results(serde_json::json!({ "device_id": device_id }))
    }

    pub fn device_id_required() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "DEVICE_ID_REQUIRED",
            "device_id is required via X-Device-Id header or device_id query",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope {
            code: self.code,
            message: self.message,
            results: self.results,
        };
        (self.status, Json(body)).into_response()
    }
}

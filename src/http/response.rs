//! GOWA response envelope: `{ "code", "message", "results"? }`.
//! Mirrors `src/pkg/utils/response.go` (`results` is omitempty).

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct Envelope {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Value>,
}

pub fn success(message: impl Into<String>, results: impl Serialize) -> Response {
    let results = serde_json::to_value(results).ok().filter(|v| !v.is_null());
    (
        StatusCode::OK,
        Json(Envelope {
            code: "SUCCESS".into(),
            message: message.into(),
            results,
        }),
    )
        .into_response()
}

/// API error rendered as a GOWA envelope.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub results: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            results: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "401", "Unauthorized access")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_SERVER_ERROR",
            message,
        )
    }

    pub fn with_results(mut self, results: impl Serialize) -> Self {
        self.results = serde_json::to_value(results).ok().filter(|v| !v.is_null());
        self
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

pub async fn not_found() -> ApiError {
    ApiError::not_found("route not found")
}

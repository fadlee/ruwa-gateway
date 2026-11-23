//! GOWA response envelope: `{ "code", "message", "results"? }`.
//! Mirrors `src/pkg/utils/response.go` (`results` is omitempty).

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::Value;

pub use crate::error::ApiError;

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

pub async fn not_found() -> ApiError {
    ApiError::not_found("route not found")
}

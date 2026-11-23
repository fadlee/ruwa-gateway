use axum::{Router, response::Response, routing::get};
use serde_json::json;

use crate::http::{AppState, response};

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> Response {
    response::success("Success", json!({ "status": "ok" }))
}

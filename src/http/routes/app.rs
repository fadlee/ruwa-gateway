//! `/app/*` endpoints. Only `/app/info` exists so far; login/logout land in M1.

use axum::{Router, response::Response, routing::get};
use serde_json::json;

use crate::http::{AppState, response};

pub fn router() -> Router<AppState> {
    Router::new().route("/app/info", get(info))
}

async fn info() -> Response {
    response::success(
        "Success",
        json!({
            "name": "ruwa-gateway",
            "version": env!("CARGO_PKG_VERSION"),
        }),
    )
}

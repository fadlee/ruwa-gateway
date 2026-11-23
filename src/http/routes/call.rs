//! `/call/*` endpoints (GOWA `ui/rest/call.go`).

use axum::{Extension, Router, response::Response, routing::post};

use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, extract::Body, response},
    usecase::call::{self, RejectCallRequest},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/call/reject", post(reject))
}

async fn reject(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<RejectCallRequest>,
) -> ApiResult<Response> {
    call::reject_call(&device, req).await?;
    Ok(response::success(
        "Call rejected successfully",
        serde_json::Value::Null,
    ))
}

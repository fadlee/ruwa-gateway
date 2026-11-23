//! `/user/*` endpoints (GOWA `ui/rest/user.go`). Fase 2: check and info.

use axum::{
    Extension, Router,
    extract::{Query, State},
    response::Response,
    routing::get,
};
use serde::Deserialize;

use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, response},
    usecase::user,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/check", get(check))
        .route("/user/info", get(info))
}

#[derive(Deserialize)]
struct PhoneQuery {
    #[serde(default)]
    phone: String,
}

async fn check(
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<PhoneQuery>,
) -> ApiResult<Response> {
    let res = user::check(&device, &q.phone).await?;
    Ok(response::success("Success check user", res))
}

async fn info(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<PhoneQuery>,
) -> ApiResult<Response> {
    let res = user::info(&device, &q.phone, state.settings.account_validation).await?;
    Ok(response::success("Success get user info", res))
}

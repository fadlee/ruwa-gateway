//! `/user/*` endpoints (GOWA `ui/rest/user.go`). Fase 2: check and info.

use axum::{
    Extension, Router,
    extract::{Query, State},
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;

use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, extract::Body, response},
    usecase::{group, user},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/user/check", get(check))
        .route("/user/info", get(info))
        .route("/user/my/groups", get(my_groups))
        .route("/user/my/privacy", get(my_privacy))
        .route("/user/my/contacts", get(my_contacts))
        .route("/user/my/contacts/detailed", get(my_contacts_detailed))
        .route("/user/pushname", post(push_name))
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

async fn my_groups(Extension(Device(device)): Extension<Device>) -> ApiResult<Response> {
    let res = group::my_groups(&device).await?;
    Ok(response::success("Success get list groups", res))
}

async fn my_privacy(Extension(Device(device)): Extension<Device>) -> ApiResult<Response> {
    let res = user::my_privacy(&device).await?;
    Ok(response::success("Success get privacy", res))
}

async fn my_contacts(Extension(Device(device)): Extension<Device>) -> ApiResult<Response> {
    let res = user::my_contacts(&device).await?;
    Ok(response::success("Success get list contacts", res))
}

async fn my_contacts_detailed(Extension(Device(device)): Extension<Device>) -> ApiResult<Response> {
    let res = user::my_contacts_detailed(&device).await?;
    Ok(response::success("Success get detailed contacts", res))
}

async fn push_name(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<user::PushNameRequest>,
) -> ApiResult<Response> {
    user::change_push_name(&device, req).await?;
    Ok(response::success(
        "Success change push name",
        serde_json::Value::Null,
    ))
}

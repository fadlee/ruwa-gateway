//! `/newsletter/*` endpoints (GOWA `ui/rest/newsletter.go`).

use axum::{
    Extension, Router,
    extract::{Path, Query, State},
    response::Response,
    routing::{get, post},
};

use super::message::public_static_path;
use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, extract::Body, forwarded::PublicOrigin, response},
    usecase::newsletter::{self, IdRequest, InfoQuery, MessagesQuery},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/newsletter/follow", post(follow))
        .route("/newsletter/unfollow", post(unfollow))
        .route("/newsletter/info", get(info))
        .route("/newsletter/messages", get(messages))
        .route(
            "/newsletter/live-updates",
            get(live_list).post(live_subscribe).delete(live_unsubscribe),
        )
        .route("/newsletter/messages/{server_id}/download", get(download))
}

async fn follow(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<IdRequest>,
) -> ApiResult<Response> {
    newsletter::follow(&device, req).await?;
    Ok(response::success(
        "Success follow newsletter",
        serde_json::Value::Null,
    ))
}

async fn unfollow(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<IdRequest>,
) -> ApiResult<Response> {
    newsletter::unfollow(&device, req).await?;
    Ok(response::success(
        "Success unfollow newsletter",
        serde_json::Value::Null,
    ))
}

async fn info(
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<InfoQuery>,
) -> ApiResult<Response> {
    let res = newsletter::info(&device, q).await?;
    Ok(response::success("Success get newsletter info", res))
}

async fn messages(
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<MessagesQuery>,
) -> ApiResult<Response> {
    let res = newsletter::messages(&device, q).await?;
    Ok(response::success("Success get newsletter messages", res))
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct DownloadQuery {
    newsletter_id: String,
}

async fn download(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(server_id): Path<String>,
    Query(q): Query<DownloadQuery>,
    origin: PublicOrigin,
) -> ApiResult<Response> {
    let media_dir = state.devices.options().media_dir.clone();
    let mut res =
        newsletter::download_media(&device, &server_id, &q.newsletter_id, &media_dir).await?;
    if let Some(path) = public_static_path(&res.file_path) {
        res.file_url = origin.url(&state.settings.base_path, &path);
    }
    Ok(response::success(res.status.clone(), res))
}

async fn live_list(Extension(Device(device)): Extension<Device>) -> ApiResult<Response> {
    Ok(response::success(
        "Success list newsletter live updates",
        newsletter::live_updates_list(&device),
    ))
}

async fn live_subscribe(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<IdRequest>,
) -> ApiResult<Response> {
    let res = newsletter::live_updates_subscribe(&device, req).await?;
    Ok(response::success(
        "Success subscribe newsletter live updates",
        res,
    ))
}

async fn live_unsubscribe(
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<IdRequest>,
) -> ApiResult<Response> {
    newsletter::live_updates_unsubscribe(&device, &q.newsletter_id)?;
    Ok(response::success(
        "Success unsubscribe newsletter live updates",
        serde_json::Value::Null,
    ))
}

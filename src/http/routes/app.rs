//! `/app/*` endpoints (GOWA `ui/rest/app.go`). All are `GET`, as in GOWA.

use axum::{
    Extension, Router,
    extract::{Query, State},
    response::Response,
    routing::get,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, forwarded::PublicOrigin, response},
    usecase,
};

/// Routes that need no device (`InitRestAppInfo`).
pub fn unscoped() -> Router<AppState> {
    Router::new()
        .route("/app/info", get(info))
        .route("/app/metrics", get(metrics))
}

/// Device-scoped routes (`InitRestApp`).
pub fn scoped() -> Router<AppState> {
    Router::new()
        .route("/app/login", get(login))
        .route("/app/login-with-code", get(login_with_code))
        .route("/app/logout", get(logout))
        .route("/app/reconnect", get(reconnect))
        .route("/app/devices", get(devices))
        .route("/app/status", get(status))
}

async fn info(State(state): State<AppState>) -> Response {
    response::success(
        "Fetch app info success",
        json!({
            "version": format!("v{}", env!("CARGO_PKG_VERSION")),
            "os": state.settings.os,
            "base_path": state.settings.base_path,
            "max_file_size": crate::media::MAX_FILE_SIZE,
            "max_video_size": crate::media::MAX_VIDEO_SIZE,
            "max_image_size": crate::media::MAX_IMAGE_SIZE,
            "chatwoot_enabled": false,
            "scheduled_sends": false,
        }),
    )
}

/// Health of the instance itself (GOWA `AppMetrics`): process/container
/// memory, disk used by media and storages, and device counts.
async fn metrics(State(state): State<AppState>) -> Response {
    let options = state.devices.options();
    let storages = options
        .root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| "storages".into());
    let system = crate::sysmetrics::snapshot(
        vec![
            ("media".to_owned(), options.media_dir.clone()),
            ("storages".to_owned(), storages.clone()),
        ],
        &storages,
    )
    .await;

    let (mut total, mut logged_in, mut connected) = (0, 0, 0);
    for device in state.devices.list().await {
        total += 1;
        logged_in += usize::from(device.is_logged_in());
        connected += usize::from(device.is_connected());
    }

    response::success(
        "Fetch app metrics success",
        json!({
            "version": format!("v{}", env!("CARGO_PKG_VERSION")),
            // Only SQLite is supported (docs/PLAN.md Fase 5: Postgres).
            "session_store": "sqlite",
            "devices": { "total": total, "logged_in": logged_in, "connected": connected },
            "system": system,
        }),
    )
}

/// GOWA `fmt.Sprintf("%s://%s%s/%s", c.Scheme(), c.Host(), AppBasePath, ImagePath)`.
pub(crate) fn qr_link(origin: &PublicOrigin, state: &AppState, image_path: &str) -> String {
    origin.url(&state.settings.base_path, &format!("/{image_path}"))
}

async fn login(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    origin: PublicOrigin,
) -> ApiResult<Response> {
    let qr = usecase::app::login(&state.devices, &device).await?;
    Ok(response::success(
        "Login success",
        json!({
            "device_id": device.id(),
            "qr_link": qr_link(&origin, &state, &qr.image_path),
            "qr_duration": qr.duration,
        }),
    ))
}

#[derive(Deserialize)]
struct PhoneQuery {
    #[serde(default)]
    phone: String,
}

async fn login_with_code(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<PhoneQuery>,
) -> ApiResult<Response> {
    let code = usecase::app::login_with_code(&state.devices, &device, q.phone.trim()).await?;
    Ok(response::success(
        "Login with code success",
        json!({ "device_id": device.id(), "pair_code": code }),
    ))
}

async fn logout(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
) -> ApiResult<Response> {
    usecase::app::logout(&state.devices, &device).await?;
    Ok(response::success(
        "Success logout",
        json!({ "device_id": device.id() }),
    ))
}

async fn reconnect(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
) -> ApiResult<Response> {
    usecase::app::reconnect(&state.devices, &device).await?;
    Ok(response::success(
        "Reconnect success",
        json!({ "device_id": device.id() }),
    ))
}

async fn devices(State(state): State<AppState>) -> Response {
    response::success(
        "Fetch device success",
        usecase::app::device_list(&state.devices).await,
    )
}

async fn status(Extension(Device(device)): Extension<Device>) -> Response {
    let (connected, logged_in) = if device.has_session() {
        (device.is_connected(), device.is_logged_in())
    } else {
        (false, false)
    };
    response::success(
        "Connection status retrieved",
        json!({
            "is_connected": connected,
            "is_logged_in": logged_in,
            "device_id": device.id(),
            "jid": device.jid().unwrap_or_default(),
        }),
    )
}

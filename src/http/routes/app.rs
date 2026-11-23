//! `/app/*` endpoints (GOWA `ui/rest/app.go`). All are `GET`, as in GOWA.

use axum::{
    Extension, Router,
    extract::{Query, State},
    http::{HeaderMap, header},
    response::Response,
    routing::get,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    error::ApiResult,
    http::{AppState, device_scope::Device, response},
    usecase,
};

/// Routes that need no device (`InitRestAppInfo`).
pub fn unscoped() -> Router<AppState> {
    Router::new().route("/app/info", get(info))
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

/// `scheme://host{base}/{path}`, honouring `X-Forwarded-*` like Fiber with
/// `TrustProxy`.
pub(crate) fn public_url(headers: &HeaderMap, base: &str, path: &str) -> String {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let scheme = get("x-forwarded-proto").unwrap_or("http");
    let host = get("x-forwarded-host")
        .or_else(|| get(header::HOST.as_str()))
        .unwrap_or("localhost");
    format!("{scheme}://{host}{base}/{path}")
}

async fn login(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let qr = usecase::app::login(&state.devices, &device).await?;
    Ok(response::success(
        "Login success",
        json!({
            "device_id": device.id(),
            "qr_link": public_url(&headers, &state.settings.base_path, &qr.image_path),
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

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn public_url_prefers_forwarded_headers() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("internal:3000"));
        assert_eq!(
            public_url(&h, "", "statics/x.png"),
            "http://internal:3000/statics/x.png"
        );
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        h.insert(
            "x-forwarded-host",
            HeaderValue::from_static("wa.example.com"),
        );
        assert_eq!(
            public_url(&h, "/gowa", "statics/x.png"),
            "https://wa.example.com/gowa/statics/x.png"
        );
    }
}

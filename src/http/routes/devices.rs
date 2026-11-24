//! `/devices/*` endpoints (GOWA `ui/rest/device.go`). Not behind the device
//! middleware: the slot comes from the path.

use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::app::qr_link;
use crate::http::extract::Body;
use crate::{
    error::{ApiError, ApiResult},
    http::{AppState, forwarded::PublicOrigin, response},
    storage::registry::DeviceWebhook,
    usecase::{self, device as dev},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/devices", get(list).post(add))
        .route("/devices/{device_id}", get(info).delete(remove))
        .route("/devices/{device_id}/login", get(login))
        .route("/devices/{device_id}/login/code", post(login_code))
        .route("/devices/{device_id}/logout", post(logout))
        .route("/devices/{device_id}/reconnect", post(reconnect))
        .route("/devices/{device_id}/status", get(status))
        .route(
            "/devices/{device_id}/webhook",
            get(get_webhook).patch(update_webhook),
        )
}

fn bad_request(message: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
}

async fn views(state: &AppState) -> Vec<dev::DeviceView> {
    state
        .devices
        .list()
        .await
        .iter()
        .map(|d| dev::view(d))
        .collect()
}

async fn list(State(state): State<AppState>) -> Response {
    let devices: Vec<_> = state
        .devices
        .list()
        .await
        .iter()
        .map(|d| dev::view(d))
        .collect();
    response::success("List devices", devices)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AddRequest {
    device_id: String,
    webhook_url: String,
    webhook_secret: String,
    webhook_events: String,
    webhook_insecure_skip_verify: bool,
}

async fn add(
    State(state): State<AppState>,
    body: Result<Body<AddRequest>, ApiError>,
) -> ApiResult<Response> {
    let Body(req) = body.map_err(|_| bad_request("Invalid request body"))?;
    let device = state
        .devices
        .create(&req.device_id)
        .await
        .map_err(ApiError::internal)?;

    let has_webhook = !req.webhook_url.is_empty()
        || !req.webhook_secret.is_empty()
        || !req.webhook_events.is_empty()
        || req.webhook_insecure_skip_verify;
    if has_webhook {
        let hook = DeviceWebhook {
            webhook_url: Some(req.webhook_url.clone()),
            webhook_secret: req.webhook_secret.clone(),
            webhook_events: req.webhook_events.clone(),
            webhook_insecure_skip_verify: req.webhook_insecure_skip_verify,
        };
        device.set_webhook(hook).await.map_err(|e| {
            ApiError::internal(format!(
                "device {} created but webhook config could not be saved: {e}",
                device.id()
            ))
        })?;
    }

    let view = dev::view(&device);
    let mut result = json!({
        "id": view.id,
        "display_name": view.display_name,
        "jid": view.jid,
        "state": view.state,
        "created_at": view.created_at,
    });
    if has_webhook {
        result["webhook_url"] = req.webhook_url.into();
        result["webhook_secret"] = req.webhook_secret.into();
        result["webhook_events"] = req.webhook_events.into();
        result["webhook_insecure_skip_verify"] = req.webhook_insecure_skip_verify.into();
    }
    Ok(response::success("Device added", result))
}

async fn info(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let device = dev::get(&state.devices, id.trim()).await?;
    Ok(response::success("Device info", dev::view(&device)))
}

async fn remove(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let id = id.trim();
    dev::validate_id(id)?;
    // Schedules are keyed by the slot id, which `id` may only alias (a JID).
    let slot = state.devices.find(id).await.map(|d| d.id().to_owned());
    state.devices.remove(id).await?;
    if let Some(slot) = slot {
        state.scheduler.delete_device(&slot).await;
    }
    state.devices.hub().publish(
        "DEVICE_REMOVED",
        format!("Device {id} removed"),
        json!({ "device_id": id, "devices": views(&state).await }),
    );
    Ok(response::success("Device removed", Value::Null))
}

async fn login(
    State(state): State<AppState>,
    Path(id): Path<String>,
    origin: PublicOrigin,
) -> ApiResult<Response> {
    let id = id.trim();
    let device = dev::ensure(&state.devices, id).await?;
    let qr = usecase::app::login(&state.devices, &device).await?;
    Ok(response::success(
        "Login success",
        json!({
            "device_id": id,
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

async fn login_code(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PhoneQuery>,
) -> ApiResult<Response> {
    let id = id.trim();
    let device = dev::ensure(&state.devices, id).await?;
    let code = usecase::app::login_with_code(&state.devices, &device, q.phone.trim()).await?;
    Ok(response::success(
        "Login with code started",
        json!({ "device_id": id, "pair_code": code }),
    ))
}

async fn logout(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let id = id.trim();
    dev::validate_id(id)?;
    let device = state
        .devices
        .find(id)
        .await
        .ok_or_else(|| ApiError::internal(format!("device {id} not found")))?;
    {
        let _guard = device.login_lock.lock().await;
        device.logout().await;
    }
    state.devices.hub().publish(
        "DEVICE_LOGGED_OUT",
        format!("Device {id} logged out (slot kept)"),
        json!({ "device_id": id, "devices": views(&state).await }),
    );
    Ok(response::success("Logout requested", Value::Null))
}

async fn reconnect(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    dev::reconnect(&state.devices, id.trim()).await?;
    Ok(response::success("Reconnect requested", Value::Null))
}

async fn status(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let id = id.trim();
    let device = dev::get(&state.devices, id).await?;
    let (connected, logged_in) = if device.has_session() {
        (device.is_connected(), device.is_logged_in())
    } else {
        (false, false)
    };
    Ok(response::success(
        "Device status",
        json!({ "device_id": id, "is_connected": connected, "is_logged_in": logged_in }),
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WebhookRequest {
    webhook_url: Option<String>,
    webhook_secret: String,
    webhook_events: String,
    webhook_insecure_skip_verify: bool,
}

async fn update_webhook(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Body<WebhookRequest>, ApiError>,
) -> ApiResult<Response> {
    let id = id.trim();
    let Body(req) = body.map_err(|_| bad_request("Invalid request body"))?;
    let Some(url) = req.webhook_url else {
        return Err(bad_request("webhook_url is required"));
    };
    let device = state
        .devices
        .get(id)
        .await
        .ok_or_else(|| ApiError::not_found("device not found"))?;
    let hook = DeviceWebhook {
        webhook_url: Some(url.clone()),
        webhook_secret: req.webhook_secret.clone(),
        webhook_events: req.webhook_events.clone(),
        webhook_insecure_skip_verify: req.webhook_insecure_skip_verify,
    };
    device
        .set_webhook(hook)
        .await
        .map_err(|e| ApiError::internal(format!("failed to set device webhook config: {e}")))?;
    state.devices.hub().publish(
        "DEVICE_WEBHOOK_CONFIG_UPDATED",
        format!("Device {id} webhook config updated"),
        json!({ "device_id": id }),
    );
    Ok(response::success(
        "Device webhook updated",
        json!({
            "device_id": id,
            "webhook_url": url,
            "webhook_secret": req.webhook_secret,
            "webhook_events": req.webhook_events,
            "webhook_insecure_skip_verify": req.webhook_insecure_skip_verify,
        }),
    ))
}

async fn get_webhook(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Response> {
    let id = id.trim();
    let device = state
        .devices
        .get(id)
        .await
        .ok_or_else(|| ApiError::not_found("device not found"))?;
    let hook = device.webhook();
    Ok(response::success(
        "Device webhook retrieved",
        json!({
            "device_id": id,
            "webhook_url": hook.webhook_url.unwrap_or_default(),
            "webhook_secret": hook.webhook_secret,
            "webhook_events": hook.webhook_events,
            "webhook_insecure_skip_verify": hook.webhook_insecure_skip_verify,
        }),
    ))
}

//! `/devices/*` logic, mirroring GOWA `usecase/device.go`.

use std::sync::Arc;

use serde::Serialize;

use crate::{
    device::{DeviceInstance, DeviceManager},
    error::{ApiError, ApiResult},
    events::rfc3339,
};

/// GOWA `domains/device.Device`.
#[derive(Debug, Serialize)]
pub struct DeviceView {
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub phone_number: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub jid: String,
    pub created_at: String,
}

/// GOWA `deriveState`.
pub fn state(device: &DeviceInstance) -> &'static str {
    if device.is_logged_in() {
        "logged_in"
    } else if device.is_connected() {
        "connected"
    } else {
        "disconnected"
    }
}

pub fn view(device: &DeviceInstance) -> DeviceView {
    let jid = device.jid().unwrap_or_default();
    let phone_number = jid.split('@').next().unwrap_or_default().to_owned();
    let display_name = if jid.is_empty() {
        String::new()
    } else {
        device.display_name()
    };
    DeviceView {
        id: device.id().to_owned(),
        phone_number,
        display_name,
        state: state(device),
        jid,
        created_at: rfc3339(device.created_at()),
    }
}

/// GOWA `ValidateDeviceID`.
pub fn validate_id(id: &str) -> ApiResult<()> {
    if id.trim().is_empty() {
        return Err(ApiError::validation("device_id is required"));
    }
    Ok(())
}

/// Exact-id lookup used by the `/devices/{id}/...` routes; GOWA reports a
/// missing slot there as a plain (500) error.
pub async fn get(manager: &DeviceManager, id: &str) -> ApiResult<Arc<DeviceInstance>> {
    manager
        .get(id)
        .await
        .ok_or_else(|| ApiError::internal(format!("device {id} not found")))
}

/// GOWA `EnsureClient`: login on an unknown id creates the slot.
pub async fn ensure(manager: &DeviceManager, id: &str) -> ApiResult<Arc<DeviceInstance>> {
    if id.is_empty() {
        return Err(ApiError::internal("device id is required"));
    }
    match manager.get(id).await {
        Some(device) => Ok(device),
        None => manager.create(id).await.map_err(ApiError::internal),
    }
}

/// GOWA `ReconnectDevice` (stricter than `/app/reconnect`).
pub async fn reconnect(manager: &DeviceManager, id: &str) -> ApiResult<()> {
    let device = get(manager, id).await?;
    if !device.has_session() {
        return Err(ApiError::internal(format!(
            "device {id} is not logged in (session deleted)"
        )));
    }
    crate::usecase::app::reconnect(manager, &device).await
}

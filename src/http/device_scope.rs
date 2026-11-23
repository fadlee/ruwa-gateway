//! GOWA `DeviceMiddleware`: resolve the target slot from the `X-Device-Id`
//! header (URL-decoded), then `?device_id`, else the only slot.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::AppState;
use crate::device::DeviceInstance;

pub const DEVICE_ID_HEADER: &str = "X-Device-Id";

/// Request extension set by [`resolve`].
#[derive(Clone)]
pub struct Device(pub Arc<DeviceInstance>);

pub async fn resolve(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let mut requested = req
        .headers()
        .get(DEVICE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let v = v.trim();
            percent_encoding::percent_decode_str(v)
                .decode_utf8()
                .map(|d| d.into_owned())
                .unwrap_or_else(|_| v.to_owned())
        })
        .unwrap_or_default();
    if requested.is_empty() {
        requested = query_param(req.uri().query(), "device_id").unwrap_or_default();
    }

    match state.devices.resolve(&requested).await {
        Ok(device) => {
            req.extensions_mut().insert(Device(device));
            next.run(req).await
        }
        Err(error) => error.into_response(),
    }
}

pub fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    query?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| {
            percent_encoding::percent_decode_str(&v.replace('+', " "))
                .decode_utf8_lossy()
                .trim()
                .to_owned()
        })
}

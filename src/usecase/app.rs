//! `/app/*` logic, mirroring GOWA `usecase/app.go`.

use std::{sync::Arc, time::Duration};

use image::Luma;
use qrcode::{EcLevel, QrCode as QrMatrix};
use regex::Regex;

use crate::{
    device::{DeviceInstance, DeviceManager, PairCode},
    error::{ApiError, ApiResult},
    wa::sdk::pair_code::PairCodeOptions,
};

/// GOWA waits this long for the first QR code.
const QR_WAIT: Duration = Duration::from_secs(120);
const PAIR_CODE_WAIT: Duration = Duration::from_secs(60);
/// Served at `{base}/statics/...`; QR files live under `statics/qrcode`.
pub const QR_PATH_PREFIX: &str = "statics/qrcode";

pub struct LoginQr {
    /// Path relative to the base URL, e.g. `statics/qrcode/scan-qr-<uuid>.png`.
    pub image_path: String,
    /// GOWA reports half the code's validity window, in seconds.
    pub duration: u64,
}

pub async fn login(manager: &DeviceManager, device: &Arc<DeviceInstance>) -> ApiResult<LoginQr> {
    let _guard = device.login_lock.lock().await;
    if device.is_logged_in() {
        return Err(ApiError::already_logged_in());
    }
    // A stored identity cannot pair again (whatsmeow ErrQRStoreContainsID):
    // GOWA connects it and reports the session as saved.
    if device.has_session() {
        device
            .start(manager, None)
            .await
            .map_err(ApiError::internal)?;
        return Err(ApiError::session_saved());
    }

    let mut rx = device.qr.subscribe();
    device.qr.send_replace(None);
    rx.borrow_and_update();
    device
        .start(manager, None)
        .await
        .map_err(ApiError::internal)?;

    let qr = tokio::time::timeout(QR_WAIT, async {
        loop {
            if rx.changed().await.is_err() {
                return None;
            }
            if let Some(qr) = rx.borrow_and_update().clone() {
                return Some(qr);
            }
        }
    })
    .await
    .map_err(|_| ApiError::internal("timeout waiting for QR code"))?
    .ok_or_else(|| ApiError::internal("QR channel closed without receiving image"))?;

    let duration = qr.timeout.as_secs() / 2;
    let name = format!("scan-qr-{}.png", uuid::Uuid::new_v4());
    let file = manager.options().qr_dir.join(&name);
    write_qr_png(&qr.code, &file).map_err(ApiError::internal)?;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(duration)).await;
        let _ = tokio::fs::remove_file(file).await;
    });

    Ok(LoginQr {
        image_path: format!("{QR_PATH_PREFIX}/{name}"),
        duration,
    })
}

fn write_qr_png(code: &str, path: &std::path::Path) -> anyhow::Result<()> {
    let matrix = QrMatrix::with_error_correction_level(code.as_bytes(), EcLevel::M)?;
    let image = matrix.render::<Luma<u8>>().min_dimensions(512, 512).build();
    image.save(path)?;
    Ok(())
}

/// GOWA `ValidateLoginWithCode`.
fn validate_login_phone(phone: &str) -> ApiResult<()> {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^\+?[0-9]{1,15}$").unwrap());
    let reason = if phone.is_empty() {
        "cannot be blank"
    } else if !RE.is_match(phone) {
        "must be in a valid format"
    } else {
        return Ok(());
    };
    Err(ApiError::validation(format!(
        "phone_number({phone}): {reason}"
    )))
}

pub async fn login_with_code(
    manager: &DeviceManager,
    device: &Arc<DeviceInstance>,
    phone: &str,
) -> ApiResult<String> {
    validate_login_phone(phone)?;
    let _guard = device.login_lock.lock().await;
    if device.is_logged_in() {
        return Err(ApiError::already_logged_in());
    }

    let mut rx = device.pair.subscribe();
    device.pair.send_replace(None);
    rx.borrow_and_update();
    let options = PairCodeOptions {
        phone_number: phone.trim_start_matches('+').to_owned(),
        show_push_notification: true,
        ..Default::default()
    };
    device
        .start(manager, Some(options))
        .await
        .map_err(ApiError::internal)?;

    let outcome = tokio::time::timeout(PAIR_CODE_WAIT, async {
        loop {
            if rx.changed().await.is_err() {
                return None;
            }
            if let Some(outcome) = rx.borrow_and_update().clone() {
                return Some(outcome);
            }
        }
    })
    .await
    .map_err(|_| ApiError::internal("timeout waiting for pairing code"))?
    .ok_or_else(|| ApiError::internal("pairing code channel closed"))?;

    match outcome {
        PairCode::Code(code) => Ok(code),
        PairCode::Error(error) => Err(ApiError::internal(error)),
    }
}

pub async fn logout(manager: &DeviceManager, device: &Arc<DeviceInstance>) -> ApiResult<()> {
    {
        let _guard = device.login_lock.lock().await;
        device.logout().await;
    }
    manager.hub().publish(
        "DEVICE_LOGGED_OUT",
        format!("Device {} logged out (slot kept)", device.id()),
        serde_json::json!({ "device_id": device.id(), "devices": device_list(manager).await }),
    );
    Ok(())
}

/// GOWA `FetchDevices`: `[{name, device, jid}]` (`/app/devices`, `LIST_DEVICES`).
pub async fn device_list(manager: &DeviceManager) -> serde_json::Value {
    manager
        .list()
        .await
        .iter()
        .map(|d| {
            serde_json::json!({
                "name": d.display_name(),
                "device": d.id(),
                "jid": d.jid().unwrap_or_default(),
            })
        })
        .collect()
}

pub async fn reconnect(manager: &DeviceManager, device: &Arc<DeviceInstance>) -> ApiResult<()> {
    let _guard = device.login_lock.lock().await;
    if !device.has_session() {
        return Err(ApiError::internal(format!(
            "device {} is not logged in (session deleted)",
            device.id()
        )));
    }
    match device.client() {
        Some(client) => client.reconnect_immediately().await,
        None => device
            .start(manager, None)
            .await
            .map_err(ApiError::internal)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_phone_validation() {
        assert!(validate_login_phone("+628123").is_ok());
        assert_eq!(
            validate_login_phone("").unwrap_err().message,
            "phone_number(): cannot be blank"
        );
        assert_eq!(
            validate_login_phone("08-12").unwrap_err().message,
            "phone_number(08-12): must be in a valid format"
        );
    }

    #[test]
    fn qr_png_is_written() {
        let dir = std::env::temp_dir().join(format!("ruwa-qr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("qr.png");
        write_qr_png("2@abc,def,ghi", &file).unwrap();
        let img = image::open(&file).unwrap();
        assert!(img.width() >= 512);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

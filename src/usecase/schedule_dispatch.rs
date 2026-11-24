//! The production [`Backend`](super::schedule::Backend): sends a stored
//! occurrence through the same `send_*` functions the REST endpoints use.

use std::{sync::Arc, time::Duration};

use serde::de::DeserializeOwned;

use super::{
    schedule::{Backend, Failure, load_asset},
    send::{self, MessageRequest, PINNED, Pinned},
    send_media::{
        self as sm, AudioRequest, ContactRequest, FileRequest, ImageRequest, LinkRequest,
        LocationRequest, PollRequest, SendSettings, StickerRequest, VideoRequest,
    },
};
use crate::{
    device::{DeviceInstance, DeviceManager},
    error::{ApiError, ApiResult},
    media::Upload,
    storage::schedule::ScheduledSend,
};

/// Same budget GOWA gives one occurrence.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

pub struct DeviceBackend {
    pub devices: Arc<DeviceManager>,
    pub settings: SendSettings,
}

fn payload<T: DeserializeOwned>(job: &ScheduledSend) -> ApiResult<T> {
    serde_json::from_str(&job.payload_json)
        .map_err(|e| ApiError::validation(format!("invalid scheduled payload: {e}")))
}

fn asset(job: &ScheduledSend, field: &str) -> ApiResult<Option<Upload>> {
    load_asset(&job.assets_json, field)
        .map_err(|e| ApiError::validation(format!("invalid scheduled payload: {e}")))
}

async fn run(
    device: Arc<DeviceInstance>,
    job: ScheduledSend,
    settings: SendSettings,
) -> ApiResult<String> {
    let d = &*device;
    let res = match job.message_type.as_str() {
        "text" => {
            send::send_text(
                d,
                payload::<MessageRequest>(&job)?,
                settings.account_validation,
            )
            .await
        }
        "image" => {
            sm::send_image(
                d,
                payload::<ImageRequest>(&job)?,
                asset(&job, "image")?,
                settings,
            )
            .await
        }
        "file" => {
            sm::send_file(
                d,
                payload::<FileRequest>(&job)?,
                asset(&job, "file")?,
                settings,
            )
            .await
        }
        "video" => {
            sm::send_video(
                d,
                payload::<VideoRequest>(&job)?,
                asset(&job, "video")?,
                settings,
            )
            .await
        }
        "audio" => {
            sm::send_audio(
                d,
                payload::<AudioRequest>(&job)?,
                asset(&job, "audio")?,
                settings,
            )
            .await
        }
        "sticker" => {
            sm::send_sticker(
                d,
                payload::<StickerRequest>(&job)?,
                asset(&job, "sticker")?,
                settings,
            )
            .await
        }
        "contact" => sm::send_contact(d, payload::<ContactRequest>(&job)?, settings).await,
        "link" => sm::send_link(d, payload::<LinkRequest>(&job)?, settings).await,
        "location" => sm::send_location(d, payload::<LocationRequest>(&job)?, settings).await,
        "poll" => sm::send_poll(d, payload::<PollRequest>(&job)?, settings).await,
        other => {
            return Err(ApiError::validation(format!(
                "invalid scheduled payload: unsupported scheduled message type {other:?}"
            )));
        }
    };
    res.map(|r| r.message_id)
}

/// A validation error (or a broken stored payload) can never succeed on retry.
fn classify(error: ApiError) -> Failure {
    if error.code == "VALIDATION_ERROR" {
        Failure::Permanent(error.message)
    } else {
        Failure::Transient(error.message)
    }
}

impl Backend for DeviceBackend {
    async fn device_ready(&self, device_id: &str) -> bool {
        self.devices
            .find(device_id)
            .await
            .is_some_and(|d| d.client().is_some() && d.is_connected() && d.is_logged_in())
    }

    async fn new_message_id(&self, device_id: &str) -> String {
        if let Some(client) = self.devices.find(device_id).await.and_then(|d| d.client()) {
            return client.generate_message_id().to_string();
        }
        let mut random = [0u8; 9];
        random.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..9]);
        let hex: String = random.iter().map(|b| format!("{b:02X}")).collect();
        format!("3EB0{hex}")
    }

    async fn dispatch(&self, job: &ScheduledSend, pinned: Pinned) -> Result<String, Failure> {
        let Some(device) = self.devices.find(&job.device_id).await else {
            return Err(Failure::Transient("device not found".into()));
        };
        let (job, settings) = (job.clone(), self.settings);
        // Its own task: a panic in the send path is a retryable failure, not a
        // dead worker.
        let task = tokio::spawn(PINNED.scope(pinned, run(device, job, settings)));
        match tokio::time::timeout(SEND_TIMEOUT, task).await {
            Err(_) => Err(Failure::Transient("context deadline exceeded".into())),
            Ok(Err(join)) => Err(Failure::Transient(format!(
                "scheduled send panicked: {join}"
            ))),
            Ok(Ok(Ok(message_id))) => Ok(message_id),
            Ok(Ok(Err(error))) => Err(classify(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_validation_errors_are_permanent() {
        assert_eq!(
            classify(ApiError::validation("bad")),
            Failure::Permanent("bad".into())
        );
        assert!(matches!(
            classify(ApiError::not_connected()),
            Failure::Transient(_)
        ));
        assert!(matches!(
            classify(ApiError::internal("boom")),
            Failure::Transient(_)
        ));
    }

    #[test]
    fn a_broken_payload_is_permanent() {
        let job = ScheduledSend {
            id: "a".into(),
            device_id: "d".into(),
            message_type: "text".into(),
            payload_json: "{not json".into(),
            assets_json: String::new(),
            phone: String::new(),
            summary: String::new(),
            scheduled_at: 0,
            next_run_at: 0,
            timezone: "UTC".into(),
            recurrence: "once".into(),
            weekdays_json: String::new(),
            day_of_month: 0,
            end_at: None,
            occurrence_limit: 0,
            occurrence_count: 0,
            attempts: 0,
            status: "active".into(),
            lease_token: String::new(),
            lease_until: None,
            last_run_at: None,
            last_message_id: String::new(),
            last_error: String::new(),
            created_at: 0,
            updated_at: 0,
            pending_message_id: String::new(),
            pending_poll_secret: None,
        };
        let err = payload::<MessageRequest>(&job).unwrap_err();
        assert!(
            matches!(classify(err), Failure::Permanent(m) if m.starts_with("invalid scheduled payload"))
        );
    }
}

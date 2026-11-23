//! Device slots and their WhatsApp sessions (GOWA `DeviceManager` /
//! `DeviceInstance`).
//!
//! Layout per slot: `<root>/<slot>/session.db` (whatsapp-rust store) and
//! `<root>/<slot>/jid` — a marker holding the account JID, written once the
//! slot is paired and removed on logout. Only slots with a marker reconnect at
//! startup, so an unpaired slot never starts emitting QR codes on its own.

mod instance;
mod processor;
mod recent;

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use tokio::sync::{RwLock, mpsc};

pub use instance::{DeviceInstance, PairCode, QrCode};
pub use recent::RecentMessage;

use crate::{error::ApiError, webhook::Dispatcher};

/// Slot created when no device exists, so single-device clients work without
/// calling `/devices` first (GOWA keeps a transient default instance).
pub const DEFAULT_DEVICE_ID: &str = "default";

#[derive(Debug, Clone)]
pub struct DeviceOptions {
    pub root: PathBuf,
    pub qr_dir: PathBuf,
    /// Linked-device name shown on the phone (`APP_OS`).
    pub os: String,
}

pub struct DeviceManager {
    opts: DeviceOptions,
    devices: RwLock<BTreeMap<String, Arc<DeviceInstance>>>,
    events: mpsc::UnboundedSender<processor::Job>,
}

impl DeviceManager {
    /// Load slots from disk, reconnect paired ones, and start the event processor.
    pub async fn bootstrap(opts: DeviceOptions, webhook: Dispatcher) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&opts.root)?;
        std::fs::create_dir_all(&opts.qr_dir)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Arc::new(Self {
            opts,
            devices: RwLock::new(BTreeMap::new()),
            events: tx,
        });
        tokio::spawn(processor::run(rx, webhook));

        let mut ids: Vec<String> = std::fs::read_dir(&manager.opts.root)?
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        if ids.is_empty() {
            ids.push(DEFAULT_DEVICE_ID.to_owned());
        }
        for id in ids {
            let instance = manager.insert(&id).await;
            if instance.has_session()
                && let Err(error) = instance.start(&manager, None).await
            {
                tracing::error!(device = %id, %error, "failed to restore session");
            }
        }
        Ok(manager)
    }

    async fn insert(&self, id: &str) -> Arc<DeviceInstance> {
        let instance = Arc::new(DeviceInstance::new(id, self.opts.root.join(id)));
        self.devices
            .write()
            .await
            .insert(id.to_owned(), instance.clone());
        instance
    }

    pub fn options(&self) -> &DeviceOptions {
        &self.opts
    }

    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<processor::Job> {
        self.events.clone()
    }

    pub async fn list(&self) -> Vec<Arc<DeviceInstance>> {
        self.devices.read().await.values().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<Arc<DeviceInstance>> {
        self.devices.read().await.get(id).cloned()
    }

    async fn get_by_jid(&self, jid: &str) -> Option<Arc<DeviceInstance>> {
        let devices = self.devices.read().await;
        devices
            .values()
            .find(|d| d.jid().as_deref() == Some(jid))
            .cloned()
    }

    /// GOWA `ResolveDevice`: explicit id (or account JID), else the only slot.
    pub async fn resolve(&self, requested: &str) -> Result<Arc<DeviceInstance>, ApiError> {
        let requested = requested.trim();
        if !requested.is_empty() {
            if let Some(d) = self.get(requested).await {
                return Ok(d);
            }
            if let Some(d) = self.get_by_jid(requested).await {
                return Ok(d);
            }
            return Err(ApiError::device_not_found(requested));
        }
        let devices = self.devices.read().await;
        if devices.len() == 1 {
            return Ok(devices.values().next().cloned().expect("one device"));
        }
        Err(ApiError::device_id_required())
    }
}

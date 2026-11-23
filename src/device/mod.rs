//! Device slots and their WhatsApp sessions (GOWA `DeviceManager` /
//! `DeviceInstance`).
//!
//! Slots live in the registry (`storage::registry`); each has a session
//! directory `<root>/<storage_key>/session.db`. The storage key is the device
//! id when that is a safe file name, else a hex encoding of it, because GOWA
//! device ids are arbitrary strings. Only slots with a recorded JID reconnect
//! at startup, so an unpaired slot never starts emitting QR codes on its own.

pub mod hub;
mod ingest;
mod instance;
mod processor;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::{RwLock, mpsc};

pub use ingest::{store_own_edit, store_sent};
pub use instance::{DeviceInstance, PairCode, QrCode};

use crate::{
    error::ApiError,
    storage::{
        Db,
        chat::ChatStore,
        registry::{DeviceRecord, Registry},
    },
    webhook::Dispatcher,
};

/// Slot created when no device exists, so single-device clients work without
/// calling `/devices` first (GOWA keeps a transient default instance).
pub const DEFAULT_DEVICE_ID: &str = "default";

#[derive(Debug, Clone)]
pub struct DeviceOptions {
    pub root: PathBuf,
    pub qr_dir: PathBuf,
    /// Linked-device name shown on the phone (`APP_OS`).
    pub os: String,
    /// Where incoming media is saved (`statics/media`).
    pub media_dir: PathBuf,
    /// `WHATSAPP_AUTO_DOWNLOAD_MEDIA`.
    pub auto_download_media: bool,
}

pub struct DeviceManager {
    opts: DeviceOptions,
    registry: Registry,
    chats: ChatStore,
    devices: RwLock<BTreeMap<String, Arc<DeviceInstance>>>,
    events: mpsc::UnboundedSender<processor::Job>,
    hub: hub::Hub,
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Directory name for a device id: the id itself when it is a plain file
/// name, else `x` + hex, so ids like `../a` or `a/b` cannot escape `root`.
pub fn storage_key(device_id: &str) -> String {
    let safe = !device_id.is_empty()
        && device_id.len() <= 64
        && device_id != "."
        && device_id != ".."
        && !device_id.starts_with('x')
        && device_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if safe {
        device_id.to_owned()
    } else {
        format!("x{}", hex::encode(device_id.as_bytes()))
    }
}

impl DeviceManager {
    /// Load slots from the registry, reconnect paired ones, and start the
    /// event processor.
    pub async fn bootstrap(
        opts: DeviceOptions,
        db: Db,
        webhook: Dispatcher,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&opts.root)?;
        std::fs::create_dir_all(&opts.qr_dir)?;
        std::fs::create_dir_all(&opts.media_dir)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Arc::new(Self {
            opts,
            registry: Registry::new(db.clone()),
            chats: ChatStore::new(db),
            devices: RwLock::new(BTreeMap::new()),
            events: tx,
            hub: hub::Hub::default(),
        });
        tokio::spawn(processor::run(
            rx,
            webhook,
            processor::MediaSettings {
                dir: manager.opts.media_dir.clone(),
                auto_download: manager.opts.auto_download_media,
            },
            manager.hub.clone(),
        ));

        let mut records = manager.registry.all().await?;
        if records.is_empty() {
            let record =
                DeviceRecord::new(DEFAULT_DEVICE_ID, &storage_key(DEFAULT_DEVICE_ID), now());
            manager.registry.insert(record.clone()).await?;
            records.push(record);
        }
        for record in records {
            let instance = manager.attach(&record).await;
            if instance.has_session()
                && let Err(error) = instance.start(&manager, None).await
            {
                tracing::error!(device = %record.device_id, %error, "failed to restore session");
            }
        }
        Ok(manager)
    }

    async fn attach(&self, record: &DeviceRecord) -> Arc<DeviceInstance> {
        let dir = self.opts.root.join(&record.storage_key);
        let instance = Arc::new(DeviceInstance::new(
            record,
            dir,
            self.registry.clone(),
            self.chats.clone(),
        ));
        self.devices
            .write()
            .await
            .insert(record.device_id.clone(), instance.clone());
        instance
    }

    /// Dashboard event hub (`/ws`).
    pub fn hub(&self) -> &hub::Hub {
        &self.hub
    }

    pub fn options(&self) -> &DeviceOptions {
        &self.opts
    }

    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<processor::Job> {
        self.events.clone()
    }

    /// Slots in creation order.
    pub async fn list(&self) -> Vec<Arc<DeviceInstance>> {
        let mut list: Vec<_> = self.devices.read().await.values().cloned().collect();
        list.sort_by(|a, b| {
            a.created_at()
                .cmp(&b.created_at())
                .then_with(|| a.id().cmp(b.id()))
        });
        list
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

    /// Slot by id, falling back to the account JID.
    pub async fn find(&self, id_or_jid: &str) -> Option<Arc<DeviceInstance>> {
        match self.get(id_or_jid).await {
            Some(d) => Some(d),
            None => self.get_by_jid(id_or_jid).await,
        }
    }

    /// GOWA `ResolveDevice`: explicit id (or account JID), else the only slot.
    pub async fn resolve(&self, requested: &str) -> Result<Arc<DeviceInstance>, ApiError> {
        let requested = requested.trim();
        if !requested.is_empty() {
            return self
                .find(requested)
                .await
                .ok_or_else(|| ApiError::device_not_found(requested));
        }
        let devices = self.devices.read().await;
        if devices.len() == 1 {
            return Ok(devices.values().next().cloned().expect("one device"));
        }
        Err(ApiError::device_id_required())
    }

    /// GOWA `CreateDevice`: empty id → random UUID; duplicates are refused.
    pub async fn create(&self, requested: &str) -> anyhow::Result<Arc<DeviceInstance>> {
        let id = if requested.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            requested.to_owned()
        };
        if self.get(&id).await.is_some() {
            anyhow::bail!("device {id} already exists");
        }
        let record = DeviceRecord::new(&id, &storage_key(&id), now());
        self.registry.insert(record.clone()).await?;
        tracing::info!(device = %id, "created device slot");
        Ok(self.attach(&record).await)
    }

    /// GOWA `PurgeDevice`: best-effort remote unlink, then delete the
    /// session, the slot directory, chat storage, and the registry row.
    pub async fn remove(&self, id_or_jid: &str) -> Result<(), ApiError> {
        let device = self
            .find(id_or_jid)
            .await
            .ok_or_else(|| ApiError::not_found("device not found"))?;
        // Chats are partitioned by account JID once paired; keep that
        // partition when another slot is linked to the same number.
        let jid = device.jid();
        device.logout().await;
        let mut partitions = vec![device.id().to_owned()];
        if let Some(jid) = jid.filter(|j| j != device.id()) {
            let shared = self
                .devices
                .read()
                .await
                .values()
                .any(|d| d.id() != device.id() && d.jid().as_deref() == Some(jid.as_str()));
            if !shared {
                partitions.push(jid);
            }
        }
        for partition in partitions {
            if let Err(error) = self.chats.delete_device_data(&partition).await {
                tracing::warn!(device = %device.id(), %partition, %error, "failed to delete chat storage");
            }
        }
        let _ = tokio::fs::remove_dir_all(device.dir()).await;
        self.devices.write().await.remove(device.id());
        self.registry
            .delete(device.id())
            .await
            .map_err(|e| ApiError::internal(format!("purge device {}: {e}", device.id())))?;
        tracing::info!(device = %device.id(), "removed device slot");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_key_is_safe() {
        assert_eq!(storage_key("sales-1"), "sales-1");
        assert_eq!(storage_key("../etc"), format!("x{}", hex::encode("../etc")));
        assert_eq!(storage_key("a/b"), format!("x{}", hex::encode("a/b")));
        assert_eq!(storage_key(".."), format!("x{}", hex::encode("..")));
        assert_eq!(storage_key(" sales"), format!("x{}", hex::encode(" sales")));
        // Ids starting with `x` are encoded too, so they can't collide with encodings.
        assert_eq!(storage_key("x41"), format!("x{}", hex::encode("x41")));
    }
}

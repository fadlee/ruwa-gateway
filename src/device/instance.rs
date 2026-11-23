//! One device slot: its whatsapp-rust `Bot`, pairing state, and identity.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex, RwLock as StdRwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Mutex, watch};

use super::{
    DeviceManager,
    processor::{Job, Relay},
    recent::{RecentMessage, RecentMessages},
};
use crate::storage::registry::{DeviceRecord, DeviceWebhook, Registry};
use crate::wa::sdk::{
    self,
    pair_code::PairCodeOptions,
    prelude::{Bot, BotHandle, Client, SqliteStore},
    wacore::store::DevicePropsOverride,
};

const SESSION_DB: &str = "session.db";
const STORE_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct QrCode {
    pub code: String,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub enum PairCode {
    Code(String),
    Error(String),
}

pub struct DeviceInstance {
    id: String,
    dir: PathBuf,
    created_at: i64,
    registry: Registry,
    webhook: StdRwLock<DeviceWebhook>,
    handle: Mutex<Option<BotHandle>>,
    client: StdRwLock<Option<Arc<Client>>>,
    jid: StdRwLock<Option<String>>,
    pub qr: watch::Sender<Option<QrCode>>,
    pub pair: watch::Sender<Option<PairCode>>,
    /// Set while this process logs the slot out, so the resulting
    /// `LoggedOut` event is not reported as a remote logout.
    pub(super) local_logout: AtomicBool,
    recent: StdMutex<RecentMessages>,
    /// Serialises login/logout/reconnect (GOWA `lockLogin`).
    pub login_lock: Mutex<()>,
}

impl DeviceInstance {
    pub(super) fn new(record: &DeviceRecord, dir: PathBuf, registry: Registry) -> Self {
        let jid = Some(record.jid.clone()).filter(|j| !j.is_empty());
        Self {
            id: record.device_id.clone(),
            dir,
            created_at: record.created_at,
            registry,
            webhook: StdRwLock::new(record.webhook()),
            handle: Mutex::new(None),
            client: StdRwLock::new(None),
            jid: StdRwLock::new(jid),
            qr: watch::channel(None).0,
            pair: watch::channel(None).0,
            local_logout: AtomicBool::new(false),
            recent: StdMutex::new(RecentMessages::default()),
            login_lock: Mutex::new(()),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Unix seconds.
    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    pub fn webhook(&self) -> DeviceWebhook {
        self.webhook.read().expect("webhook lock").clone()
    }

    pub async fn set_webhook(&self, hook: DeviceWebhook) -> anyhow::Result<()> {
        self.registry
            .set_webhook(&self.id, hook.clone(), super::now())
            .await?;
        *self.webhook.write().expect("webhook lock") = hook;
        Ok(())
    }

    /// Account JID (`628...@s.whatsapp.net`), once paired.
    pub fn jid(&self) -> Option<String> {
        self.jid.read().expect("jid lock").clone()
    }

    /// Paired on disk (may still be disconnected).
    pub fn has_session(&self) -> bool {
        self.jid().is_some()
    }

    pub fn client(&self) -> Option<Arc<Client>> {
        self.client.read().expect("client lock").clone()
    }

    pub fn is_connected(&self) -> bool {
        self.client().is_some_and(|c| c.is_socket_connected())
    }

    pub fn is_logged_in(&self) -> bool {
        self.client().is_some_and(|c| c.is_logged_in())
    }

    /// GOWA `FetchDevices` display name: push name, else phone number.
    pub fn display_name(&self) -> String {
        let push = self.client().map(|c| c.push_name()).unwrap_or_default();
        if !push.is_empty() {
            return push;
        }
        self.jid()
            .map(|j| j.split('@').next().unwrap_or_default().to_owned())
            .unwrap_or_default()
    }

    /// Record the paired account (or clear it) in memory and the registry.
    pub(super) async fn set_jid(&self, jid: Option<String>) {
        *self.jid.write().expect("jid lock") = jid.clone();
        let name = if jid.is_some() {
            self.display_name()
        } else {
            String::new()
        };
        if let Err(error) = self
            .registry
            .set_identity(
                &self.id,
                jid.as_deref().unwrap_or_default(),
                &name,
                super::now(),
            )
            .await
        {
            tracing::warn!(device = %self.id, %error, "could not persist device identity");
        }
    }

    pub fn remember(&self, id: String, message: RecentMessage) {
        self.recent.lock().expect("recent lock").insert(id, message);
    }

    pub fn recall(&self, id: &str) -> Option<RecentMessage> {
        self.recent.lock().expect("recent lock").get(id).cloned()
    }

    /// Open the store and spawn the bot. With `pair` set, the bot links by
    /// phone-number code instead of QR.
    pub async fn start(
        self: &Arc<Self>,
        manager: &DeviceManager,
        pair: Option<PairCodeOptions>,
    ) -> anyhow::Result<()> {
        self.stop().await;
        std::fs::create_dir_all(&self.dir)?;
        let db = self.dir.join(SESSION_DB);
        let store = SqliteStore::open(&db.to_string_lossy()).await?;

        let mut builder = Bot::builder()
            .with_backend(store)
            .with_device_props(DevicePropsOverride::new().with_os(manager.options().os.clone()))
            .with_event_handler(Relay::new(Arc::downgrade(self), manager.event_sender()));
        if let Some(options) = pair {
            builder = builder.with_pair_code(options);
        }
        let bot = builder.build().await?;
        let handle = bot.spawn();
        *self.client.write().expect("client lock") = Some(handle.client());
        *self.handle.lock().await = Some(handle);
        tracing::info!(device = %self.id, "bot started");
        Ok(())
    }

    /// Gracefully stop the bot, if running, and wait for the store to close.
    pub async fn stop(&self) {
        let handle = self.handle.lock().await.take();
        let client = self.client.write().expect("client lock").take();
        let Some(handle) = handle else { return };
        let release = client.as_ref().map(|c| c.store_release());
        drop(client);
        let _ = handle.shutdown().await;
        if let Some(release) = release
            && tokio::time::timeout(STORE_RELEASE_TIMEOUT, release.wait())
                .await
                .is_err()
        {
            tracing::warn!(device = %self.id, "store not released before timeout");
        }
        tracing::info!(device = %self.id, "bot stopped");
    }

    /// Stop the bot and delete the session, keeping the slot (GOWA
    /// `LogoutDeviceKeepSlot`).
    pub async fn wipe_session(&self) {
        self.stop().await;
        remove_session_files(&self.dir.join(SESSION_DB));
        self.set_jid(None).await;
        self.qr.send_replace(None);
        self.pair.send_replace(None);
    }

    /// Log out on the server, then wipe local state.
    pub async fn logout(&self) {
        self.local_logout.store(true, Ordering::SeqCst);
        if let Some(client) = self.client() {
            let _ = client.logout().await;
        }
        self.wipe_session().await;
        self.local_logout.store(false, Ordering::SeqCst);
    }

    pub(super) fn job(self: &Arc<Self>, event: Arc<sdk::prelude::Event>) -> Job {
        Job {
            device: self.clone(),
            event,
        }
    }
}

fn remove_session_files(db: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut path = db.as_os_str().to_owned();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
}

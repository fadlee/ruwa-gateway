//! Event pump: whatsapp-rust events → slot state + GOWA webhooks.

use std::{
    path::PathBuf,
    sync::{Arc, Weak, atomic::Ordering},
};

use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    instance::{DeviceInstance, PairCode, QrCode},
    recent::RecentMessage,
};
use crate::{
    events::{self, IncomingMessage, MediaRef, ReceiptView, Resolver},
    wa::{
        Jid, message as msgx,
        sdk::prelude::{Client, Event, EventHandler, EventInterest, EventKind, InboundMessage},
    },
    webhook::Dispatcher,
};

pub(crate) struct Job {
    pub device: Arc<DeviceInstance>,
    pub event: Arc<Event>,
}

/// Registered on each `Bot`; forwards the events we care about.
pub(super) struct Relay {
    device: Weak<DeviceInstance>,
    tx: mpsc::UnboundedSender<Job>,
}

impl Relay {
    pub fn new(device: Weak<DeviceInstance>, tx: mpsc::UnboundedSender<Job>) -> Self {
        Self { device, tx }
    }
}

const KINDS: &[EventKind] = &[
    EventKind::PairingQrCode,
    EventKind::PairingQrCodesExhausted,
    EventKind::PairingCode,
    EventKind::PairingCodeError,
    EventKind::PairSuccess,
    EventKind::PairError,
    EventKind::Connected,
    EventKind::Disconnected,
    EventKind::LoggedOut,
    EventKind::Messages,
    EventKind::Receipt,
    EventKind::ChatPresence,
    EventKind::TemporaryBan,
];

impl EventHandler for Relay {
    fn handle_event(&self, event: Arc<Event>) {
        if let Some(device) = self.device.upgrade() {
            let _ = self.tx.send(device.job(event));
        }
    }

    fn interest(&self) -> EventInterest {
        EventInterest::of(KINDS)
    }
}

#[derive(Debug, Clone)]
pub(super) struct MediaSettings {
    pub dir: PathBuf,
    pub auto_download: bool,
}

pub(super) async fn run(
    mut rx: mpsc::UnboundedReceiver<Job>,
    webhook: Dispatcher,
    media: MediaSettings,
) {
    while let Some(Job { device, event }) = rx.recv().await {
        handle(&device, &event, &webhook, &media).await;
    }
}

async fn handle(
    device: &Arc<DeviceInstance>,
    event: &Event,
    webhook: &Dispatcher,
    media: &MediaSettings,
) {
    match event {
        Event::PairingQrCode(qr) => {
            device.qr.send_replace(Some(QrCode {
                code: qr.code.clone(),
                timeout: qr.timeout,
            }));
        }
        Event::PairingQrCodesExhausted(_) => {
            tracing::info!(device = %device.id(), "QR codes exhausted; stopping until next login");
            let device = device.clone();
            tokio::spawn(async move {
                if !device.is_logged_in() {
                    device.stop().await;
                }
            });
        }
        Event::PairingCode(pc) => {
            device
                .pair
                .send_replace(Some(PairCode::Code(pc.code.clone())));
        }
        Event::PairingCodeError(e) => {
            device
                .pair
                .send_replace(Some(PairCode::Error(format!("{e:?}"))));
        }
        Event::PairSuccess(ok) => {
            tracing::info!(device = %device.id(), jid = %ok.id, "paired");
            device.set_jid(Some(ok.id.to_non_ad().to_string())).await;
        }
        Event::PairError(e) => {
            tracing::warn!(device = %device.id(), error = %e.error, "pairing failed");
        }
        Event::Connected(_) => {
            if let Some(pn) = device.client().and_then(|c| c.pn()) {
                let jid = pn.to_non_ad().to_string();
                if device.jid().as_deref() != Some(jid.as_str()) {
                    device.set_jid(Some(jid)).await;
                }
            }
            tracing::info!(device = %device.id(), "connected");
        }
        Event::Disconnected(d) => {
            tracing::info!(device = %device.id(), reason = ?d.reason, "disconnected");
        }
        Event::LoggedOut(out) => {
            if device.local_logout.load(Ordering::SeqCst) {
                return;
            }
            tracing::warn!(device = %device.id(), reason = ?out.reason, "logged out remotely");
            let env = events::device_envelope(
                events::DEVICE_LOGGED_OUT,
                device.id(),
                device.jid().as_deref(),
                events::logged_out_payload(out.reason.code(), out.on_connect),
                super::now(),
            );
            webhook.send(events::DEVICE_LOGGED_OUT, env, &device.webhook());
            let device = device.clone();
            tokio::spawn(async move { device.wipe_session().await });
        }
        Event::Messages(batch) => {
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone());
            for inbound in batch.messages.iter() {
                let m = incoming(inbound);
                remember(device, &resolver, &m).await;
                let (name, payload) = events::message_payload(&resolver, &m).await;
                let mut env = events::envelope(name, &device.jid().unwrap_or_default(), payload);
                add_session_id(device, &mut env);
                webhook.send(name, env, &device.webhook());
            }
        }
        Event::Receipt(r) => {
            let view = ReceiptView {
                chat: r.source.chat.clone(),
                sender: r.source.sender.clone(),
                is_from_me: r.source.is_from_me,
                message_ids: r.message_ids.iter().map(|id| id.to_string()).collect(),
                receipt_type: r.r#type.as_wire_str().to_owned(),
                timestamp: r.timestamp.as_secs(),
            };
            if !events::should_forward_receipt(&view) {
                return;
            }
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone());
            let mut env =
                events::receipt_envelope(&resolver, &view, &device.jid().unwrap_or_default()).await;
            add_session_id(device, &mut env);
            webhook.send(events::MESSAGE_ACK, env, &device.webhook());
        }
        Event::ChatPresence(cp) => {
            use crate::wa::sdk::wacore::types::presence::ChatActivity;
            let (state, media_kind) = match cp.state {
                ChatActivity::Typing => ("composing", ""),
                ChatActivity::RecordingAudio => ("composing", "audio"),
                _ => ("paused", ""),
            };
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone());
            let view = events::ChatPresenceView {
                chat: cp.source.chat.clone(),
                sender: cp.source.sender.clone(),
                is_group: cp.source.is_group,
                state,
                media: media_kind,
            };
            let mut env = events::chat_presence_envelope(
                &resolver,
                &view,
                &device.jid().unwrap_or_default(),
                super::now(),
            )
            .await;
            add_session_id(device, &mut env);
            webhook.send(events::CHAT_PRESENCE, env, &device.webhook());
        }
        Event::TemporaryBan(ban) => {
            tracing::warn!(device = %device.id(), code = ban.code.code(), "temporarily banned");
            let env = events::device_envelope(
                events::DEVICE_TEMPORARY_BAN,
                device.id(),
                device.jid().as_deref(),
                events::temporary_ban_payload(
                    ban.code.code(),
                    ban.expire.as_secs() as i64,
                    super::now(),
                ),
                super::now(),
            );
            webhook.send(events::DEVICE_TEMPORARY_BAN, env, &device.webhook());
        }
        _ => {}
    }
}

/// GOWA `addWebhookSessionID`: tag events with the slot that emitted them.
fn add_session_id(device: &DeviceInstance, env: &mut serde_json::Map<String, Value>) {
    if device.jid().is_some() {
        env.entry("session_id")
            .or_insert_with(|| device.id().into());
    }
}

fn incoming(inbound: &InboundMessage) -> IncomingMessage {
    let info = &inbound.info;
    let verified_name = info.verified_name.as_ref().and_then(|v| v.name.clone());
    IncomingMessage {
        id: info.id.to_string(),
        chat: info.source.chat.clone(),
        sender: info.source.sender.clone(),
        is_from_me: info.source.is_from_me,
        push_name: info.push_name.to_string(),
        timestamp: info.timestamp.as_secs(),
        is_business: info.verified_name.is_some(),
        verified_name,
        verified_level: info.verified_level.clone(),
        message: inbound.message.clone(),
    }
}

/// Keep text messages around so `/send/message` can quote them by id.
async fn remember(device: &DeviceInstance, r: &ClientResolver, m: &IncomingMessage) {
    let base = msgx::unwrap(&m.message);
    let Some(text) = msgx::text(base).or_else(|| msgx::caption(base)) else {
        return;
    };
    let sender = if m.sender.is_lid() {
        r.pn_for_lid(&m.sender)
            .await
            .unwrap_or_else(|| m.sender.to_non_ad())
    } else {
        m.sender.to_non_ad()
    };
    device.remember(
        m.id.clone(),
        RecentMessage {
            chat: m.chat.to_non_ad().to_string(),
            sender: sender.to_string(),
            text: text.to_owned(),
        },
    );
}

/// `Resolver` backed by a live client's LID store and identity.
pub struct ClientResolver {
    client: Arc<Client>,
    media: MediaSettings,
}

impl ClientResolver {
    pub(super) fn new(client: Arc<Client>, media: MediaSettings) -> Self {
        Self { client, media }
    }
}

impl Resolver for ClientResolver {
    async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
        let entry = self
            .client
            .get_lid_pn_entry(&lid.to_non_ad())
            .await
            .ok()
            .flatten()?;
        Some(Jid::pn(&*entry.phone_number))
    }

    /// Best effort until contacts are stored (Fase 3): for others the live
    /// push name, else the phone user part; for the account its push name.
    async fn display_name(&self, sender: &Jid, is_from_me: bool, push_name: &str) -> String {
        if is_from_me {
            let own = self.client.push_name();
            if !own.is_empty() {
                return own;
            }
        } else if !push_name.is_empty() {
            return push_name.to_owned();
        }
        let jid = if sender.is_lid() {
            self.pn_for_lid(sender)
                .await
                .unwrap_or_else(|| sender.clone())
        } else {
            sender.clone()
        };
        jid.user.to_string()
    }

    fn auto_download(&self) -> bool {
        self.media.auto_download
    }

    async fn save_media<'a>(&'a self, media: MediaRef<'a>) -> anyhow::Result<String> {
        use crate::wa::sdk::download::{DownloadLimits, Downloadable};

        let (source, mime, filename, length): (&dyn Downloadable, String, String, u64) = match media
        {
            MediaRef::Image(m) => (
                m,
                m.mimetype.clone().unwrap_or_default(),
                String::new(),
                m.file_length.unwrap_or(0),
            ),
            MediaRef::Video(m) => (
                m,
                m.mimetype.clone().unwrap_or_default(),
                String::new(),
                m.file_length.unwrap_or(0),
            ),
            MediaRef::Audio(m) => (
                m,
                m.mimetype.clone().unwrap_or_default(),
                String::new(),
                m.file_length.unwrap_or(0),
            ),
            MediaRef::Sticker(m) => (
                m,
                m.mimetype.clone().unwrap_or_default(),
                String::new(),
                m.file_length.unwrap_or(0),
            ),
            MediaRef::Document(m) => (
                m,
                m.mimetype.clone().unwrap_or_default(),
                m.file_name.clone().unwrap_or_default(),
                m.file_length.unwrap_or(0),
            ),
        };
        if length > crate::media::MAX_DOWNLOAD_SIZE {
            anyhow::bail!(
                "file size exceeds the maximum limit of {} bytes",
                crate::media::MAX_DOWNLOAD_SIZE
            );
        }
        let bytes = self
            .client
            .download(source, DownloadLimits::default())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mime = if mime.is_empty() {
            crate::media::detect_mime(&bytes, &filename)
        } else {
            mime
        };
        let name = format!(
            "{}-{}{}",
            super::now(),
            uuid::Uuid::new_v4(),
            crate::media::extension_for(&filename, &mime)
        );
        let path = self.media.dir.join(&name);
        tokio::fs::write(&path, &bytes).await?;
        Ok(path.to_string_lossy().into_owned())
    }
}

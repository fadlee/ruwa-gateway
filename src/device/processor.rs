//! Event pump: whatsapp-rust events → slot state + GOWA webhooks.

use std::{
    path::PathBuf,
    sync::{Arc, Weak, atomic::Ordering},
};

use serde_json::Value;
use tokio::sync::mpsc;

use super::{
    hub::Hub,
    ingest,
    instance::{DeviceInstance, PairCode, QrCode},
};
use crate::{
    events::{self, IncomingMessage, MediaRef, ReceiptView, Resolver},
    storage::chat::ChatStore,
    usecase::names,
    wa::{
        Jid, Server,
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
    EventKind::StreamReplaced,
    EventKind::HistorySync,
    EventKind::ArchiveUpdate,
    EventKind::ContactUpdate,
    EventKind::GroupUpdate,
    EventKind::IncomingCall,
    EventKind::Presence,
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
    pub auto_reject_call: bool,
    /// `WHATSAPP_AUTO_MARK_READ`.
    pub auto_mark_read: bool,
    /// `WHATSAPP_AUTO_REPLY` text; empty disables it.
    pub auto_reply: String,
}

pub(super) async fn run(
    mut rx: mpsc::UnboundedReceiver<Job>,
    webhook: Dispatcher,
    media: MediaSettings,
    hub: Hub,
) {
    while let Some(Job { device, event }) = rx.recv().await {
        handle(&device, &event, &webhook, &media, &hub).await;
    }
}

async fn handle(
    device: &Arc<DeviceInstance>,
    event_arc: &Arc<Event>,
    webhook: &Dispatcher,
    media: &MediaSettings,
    hub: &Hub,
) {
    match &**event_arc {
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
            // A fresh link gets every contact with its first sync.
            super::backfill::mark_contacts_resynced(device.dir());
            hub.publish(
                "LOGIN_SUCCESS",
                format!("Successfully pair with {}", ok.id),
                serde_json::Value::Null,
            );
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
            super::backfill::resync_contacts(device);
            super::backfill::kick_group_names(device);
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
            let code = out.reason.code();
            hub.publish(
                "DEVICE_LOGGED_OUT",
                "Device logged out (slot kept)",
                serde_json::json!({
                    "device_id": device.id(),
                    "reason": format!("{code}: {}", events::connect_failure_reason(code)),
                }),
            );
            let device = device.clone();
            tokio::spawn(async move { device.wipe_session().await });
        }
        Event::Messages(batch) => {
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client.clone(), media.clone(), device);
            let own = own_jid(&client);
            for inbound in batch.messages.iter() {
                let m = incoming(inbound);
                if m.chat.server == Server::Group {
                    super::backfill::note_group_seen(device, &m.chat);
                }
                // Store first, like GOWA, so replies to it resolve at once.
                if let Err(error) = ingest::store_incoming(
                    device.chats(),
                    &resolver,
                    &device.storage_partition(),
                    own.as_ref(),
                    &m,
                )
                .await
                {
                    tracing::error!(device = %device.id(), id = %m.id, %error, "failed to store incoming message");
                }
                auto_mark_read(&client, media, &m).await;
                auto_reply(device, &client, media, &m).await;
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
            let resolver = ClientResolver::new(client, media.clone(), device);
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
            let resolver = ClientResolver::new(client, media.clone(), device);
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
            let payload = env.get("payload").cloned().unwrap_or_default();
            webhook.send(events::DEVICE_TEMPORARY_BAN, env, &device.webhook());
            let mut result = payload;
            result["device_id"] = device.id().into();
            hub.publish(
                "DEVICE_TEMPORARY_BAN",
                format!(
                    "{}: {}",
                    ban.code.code(),
                    events::temp_ban_reason(ban.code.code())
                ),
                result,
            );
        }
        Event::HistorySync(sync) => {
            let Some(client) = device.client() else {
                return;
            };
            // Inflate + decode off the runtime; the result is cached in the event.
            let job = event_arc.clone();
            let decoded = tokio::task::spawn_blocking(
                move || matches!(&*job, Event::HistorySync(s) if s.get().is_some()),
            )
            .await
            .unwrap_or(false);
            let Some(data) = sync.get().filter(|_| decoded) else {
                tracing::warn!(device = %device.id(), "could not decode history sync");
                return;
            };
            tracing::info!(
                device = %device.id(),
                sync_type = sync.sync_type(),
                conversations = data.conversations.len(),
                "storing history sync"
            );
            let resolver = ClientResolver::new(client.clone(), media.clone(), device);
            if let Err(error) = ingest::store_history(
                device.chats(),
                &resolver,
                &device.storage_partition(),
                own_jid(&client).as_ref(),
                data,
            )
            .await
            {
                tracing::error!(device = %device.id(), %error, "failed to store history sync");
            }
            super::backfill::kick_group_names(device);
        }
        Event::ArchiveUpdate(update) => {
            let (Some(client), Some(archived)) = (device.client(), update.action.archived) else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone(), device);
            if let Err(error) = ingest::store_archive(
                device.chats(),
                &resolver,
                &device.storage_partition(),
                &update.jid,
                archived,
            )
            .await
            {
                tracing::warn!(device = %device.id(), %error, "failed to update archive state");
            }
        }
        Event::ContactUpdate(update) => {
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone(), device);
            if let Err(error) = ingest::store_contact(
                device.chats(),
                &resolver,
                &device.storage_partition(),
                &update.jid,
                &update.action,
            )
            .await
            {
                tracing::warn!(device = %device.id(), %error, "failed to store contact");
            }
        }
        Event::Presence(update) => {
            use crate::wa::sdk::wacore::types::presence::PresenceStatus;
            let Some(client) = device.client() else {
                return;
            };
            let resolver = ClientResolver::new(client, media.clone(), device);
            let mut env = events::presence_envelope(
                &resolver,
                &update.from,
                matches!(update.status, PresenceStatus::Available),
                update.last_seen.map(|t| t.as_secs()),
                &device.jid().unwrap_or_default(),
                super::now(),
            )
            .await;
            add_session_id(device, &mut env);
            webhook.send(events::PRESENCE, env, &device.webhook());
        }
        Event::IncomingCall(call) => {
            incoming_call(device, call, webhook, media).await;
        }
        Event::GroupUpdate(update) => {
            group_update(device, update, webhook, media).await;
        }
        Event::StreamReplaced(_) => {
            tracing::warn!(device = %device.id(), "stream replaced by another client");
            hub.publish(
                "DEVICE_STREAM_REPLACED",
                "Device connection was replaced by another client using the same session",
                serde_json::json!({ "device_id": device.id() }),
            );
        }
        _ => {}
    }
}

/// `WHATSAPP_AUTO_MARK_READ` (GOWA `handleAutoMarkRead`): incoming only.
async fn auto_mark_read(client: &Client, media: &MediaSettings, m: &IncomingMessage) {
    if !media.auto_mark_read || m.is_from_me {
        return;
    }
    let sender = (m.chat.server == Server::Group).then_some(&m.sender);
    let ids = [m.id.as_str()];
    if let Err(error) = client.mark_as_read(&m.chat, sender, &ids).await {
        tracing::warn!(id = %m.id, %error, "failed to auto-mark message as read");
    }
}

/// Whether the message carries genuine typed text (not a caption), also
/// inside an edit (GOWA `handleAutoReply`).
fn has_typed_text(msg: &crate::wa::proto::Message) -> bool {
    let msg = crate::wa::message::unwrap(msg);
    let text = |m: &crate::wa::proto::Message| {
        m.conversation.as_deref().is_some_and(|t| !t.is_empty())
            || m.extended_text_message
                .as_option()
                .and_then(|e| e.text.as_deref())
                .is_some_and(|t| !t.is_empty())
    };
    text(msg)
        || msg
            .protocol_message
            .as_option()
            .and_then(|p| p.edited_message.as_option())
            .is_some_and(text)
}

/// `WHATSAPP_AUTO_REPLY` (GOWA `handleAutoReply`): plain text to 1:1 chats
/// only, never to groups, broadcasts, or our own messages.
async fn auto_reply(
    device: &Arc<DeviceInstance>,
    client: &Client,
    media: &MediaSettings,
    m: &IncomingMessage,
) {
    use crate::wa::sdk::prelude::SendRequest;

    if media.auto_reply.is_empty()
        || m.is_from_me
        || !matches!(m.chat.server, Server::Pn | Server::Lid)
        || !has_typed_text(&m.message)
    {
        return;
    }
    let mut reply = crate::wa::proto::Message::default();
    reply.conversation = Some(media.auto_reply.clone());
    let sent = match client
        .send(SendRequest::new(&m.sender.to_non_ad(), reply.clone()))
        .await
    {
        Ok(sent) => sent,
        Err(error) => {
            tracing::error!(device = %device.id(), %error, "failed to send auto-reply");
            return;
        }
    };
    let own = own_jid(client).map(|j| j.to_string()).unwrap_or_default();
    if let Err(error) = super::store_sent(
        device.chats(),
        &device.storage_partition(),
        &own,
        &m.sender.to_non_ad(),
        sent.message_id.as_ref(),
        &media.auto_reply,
        super::now(),
        &reply,
    )
    .await
    {
        tracing::warn!(%error, "failed to store auto-reply");
    }
}

/// Offer → optional auto-reject (`WHATSAPP_AUTO_REJECT_CALL`, 10 s) and the
/// `call.offer` webhook (GOWA `handleCallOffer`). Other call actions are ignored.
async fn incoming_call(
    device: &Arc<DeviceInstance>,
    call: &crate::wa::sdk::wacore::types::call::IncomingCall,
    webhook: &Dispatcher,
    media: &MediaSettings,
) {
    use crate::wa::sdk::wacore::types::call::CallAction;

    let CallAction::Offer {
        call_id,
        call_creator,
        group_jid,
        ..
    } = &call.action
    else {
        return;
    };
    let Some(client) = device.client() else {
        return;
    };
    let mut auto_rejected = false;
    if media.auto_reject_call {
        let voip = client.voip();
        let reject = voip.reject_call(call_id, &call.from, call_creator);
        match tokio::time::timeout(std::time::Duration::from_secs(10), reject).await {
            Ok(Ok(())) => auto_rejected = true,
            Ok(Err(error)) => {
                tracing::error!(device = %device.id(), %error, "failed to auto-reject call");
            }
            Err(_) => tracing::error!(device = %device.id(), "auto-reject call timed out"),
        }
    }
    let resolver = ClientResolver::new(client, media.clone(), device);
    let view = events::CallOfferView {
        call_id: call_id.clone(),
        from: call_creator.clone(),
        auto_rejected,
        platform: call.platform.clone().unwrap_or_default(),
        version: call.version.clone().unwrap_or_default(),
        group: group_jid.clone(),
    };
    let ts = call.timestamp.map_or_else(super::now, |t| t.as_secs());
    let mut env =
        events::call_offer_envelope(&resolver, &view, &device.jid().unwrap_or_default(), ts).await;
    add_session_id(device, &mut env);
    webhook.send(events::CALL_OFFER, env, &device.webhook());
}

/// `w:gp2` notifications → `group.participants` / `group.info` /
/// `group.joined` webhooks (GOWA `handleGroupInfo`, `handleJoinedGroup`) and
/// the stored chat name on a subject change.
async fn group_update(
    device: &Arc<DeviceInstance>,
    update: &crate::wa::sdk::wacore::types::events::GroupUpdate,
    webhook: &Dispatcher,
    media: &MediaSettings,
) {
    use crate::wa::sdk::wacore::stanza::groups::GroupNotificationAction as A;

    let Some(client) = device.client() else {
        return;
    };
    let resolver = ClientResolver::new(client, media.clone(), device);
    let device_id = device.jid().unwrap_or_default();
    let ts = update.timestamp.as_secs();
    let group = &update.group_jid;
    let own_user = device_id.parse::<Jid>().ok().map(|j| j.user.to_string());
    let is_own = |j: &Jid| own_user.as_deref() == Some(&*j.user);

    let participants = |list: &[crate::wa::sdk::wacore::stanza::groups::GroupParticipantInfo]| {
        list.iter()
            .map(|p| p.phone_number.clone().unwrap_or_else(|| p.jid.clone()))
            .collect::<Vec<Jid>>()
    };
    let mut sent: Vec<(&str, serde_json::Map<String, Value>)> = Vec::new();
    match &*update.action {
        A::Add {
            participants: list,
            reason,
            ..
        } => {
            let jids = participants(list);
            sent.push((
                events::GROUP_PARTICIPANTS,
                events::group_participants_envelope(
                    &resolver, group, "join", &jids, &device_id, ts,
                )
                .await,
            ));
            if let Some(me) = jids
                .iter()
                .chain(list.iter().map(|p| &p.jid))
                .find(|j| is_own(j))
            {
                let name = device
                    .chats()
                    .get_chat(&device.storage_partition(), &group.to_non_ad().to_string())
                    .await
                    .ok()
                    .flatten()
                    .map(|c| c.name)
                    .unwrap_or_default();
                let reason = match reason.as_deref() {
                    Some("invite") => "invite",
                    _ => "",
                };
                sent.push((
                    events::GROUP_JOINED,
                    events::group_joined_envelope(group, me, reason, &name, &device_id, ts),
                ));
            }
        }
        A::Remove {
            participants: list, ..
        } => sent.push((
            events::GROUP_PARTICIPANTS,
            events::group_participants_envelope(
                &resolver,
                group,
                "leave",
                &participants(list),
                &device_id,
                ts,
            )
            .await,
        )),
        A::Promote {
            participants: list, ..
        } => sent.push((
            events::GROUP_PARTICIPANTS,
            events::group_participants_envelope(
                &resolver,
                group,
                "promote",
                &participants(list),
                &device_id,
                ts,
            )
            .await,
        )),
        A::Demote {
            participants: list, ..
        } => sent.push((
            events::GROUP_PARTICIPANTS,
            events::group_participants_envelope(
                &resolver,
                group,
                "demote",
                &participants(list),
                &device_id,
                ts,
            )
            .await,
        )),
        A::Subject {
            subject,
            subject_owner,
            subject_owner_pn,
            ..
        } => {
            if let Err(error) = device
                .chats()
                .rename_chat(
                    &device.storage_partition(),
                    &group.to_non_ad().to_string(),
                    subject,
                )
                .await
            {
                tracing::warn!(device = %device.id(), %error, "failed to rename group chat");
            }
            let set_by = subject_owner
                .as_ref()
                .map(|o| (o, subject_owner_pn.as_ref()));
            sent.push((
                events::GROUP_INFO,
                events::group_info_envelope(&resolver, group, subject, set_by, &device_id, ts)
                    .await,
            ));
        }
        _ => {}
    }
    for (name, mut env) in sent {
        add_session_id(device, &mut env);
        webhook.send(name, env, &device.webhook());
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
        sender_alt: info.source.sender_alt.clone(),
        admin_revoke: info.edit
            == crate::wa::sdk::wacore::types::message::EditAttribute::AdminRevoke,
        verified_level: info.verified_level.clone(),
        message: inbound.message.clone(),
    }
}

/// The account's own JID (non-AD phone number).
fn own_jid(client: &Client) -> Option<Jid> {
    client.pn().map(|pn| pn.to_non_ad())
}

/// `Resolver` backed by a live client's LID store and identity.
pub struct ClientResolver {
    client: Arc<Client>,
    media: MediaSettings,
    chats: ChatStore,
    partition: String,
}

impl ClientResolver {
    pub(super) fn new(client: Arc<Client>, media: MediaSettings, device: &DeviceInstance) -> Self {
        Self {
            client,
            media,
            chats: device.chats().clone(),
            partition: device.storage_partition(),
        }
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

    /// GOWA `addSenderDisplayName`: contact store, live push name, user part;
    /// the account's own push name for its messages.
    async fn display_name(&self, sender: &Jid, is_from_me: bool, push_name: &str) -> String {
        let normalized = if sender.is_lid() {
            self.pn_for_lid(sender)
                .await
                .unwrap_or_else(|| sender.clone())
        } else {
            sender.clone()
        }
        .to_non_ad();
        let key = normalized.to_string();
        let contact = self
            .chats
            .contacts(&self.partition, vec![key.clone()])
            .await
            .ok()
            .and_then(|mut m| m.remove(&key));
        let account = names::Account {
            jid: own_jid(&self.client),
            name: self.client.push_name(),
        };
        names::sender_display_name(
            &key,
            Some(&normalized),
            is_from_me,
            &account,
            push_name,
            contact.as_ref(),
        )
    }

    fn auto_download(&self) -> bool {
        self.media.auto_download
    }

    async fn is_group_admin(&self, group: &Jid, user: &Jid, alt: Option<&Jid>) -> bool {
        let Ok(meta) = self.client.groups().fetch_metadata(group).await else {
            return false;
        };
        let matches = |j: &Jid| {
            let j = j.to_non_ad();
            j == user.to_non_ad() || alt.is_some_and(|a| a.to_non_ad() == j)
        };
        meta.participants.iter().any(|p| {
            p.is_admin()
                && (matches(&p.jid)
                    || p.phone_number.as_ref().is_some_and(matches)
                    || p.lid.as_ref().is_some_and(matches))
        })
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

#[cfg(test)]
mod auto_reply_tests {
    use super::*;
    use crate::wa::{message as msgx, proto, sdk::waproto::buffa::MessageField};

    #[test]
    fn typed_text_detection() {
        assert!(has_typed_text(&msgx::conversation("hi")));
        assert!(!has_typed_text(&msgx::conversation("")));
        assert!(has_typed_text(&msgx::extended_text(
            "hi",
            proto::ContextInfo::default()
        )));

        // A caption is not typed text.
        let mut image = proto::message::ImageMessage::default();
        image.caption = Some("caption".into());
        let mut msg = proto::Message::default();
        msg.image_message = MessageField::some(image);
        assert!(!has_typed_text(&msg));

        // An edit of a text message counts.
        let mut protocol = proto::message::ProtocolMessage::default();
        protocol.edited_message = MessageField::some(msgx::conversation("fixed"));
        let mut edit = proto::Message::default();
        edit.protocol_message = MessageField::some(protocol);
        assert!(has_typed_text(&edit));
    }
}

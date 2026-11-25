//! `/chats` and `/chat/{jid}/*` logic (GOWA `usecase/chat.go`,
//! `validations/chat_validation.go`).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    storage::chat::{Chat, ChatFilter, Contact, MessageFilter},
    usecase::{
        names::{self, Account},
        send::{chat_keys, must_login, resolve_recipient},
    },
    wa::{Jid, Server, sdk::prelude::Client},
};

const VALID_TIMERS: [u32; 4] = [0, 86_400, 604_800, 7_776_000];
const MAX_HISTORY_COUNT: i64 = 500;

/// GOWA `formatTimestamp`: RFC 3339 in `APP_TIMEZONE`. Unix 0 stands for
/// Go's zero time, which keeps its UTC form.
pub fn format_ts(secs: i64) -> String {
    if secs == 0 {
        return "0001-01-01T00:00:00Z".to_owned();
    }
    crate::events::app_rfc3339(secs)
}

/// A whatsmeow `time.Time` marshalled by GOWA, which pins `time.Local` to
/// UTC: RFC 3339 in UTC regardless of `APP_TIMEZONE`; Unix 0 = Go zero time.
pub fn format_go_time(secs: i64) -> String {
    if secs == 0 {
        return "0001-01-01T00:00:00Z".to_owned();
    }
    crate::events::rfc3339(secs)
}

/// ozzo-validation struct errors: `"a: msg; b: msg."`, keys sorted.
pub(crate) fn ozzo(errors: BTreeMap<&str, String>) -> ApiResult<()> {
    if errors.is_empty() {
        return Ok(());
    }
    let joined = errors
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    Err(ApiError::validation(format!("{joined}.")))
}

fn range(
    errors: &mut BTreeMap<&'static str, String>,
    field: &'static str,
    v: i64,
    min: i64,
    max: Option<i64>,
) {
    if v < min {
        errors.insert(field, format!("must be no less than {min}"));
    } else if let Some(max) = max.filter(|m| v > *m) {
        errors.insert(field, format!("must be no greater than {max}"));
    }
}

pub(crate) fn required(errors: &mut BTreeMap<&'static str, String>, field: &'static str, v: &str) {
    if v.trim().is_empty() {
        errors.insert(field, "cannot be blank".into());
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatInfo {
    pub jid: String,
    pub name: String,
    pub last_message_time: String,
    pub ephemeral_expiration: u32,
    pub created_at: String,
    pub updated_at: String,
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReactionInfo {
    pub emoji: String,
    pub sender_jid: String,
    pub sender_display_name: String,
    pub is_from_me: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageInfo {
    pub id: String,
    pub chat_jid: String,
    pub sender_jid: String,
    pub sender_display_name: String,
    pub content: String,
    pub timestamp: String,
    pub is_from_me: bool,
    pub media_type: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<ReactionInfo>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub call_metadata: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub context_metadata: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub referral_metadata: String,
    pub filename: String,
    pub url: String,
    pub file_length: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pagination {
    pub limit: i64,
    pub offset: i64,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct ListChatsResponse {
    pub data: Vec<ChatInfo>,
    pub pagination: Pagination,
}

#[derive(Debug, Serialize)]
pub struct ChatMessagesResponse {
    pub data: Vec<MessageInfo>,
    pub pagination: Pagination,
    pub chat_info: ChatInfo,
}

#[derive(Debug, Default)]
pub struct ListChatsRequest {
    pub limit: i64,
    pub offset: i64,
    pub search: String,
    pub has_media: bool,
    pub archived: Option<bool>,
}

#[derive(Debug, Default)]
pub struct ChatMessagesRequest {
    pub chat_jid: String,
    pub limit: i64,
    pub offset: i64,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub media_only: bool,
    pub is_from_me: Option<bool>,
    pub search: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PinRequest {
    pub pinned: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ArchiveRequest {
    pub archived: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DisappearingRequest {
    pub timer_seconds: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct HistoryRequest {
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct PinResponse {
    pub status: String,
    pub message: String,
    pub chat_jid: String,
    pub pinned: bool,
}

#[derive(Debug, Serialize)]
pub struct ArchiveResponse {
    pub status: String,
    pub message: String,
    pub chat_jid: String,
    pub archived: bool,
}

#[derive(Debug, Serialize)]
pub struct DisappearingResponse {
    pub status: String,
    pub message: String,
    pub chat_jid: String,
    pub timer_seconds: u32,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub status: String,
    pub chat_jid: String,
    pub requested_count: i64,
    pub anchor_message_id: String,
    pub anchor_timestamp: String,
}

/// Name lookups for one response: LID → PN through the live client (when
/// connected) and contacts from storage, fetched in bulk.
struct Names {
    client: Option<std::sync::Arc<Client>>,
    partition: String,
    device: std::sync::Arc<DeviceInstance>,
    contacts: HashMap<String, Contact>,
}

impl Names {
    fn new(device: &std::sync::Arc<DeviceInstance>) -> Self {
        Self {
            client: device.client(),
            partition: device.storage_partition(),
            device: device.clone(),
            contacts: HashMap::new(),
        }
    }

    async fn normalize(&self, raw: &str) -> Option<Jid> {
        let jid = names::user_jid(raw)?;
        if jid.is_lid()
            && let Some(client) = &self.client
            && let Ok(Some(entry)) = client.get_lid_pn_entry(&jid).await
        {
            return Some(Jid::pn(&*entry.phone_number));
        }
        Some(jid)
    }

    /// Normalise `raws` and load their contacts.
    async fn load(&mut self, raws: Vec<String>) -> HashMap<String, Option<Jid>> {
        let mut out = HashMap::new();
        for raw in raws {
            if let std::collections::hash_map::Entry::Vacant(slot) = out.entry(raw) {
                let jid = self.normalize(slot.key()).await;
                slot.insert(jid);
            }
        }
        let keys: Vec<String> = out
            .values()
            .flatten()
            .map(|j| j.to_string())
            .filter(|k| !self.contacts.contains_key(k))
            .collect();
        if let Ok(found) = self.device.chats().contacts(&self.partition, keys).await {
            self.contacts.extend(found);
        }
        out
    }

    fn contact(&self, jid: Option<&Jid>) -> Option<&Contact> {
        self.contacts.get(&jid?.to_string())
    }

    fn chat_info(&self, chat: &Chat, normalized: Option<&Jid>) -> ChatInfo {
        ChatInfo {
            jid: chat.jid.clone(),
            name: names::chat_display_name(
                &chat.jid,
                normalized,
                &chat.name,
                self.contact(normalized),
            ),
            last_message_time: format_ts(chat.last_message_time),
            ephemeral_expiration: chat.ephemeral_expiration.max(0) as u32,
            created_at: format_ts(chat.created_at),
            updated_at: format_ts(chat.updated_at),
            archived: chat.archived,
        }
    }
}

/// GOWA `ListChats`.
pub async fn list_chats(
    device: &std::sync::Arc<DeviceInstance>,
    mut req: ListChatsRequest,
) -> ApiResult<ListChatsResponse> {
    if req.limit == 0 {
        req.limit = 25;
    }
    let mut errors = BTreeMap::new();
    range(&mut errors, "limit", req.limit, 1, Some(100));
    range(&mut errors, "offset", req.offset, 0, None);
    ozzo(errors)?;

    let filter = ChatFilter {
        device_id: device.storage_partition(),
        limit: req.limit,
        offset: req.offset,
        search_name: req.search,
        has_media: req.has_media,
        archived: req.archived,
    };
    let chats = device
        .chats()
        .list_chats(filter.clone())
        .await
        .map_err(ApiError::internal)?;
    let total = device
        .chats()
        .count_chats(filter)
        .await
        .unwrap_or_else(|error| {
            tracing::error!(%error, "failed to get total chat count");
            0
        });

    let mut names = Names::new(device);
    let normalized = names
        .load(chats.iter().map(|c| c.jid.clone()).collect())
        .await;
    let data = chats
        .iter()
        .map(|c| names.chat_info(c, normalized.get(&c.jid).and_then(Option::as_ref)))
        .collect();
    Ok(ListChatsResponse {
        data,
        pagination: Pagination {
            limit: req.limit,
            offset: req.offset,
            total,
        },
    })
}

fn parse_rfc3339(field: &str, value: Option<&str>) -> ApiResult<Option<i64>> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => chrono::DateTime::parse_from_rfc3339(v)
            .map(|t| Some(t.timestamp()))
            .map_err(|e| {
                ApiError::internal(format!(
                    "invalid {field} format: parsing time {v:?} as \"2006-01-02T15:04:05Z07:00\": {e}"
                ))
            }),
    }
}

/// GOWA `GetChatMessages`. `total` counts every message of the chat, not
/// only the filtered ones (GOWA `GetChatMessageCountByDevice`).
pub async fn chat_messages(
    device: &std::sync::Arc<DeviceInstance>,
    mut req: ChatMessagesRequest,
) -> ApiResult<ChatMessagesResponse> {
    if req.limit == 0 {
        req.limit = 50;
    }
    let mut errors = BTreeMap::new();
    required(&mut errors, "chat_jid", &req.chat_jid);
    range(&mut errors, "limit", req.limit, 1, Some(100));
    range(&mut errors, "offset", req.offset, 0, None);
    ozzo(errors)?;

    let partition = device.storage_partition();
    let store = device.chats();
    let mut names = Names::new(device);
    let pagination = |total| Pagination {
        limit: req.limit,
        offset: req.offset,
        total,
    };

    let Some(chat) = store
        .get_chat(&partition, &req.chat_jid)
        .await
        .map_err(ApiError::internal)?
    else {
        // Not stored yet: an empty chat, not an error (GOWA).
        let normalized = names.load(vec![req.chat_jid.clone()]).await;
        let jid = normalized.get(&req.chat_jid).and_then(Option::as_ref);
        return Ok(ChatMessagesResponse {
            data: Vec::new(),
            pagination: pagination(0),
            chat_info: ChatInfo {
                jid: req.chat_jid.clone(),
                name: names::chat_display_name(&req.chat_jid, jid, "", names.contact(jid)),
                ..Default::default()
            },
        });
    };

    let start = parse_rfc3339("start_time", req.start_time.as_deref())?;
    let end = parse_rfc3339("end_time", req.end_time.as_deref())?;
    let rows = if req.search.is_empty() {
        store
            .list_messages(MessageFilter {
                device_id: partition.clone(),
                chat_jid: req.chat_jid.clone(),
                limit: req.limit,
                offset: req.offset,
                start_time: start,
                end_time: end,
                media_only: req.media_only,
                is_from_me: req.is_from_me,
            })
            .await
    } else {
        store
            .search_messages(&partition, &req.chat_jid, &req.search, req.limit)
            .await
    }
    .map_err(ApiError::internal)?;
    let total = store
        .count_messages(MessageFilter {
            device_id: partition.clone(),
            chat_jid: req.chat_jid.clone(),
            ..Default::default()
        })
        .await
        .unwrap_or(0);

    let mut raws: Vec<String> = rows
        .iter()
        .flat_map(|m| {
            std::iter::once(m.message.sender.clone())
                .chain(m.reactions.iter().map(|r| r.reactor_jid.clone()))
        })
        .collect();
    raws.push(chat.jid.clone());
    let normalized = names.load(raws).await;
    let account = Account {
        jid: names
            .client
            .as_ref()
            .and_then(|c| c.pn())
            .map(|j| j.to_non_ad())
            .or_else(|| device.jid().and_then(|j| names::user_jid(&j))),
        name: device.display_name(),
    };
    let sender_name = |raw: &str, from_me: bool| {
        let jid = normalized.get(raw).and_then(Option::as_ref);
        names::sender_display_name(raw, jid, from_me, &account, "", names.contact(jid))
    };

    let data = rows
        .iter()
        .map(|row| {
            let m = &row.message;
            MessageInfo {
                id: m.id.clone(),
                chat_jid: m.chat_jid.clone(),
                sender_jid: m.sender.clone(),
                sender_display_name: sender_name(&m.sender, m.is_from_me),
                content: m.content.clone(),
                timestamp: format_ts(m.timestamp),
                is_from_me: m.is_from_me,
                media_type: m.media_type.clone(),
                reactions: row
                    .reactions
                    .iter()
                    .map(|r| ReactionInfo {
                        emoji: r.emoji.clone(),
                        sender_jid: r.reactor_jid.clone(),
                        sender_display_name: sender_name(&r.reactor_jid, r.is_from_me),
                        is_from_me: r.is_from_me,
                        timestamp: format_ts(r.reaction_timestamp),
                    })
                    .collect(),
                call_metadata: m.call_metadata.clone(),
                context_metadata: m.context_metadata.clone(),
                referral_metadata: m.referral_metadata.clone(),
                filename: m.filename.clone(),
                url: m.url.clone(),
                file_length: m.file_length.max(0) as u64,
                created_at: format_ts(m.created_at),
                updated_at: format_ts(m.updated_at),
            }
        })
        .collect();
    let chat_info = names.chat_info(&chat, normalized.get(&chat.jid).and_then(Option::as_ref));
    Ok(ChatMessagesResponse {
        data,
        pagination: pagination(total),
        chat_info,
    })
}

fn require_jid(chat_jid: &str) -> ApiResult<()> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "chat_jid", chat_jid);
    ozzo(errors)
}

/// GOWA `PinChat` (app-state pin mutation).
pub async fn pin(
    device: &DeviceInstance,
    chat_jid: &str,
    req: PinRequest,
    account_validation: bool,
) -> ApiResult<PinResponse> {
    require_jid(chat_jid)?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, chat_jid, account_validation).await?;
    let actions = client.chat_actions();
    if req.pinned {
        actions.pin_chat(&target).await
    } else {
        actions.unpin_chat(&target).await
    }
    .map_err(ApiError::internal)?;
    Ok(PinResponse {
        status: "success".into(),
        message: if req.pinned {
            "Chat pinned successfully"
        } else {
            "Chat unpinned successfully"
        }
        .into(),
        chat_jid: chat_jid.to_owned(),
        pinned: req.pinned,
    })
}

/// GOWA `ArchiveChat`: app-state archive mutation, then the stored flag.
pub async fn archive(
    device: &DeviceInstance,
    chat_jid: &str,
    req: ArchiveRequest,
    account_validation: bool,
) -> ApiResult<ArchiveResponse> {
    require_jid(chat_jid)?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, chat_jid, account_validation).await?;
    let actions = client.chat_actions();
    if req.archived {
        actions.archive_chat(&target, None).await
    } else {
        actions.unarchive_chat(&target, None).await
    }
    .map_err(ApiError::internal)?;
    if let Ok(Some(mut chat)) = device
        .chats()
        .first_chat(&device.storage_partition(), &chat_keys(chat_jid, &target))
        .await
    {
        chat.archived = req.archived;
        let _ = device.chats().store_chat(chat).await;
    }
    Ok(ArchiveResponse {
        status: "success".into(),
        message: if req.archived {
            "Chat archived successfully"
        } else {
            "Chat unarchived successfully"
        }
        .into(),
        chat_jid: chat_jid.to_owned(),
        archived: req.archived,
    })
}

/// GOWA `SetDisappearingTimer` (whatsmeow: a protocol message for 1:1
/// chats, the group ephemeral IQ for groups).
pub async fn set_disappearing(
    device: &DeviceInstance,
    chat_jid: &str,
    req: DisappearingRequest,
    account_validation: bool,
) -> ApiResult<DisappearingResponse> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "chat_jid", chat_jid);
    if !VALID_TIMERS.contains(&req.timer_seconds) {
        errors.insert(
            "timer_seconds",
            "timer_seconds must be one of: 0 (off), 86400 (24h), 604800 (7d), 7776000 (90d)".into(),
        );
    }
    ozzo(errors)?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, chat_jid, account_validation).await?;
    match target.server {
        Server::Pn | Server::Lid => client
            .set_chat_disappearing_timer(target.clone(), req.timer_seconds)
            .await
            .map(|_| ())
            .map_err(ApiError::internal)?,
        Server::Group => client
            .groups()
            .set_ephemeral(target.clone(), req.timer_seconds)
            .await
            .map_err(ApiError::internal)?,
        _ => {
            return Err(ApiError::internal(format!(
                "can't set disappearing time in a {} chat",
                target.server.as_str()
            )));
        }
    }
    if let Ok(Some(mut chat)) = device
        .chats()
        .first_chat(&device.storage_partition(), &chat_keys(chat_jid, &target))
        .await
    {
        chat.ephemeral_expiration = req.timer_seconds as i32;
        let _ = device.chats().store_chat(chat).await;
    }
    Ok(DisappearingResponse {
        status: "success".into(),
        message: if req.timer_seconds == 0 {
            "Disappearing messages disabled".into()
        } else {
            format!("Disappearing messages set to {} seconds", req.timer_seconds)
        },
        chat_jid: chat_jid.to_owned(),
        timer_seconds: req.timer_seconds,
    })
}

/// GOWA `RequestChatHistory`: ask the phone for messages older than the
/// oldest stored one; the answer arrives as an on-demand history sync.
pub async fn request_history(
    device: &DeviceInstance,
    chat_jid: &str,
    mut req: HistoryRequest,
    account_validation: bool,
) -> ApiResult<HistoryResponse> {
    if req.count == 0 {
        req.count = 50;
    }
    let mut errors = BTreeMap::new();
    required(&mut errors, "chat_jid", chat_jid);
    range(&mut errors, "count", req.count, 1, Some(MAX_HISTORY_COUNT));
    ozzo(errors)?;

    // Resolve the anchor before touching the network.
    let oldest = device
        .chats()
        .oldest_message(&device.storage_partition(), chat_jid)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::validation(
                "chat has no stored messages yet; nothing to anchor the history sync request on",
            )
        })?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, chat_jid, account_validation).await?;
    client
        .fetch_message_history(
            &target,
            &oldest.id,
            oldest.is_from_me,
            oldest.timestamp * 1000,
            req.count as i32,
        )
        .await
        .map_err(ApiError::internal)?;
    Ok(HistoryResponse {
        status: "requested".into(),
        chat_jid: chat_jid.to_owned(),
        requested_count: req.count,
        anchor_message_id: oldest.id,
        anchor_timestamp: format_ts(oldest.timestamp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ozzo_messages() {
        let mut e = BTreeMap::new();
        range(&mut e, "offset", -1, 0, None);
        range(&mut e, "limit", 101, 1, Some(100));
        assert_eq!(
            ozzo(e).unwrap_err().message,
            "limit: must be no greater than 100; offset: must be no less than 0."
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(format_ts(0), "0001-01-01T00:00:00Z");
        assert_eq!(format_ts(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}

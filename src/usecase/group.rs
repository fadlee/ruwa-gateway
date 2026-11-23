//! `/group/*` logic (GOWA `usecase/group.go`, `validations/group_validation.go`).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    media::{self, Upload},
    usecase::{
        chat::{format_ts, ozzo, required},
        names,
        send::{must_login, resolve_recipient},
    },
    wa::{
        Jid, jid,
        sdk::{
            GroupCreateOptions, GroupDescription, GroupMetadata, GroupParticipantOptions,
            GroupSubject, JoinGroupResult, PreviousDescription, prelude::Client,
        },
    },
};

const MAX_NAME_CHARS: usize = 25;

/// Fiber binds `locked=true` from JSON and `locked=1|t|true` from forms.
fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a boolean")
        }
        fn visit_bool<E>(self, v: bool) -> Result<bool, E> {
            Ok(v)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<bool, E> {
            match v {
                "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
                "0" | "f" | "F" | "FALSE" | "false" | "False" | "" => Ok(false),
                _ => Err(E::custom(format!("invalid boolean {v:?}"))),
            }
        }
    }
    d.deserialize_any(V)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct JoinRequest {
    pub link: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LeaveRequest {
    pub group_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateRequest {
    pub title: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ParticipantRequest {
    pub group_id: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NameRequest {
    pub group_id: String,
    pub name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LockedRequest {
    pub group_id: String,
    #[serde(deserialize_with = "lenient_bool")]
    pub locked: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AnnounceRequest {
    pub group_id: String,
    #[serde(deserialize_with = "lenient_bool")]
    pub announce: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TopicRequest {
    pub group_id: String,
    pub topic: String,
}

/// `whatsmeow.ParticipantChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add,
    Remove,
    Promote,
    Demote,
}

/// `whatsmeow.ParticipantRequestChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestAction {
    Approve,
    Reject,
}

impl RequestAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ParticipantStatus {
    pub participant: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct GroupParticipant {
    pub jid: String,
    pub phone_number: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub lid: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    pub is_admin: bool,
    pub is_super_admin: bool,
}

#[derive(Debug, Serialize)]
pub struct ParticipantsResponse {
    pub group_id: String,
    pub name: String,
    pub participants: Vec<GroupParticipant>,
}

#[derive(Debug, Serialize)]
pub struct JoinRequestItem {
    pub jid: String,
    pub phone_number: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    pub requested_at: String,
}

#[derive(Debug, Serialize)]
pub struct InfoFromLink {
    pub group_id: String,
    pub name: String,
    pub topic: String,
    pub created_at: String,
    pub participant_count: usize,
    pub is_locked: bool,
    pub is_announce: bool,
    pub is_ephemeral: bool,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct InviteLink {
    pub invite_link: String,
    pub group_id: String,
}

#[derive(Debug, Serialize)]
pub struct PhotoResponse {
    pub picture_id: String,
    pub message: String,
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// `validation.Required` + `validation.Each(validation.Required)` on a slice.
fn participants_rule(errors: &mut BTreeMap<&'static str, String>, list: &[String]) {
    if list.is_empty() {
        errors.insert("participants", "cannot be blank".into());
        return;
    }
    let bad: Vec<String> = list
        .iter()
        .enumerate()
        .filter(|(_, p)| blank(p))
        .map(|(i, _)| format!("{i}: cannot be blank"))
        .collect();
    if !bad.is_empty() {
        errors.insert("participants", format!("({}.)", bad.join("; ")));
    }
}

fn group_required(group_id: &str) -> ApiResult<()> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "group_id", group_id);
    ozzo(errors)
}

fn group_and_participants(group_id: &str, list: &[String]) -> ApiResult<()> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "group_id", group_id);
    participants_rule(&mut errors, list);
    ozzo(errors)
}

/// GOWA `participantToJID`: always `@s.whatsapp.net`, always checked against
/// WhatsApp (independent of `WHATSAPP_ACCOUNT_VALIDATION`), duplicates dropped.
async fn participants_to_jids(client: &Client, raw: &[String]) -> ApiResult<Vec<Jid>> {
    let mut out: Vec<Jid> = Vec::new();
    for participant in raw {
        let not_registered = || ApiError::invalid_jid("user is not registered");
        let parsed = jid::parse_jid(&format!("{participant}@s.whatsapp.net"))
            .map_err(|_| not_registered())?;
        let found = if jid::is_pn_user(&parsed) {
            let bare = Jid::pn(parsed.user.trim_start_matches('+'));
            if bare.user.is_empty() {
                return Err(not_registered());
            }
            let results = client
                .contacts()
                .is_on_whatsapp(std::slice::from_ref(&bare))
                .await
                .map_err(|e| {
                    ApiError::internal(format!("failed to check {} on WhatsApp: {e}", bare.user))
                })?;
            results
                .into_iter()
                .find(|r| r.is_registered)
                .map(|r| r.jid.to_non_ad())
                .ok_or_else(not_registered)?
        } else {
            parsed
        };
        if !out.contains(&found) {
            out.push(found);
        }
    }
    Ok(out)
}

/// `ValidateJidWithLogin` for a group id (login is checked by the caller).
async fn group_jid(client: &Client, raw: &str, account_validation: bool) -> ApiResult<Jid> {
    resolve_recipient(client, raw, account_validation).await
}

/// GOWA `ResolveLIDToPhone`: the phone JID for a LID when known.
async fn phone_jid(client: &Client, wire: &Jid, alt: Option<&Jid>) -> Option<Jid> {
    if jid::is_pn_user(wire) {
        return Some(wire.to_non_ad());
    }
    if wire.is_lid()
        && let Ok(Some(entry)) = client.get_lid_pn_entry(&wire.to_non_ad()).await
    {
        return Some(Jid::pn(&*entry.phone_number));
    }
    alt.filter(|j| jid::is_pn_user(j)).map(Jid::to_non_ad)
}

/// Contact-book names for the given JIDs, keyed by JID string.
async fn contact_names(device: &DeviceInstance, jids: Vec<String>) -> HashMap<String, String> {
    let Ok(contacts) = device
        .chats()
        .contacts(&device.storage_partition(), jids)
        .await
    else {
        return HashMap::new();
    };
    contacts
        .into_iter()
        .filter_map(|(jid, c)| {
            let name = names::preferred_contact_name(Some(&c), "");
            (!name.is_empty()).then_some((jid, name))
        })
        .collect()
}

pub async fn join_with_link(device: &DeviceInstance, req: JoinRequest) -> ApiResult<String> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "link", &req.link);
    ozzo(errors)?;
    let client = must_login(device)?;
    let result = client
        .groups()
        .join_with_invite_code(&req.link)
        .await
        .map_err(ApiError::internal)?;
    Ok(match result {
        JoinGroupResult::Joined(j) | JoinGroupResult::PendingApproval(j) => j.to_string(),
        #[allow(unreachable_patterns)]
        other => other.group_jid().to_string(),
    })
}

pub async fn leave(
    device: &DeviceInstance,
    req: LeaveRequest,
    account_validation: bool,
) -> ApiResult<()> {
    group_required(&req.group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    client
        .groups()
        .leave(group)
        .await
        .map_err(ApiError::internal)
}

pub async fn create(device: &DeviceInstance, req: CreateRequest) -> ApiResult<String> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "title", &req.title);
    participants_rule(&mut errors, &req.participants);
    ozzo(errors)?;
    let client = must_login(device)?;
    let participants = participants_to_jids(&client, &req.participants).await?;
    let options = GroupCreateOptions::builder()
        .subject(req.title)
        .participants(
            participants
                .into_iter()
                .map(GroupParticipantOptions::new)
                .collect(),
        )
        .finish();
    let created = client
        .groups()
        .create_group(options)
        .await
        .map_err(ApiError::internal)?;
    Ok(created.metadata.id.to_string())
}

pub async fn info_from_link(device: &DeviceInstance, link: &str) -> ApiResult<InfoFromLink> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "link", link);
    ozzo(errors)?;
    let client = must_login(device)?;
    let meta = client
        .groups()
        .get_invite_info(link)
        .await
        .map_err(ApiError::internal)?;
    let topic = meta.description.clone().unwrap_or_default();
    Ok(InfoFromLink {
        group_id: meta.id.to_string(),
        name: meta.subject.clone().unwrap_or_default(),
        topic: topic.clone(),
        created_at: format_ts(meta.creation_time.unwrap_or(0) as i64),
        participant_count: if meta.participants.is_empty() {
            meta.participant_count.unwrap_or(0) as usize
        } else {
            meta.participants.len()
        },
        is_locked: meta.is_locked,
        is_announce: meta.is_announcement,
        is_ephemeral: meta
            .ephemeral
            .as_ref()
            .is_some_and(|e| e.expiration.unwrap_or(0) > 0),
        description: topic,
    })
}

pub async fn manage_participants(
    device: &DeviceInstance,
    req: ParticipantRequest,
    action: Action,
    account_validation: bool,
) -> ApiResult<Vec<ParticipantStatus>> {
    group_and_participants(&req.group_id, &req.participants)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    let participants = participants_to_jids(&client, &req.participants).await?;
    let groups = client.groups();
    let changed = match action {
        Action::Add => groups.add_participants(group, &participants).await,
        Action::Remove => groups.remove_participants(group, &participants).await,
        Action::Promote => groups.promote_participants(group, &participants).await,
        Action::Demote => groups.demote_participants(group, &participants).await,
    }
    .map_err(ApiError::internal)?;
    Ok(changed
        .into_iter()
        .map(|p| {
            let (status, message) = match (&p.error, &p.add_request) {
                // GOWA: a 403 that carries an invite token means "could not add".
                (Some(code), Some(_)) if code == "403" => {
                    ("error", "Failed to add participant".to_owned())
                }
                // GOWA reports every other outcome as success; a server error
                // code is surfaced instead of hidden.
                (Some(code), _) => ("error", format!("Action failed (code {code})")),
                (None, _) => ("success", "Action success".to_owned()),
            };
            ParticipantStatus {
                participant: p.jid.to_string(),
                status: status.into(),
                message,
            }
        })
        .collect())
}

fn participant_view(
    p: &crate::wa::sdk::GroupParticipant,
    phone: Option<&Jid>,
    names: &HashMap<String, String>,
) -> GroupParticipant {
    let lid = match (&p.lid, p.jid.is_lid()) {
        (Some(l), _) => l.to_non_ad().to_string(),
        (None, true) => p.jid.to_non_ad().to_string(),
        _ => String::new(),
    };
    let display_name = phone
        .and_then(|j| names.get(&j.to_string()))
        .or_else(|| names.get(&p.jid.to_non_ad().to_string()))
        .cloned()
        .unwrap_or_default();
    GroupParticipant {
        jid: p.jid.to_string(),
        phone_number: phone.map(|j| j.user.to_string()).unwrap_or_default(),
        lid,
        display_name,
        is_admin: p.is_admin(),
        is_super_admin: p.is_super_admin(),
    }
}

pub async fn participants(
    device: &DeviceInstance,
    group_id: &str,
    account_validation: bool,
) -> ApiResult<ParticipantsResponse> {
    group_required(group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, group_id, account_validation).await?;
    let meta = client
        .groups()
        .fetch_metadata(&group)
        .await
        .map_err(ApiError::internal)?;

    let mut phones = Vec::with_capacity(meta.participants.len());
    for p in &meta.participants {
        phones.push(phone_jid(&client, &p.jid, p.phone_number.as_ref()).await);
    }
    let lookups = meta
        .participants
        .iter()
        .zip(&phones)
        .flat_map(|(p, phone)| {
            [
                phone.as_ref().map(|j| j.to_string()),
                Some(p.jid.to_non_ad().to_string()),
            ]
        })
        .flatten()
        .collect();
    let names = contact_names(device, lookups).await;

    Ok(ParticipantsResponse {
        group_id: group.to_string(),
        name: meta.subject.clone().unwrap_or_default(),
        participants: meta
            .participants
            .iter()
            .zip(&phones)
            .map(|(p, phone)| participant_view(p, phone.as_ref(), &names))
            .collect(),
    })
}

pub async fn join_requests(
    device: &DeviceInstance,
    group_id: &str,
    account_validation: bool,
) -> ApiResult<Vec<JoinRequestItem>> {
    group_required(group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, group_id, account_validation).await?;
    let requests = client
        .groups()
        .get_membership_requests(group)
        .await
        .map_err(ApiError::internal)?;

    let mut resolved = Vec::with_capacity(requests.len());
    for r in &requests {
        resolved.push(phone_jid(&client, &r.jid, None).await);
    }
    let keys: Vec<String> = requests
        .iter()
        .zip(&resolved)
        .map(|(r, phone)| {
            phone
                .as_ref()
                .map_or_else(|| r.jid.to_non_ad().to_string(), |j| j.to_string())
        })
        .collect();
    let names = contact_names(device, keys.clone()).await;

    Ok(requests
        .iter()
        .zip(&resolved)
        .zip(&keys)
        .map(|((r, phone), key)| JoinRequestItem {
            jid: r.jid.to_string(),
            phone_number: phone
                .as_ref()
                .map(|j| j.user.to_string())
                .unwrap_or_default(),
            display_name: names.get(key).cloned().unwrap_or_default(),
            requested_at: format_ts(r.request_time.unwrap_or(0) as i64),
        })
        .collect())
}

pub async fn manage_join_requests(
    device: &DeviceInstance,
    req: ParticipantRequest,
    action: RequestAction,
    account_validation: bool,
) -> ApiResult<Vec<ParticipantStatus>> {
    group_and_participants(&req.group_id, &req.participants)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    let participants = participants_to_jids(&client, &req.participants).await?;
    let groups = client.groups();
    let changed = match action {
        RequestAction::Approve => {
            groups
                .approve_membership_requests(group, &participants)
                .await
        }
        RequestAction::Reject => {
            groups
                .reject_membership_requests(group, &participants)
                .await
        }
    }
    .map_err(ApiError::internal)?;
    let verb = action.as_str();
    Ok(changed
        .into_iter()
        .map(|p| match p.error {
            Some(code) => ParticipantStatus {
                participant: p.jid.to_string(),
                status: "error".into(),
                message: format!("Action {verb} failed (code {code})"),
            },
            None => ParticipantStatus {
                participant: p.jid.to_string(),
                status: "success".into(),
                message: format!("Action {verb} success"),
            },
        })
        .collect())
}

/// `None` removes the photo. Returns the new picture id (empty on removal).
pub async fn set_photo(
    device: &DeviceInstance,
    group_id: &str,
    photo: Option<Upload>,
    account_validation: bool,
) -> ApiResult<String> {
    group_required(group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, group_id, account_validation).await?;
    let groups = client.groups();
    match photo {
        Some(upload) => {
            let jpeg = media::image::group_photo(&upload.bytes).map_err(ApiError::internal)?;
            groups
                .set_profile_picture(group, jpeg)
                .await
                .map(|r| r.id)
                .map_err(ApiError::internal)
        }
        None => groups
            .remove_profile_picture(group)
            .await
            .map(|_| String::new())
            .map_err(ApiError::internal),
    }
}

pub async fn set_name(
    device: &DeviceInstance,
    req: NameRequest,
    account_validation: bool,
) -> ApiResult<()> {
    let mut errors = BTreeMap::new();
    required(&mut errors, "group_id", &req.group_id);
    if blank(&req.name) {
        errors.insert("name", "cannot be blank".into());
    } else if req.name.chars().count() > MAX_NAME_CHARS {
        errors.insert("name", "the length must be between 1 and 25".into());
    }
    ozzo(errors)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    let subject = GroupSubject::new(req.name).map_err(ApiError::internal)?;
    client
        .groups()
        .set_subject(group, subject)
        .await
        .map_err(ApiError::internal)
}

pub async fn set_locked(
    device: &DeviceInstance,
    req: LockedRequest,
    account_validation: bool,
) -> ApiResult<()> {
    group_required(&req.group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    client
        .groups()
        .set_locked(group, req.locked)
        .await
        .map_err(ApiError::internal)
}

pub async fn set_announce(
    device: &DeviceInstance,
    req: AnnounceRequest,
    account_validation: bool,
) -> ApiResult<()> {
    group_required(&req.group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    client
        .groups()
        .set_announce(group, req.announce)
        .await
        .map_err(ApiError::internal)
}

/// An empty topic removes the description.
pub async fn set_topic(
    device: &DeviceInstance,
    req: TopicRequest,
    account_validation: bool,
) -> ApiResult<()> {
    group_required(&req.group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, &req.group_id, account_validation).await?;
    let description = if req.topic.is_empty() {
        None
    } else {
        Some(GroupDescription::new(req.topic).map_err(ApiError::internal)?)
    };
    client
        .groups()
        .set_description(group, description, PreviousDescription::Resolve)
        .await
        .map_err(ApiError::internal)
}

pub async fn invite_link(
    device: &DeviceInstance,
    group_id: &str,
    reset: bool,
    account_validation: bool,
) -> ApiResult<InviteLink> {
    group_required(group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, group_id, account_validation).await?;
    let link = client
        .groups()
        .get_invite_link(group, reset)
        .await
        .map_err(ApiError::internal)?;
    Ok(InviteLink {
        invite_link: link,
        group_id: group_id.to_owned(),
    })
}

fn jid_str(j: &Option<Jid>) -> String {
    j.as_ref().map(|j| j.to_string()).unwrap_or_default()
}

/// whatsmeow `types.GroupInfo` marshalled by Go's `encoding/json` (embedded
/// structs are flattened, keys keep the Go field names).
fn group_info_json(
    meta: &GroupMetadata,
    phones: &[Option<Jid>],
    names: &HashMap<String, String>,
) -> Value {
    let participants: Vec<Value> = meta
        .participants
        .iter()
        .zip(phones)
        .map(|(p, phone)| {
            let phone_number = phone
                .clone()
                .or_else(|| p.phone_number.clone())
                .map(|j| j.to_string())
                .unwrap_or_default();
            let lid = match (&p.lid, p.jid.is_lid()) {
                (Some(l), _) => l.to_string(),
                (None, true) => p.jid.to_string(),
                _ => String::new(),
            };
            let display_name = phone
                .as_ref()
                .and_then(|j| names.get(&j.to_string()))
                .cloned()
                .unwrap_or_default();
            json!({
                "JID": p.jid.to_string(),
                "PhoneNumber": phone_number,
                "LID": lid,
                "IsAdmin": p.is_admin(),
                "IsSuperAdmin": p.is_super_admin(),
                "DisplayName": display_name,
                "Error": 0,
                "AddRequest": Value::Null,
            })
        })
        .collect();
    let ephemeral = meta.ephemeral.as_ref().and_then(|e| e.expiration);
    json!({
        "JID": meta.id.to_string(),
        "OwnerJID": jid_str(&meta.creator),
        "OwnerPN": jid_str(&meta.creator_pn),
        "Name": meta.subject.clone().unwrap_or_default(),
        "NameSetAt": format_ts(meta.subject_time.unwrap_or(0) as i64),
        "NameSetBy": jid_str(&meta.subject_owner),
        "NameSetByPN": jid_str(&meta.subject_owner_pn),
        "Topic": meta.description.clone().unwrap_or_default(),
        "TopicID": meta.description_id.clone().unwrap_or_default(),
        "TopicSetAt": format_ts(meta.description_time.unwrap_or(0) as i64),
        "TopicSetBy": jid_str(&meta.description_owner),
        "TopicSetByPN": jid_str(&meta.description_owner_pn),
        "TopicDeleted": false,
        "IsLocked": meta.is_locked,
        "IsAnnounce": meta.is_announcement,
        "AnnounceVersionID": "",
        "IsEphemeral": ephemeral.is_some_and(|e| e > 0),
        "DisappearingTimer": ephemeral.unwrap_or(0),
        "IsIncognito": meta.is_incognito,
        "IsParent": meta.is_parent_group,
        "DefaultMembershipApprovalMode": "",
        "LinkedParentJID": jid_str(&meta.parent_group_jid),
        "IsDefaultSubGroup": meta.is_default_sub_group,
        "IsJoinApprovalRequired": meta.membership_approval,
        "AddressingMode": meta.addressing_mode.as_str(),
        "GroupCreated": format_ts(meta.creation_time.unwrap_or(0) as i64),
        "CreatorCountryCode": meta.creator_country_code.clone().unwrap_or_default(),
        "ParticipantVersionID": meta.participant_version_id.clone().unwrap_or_default(),
        "Participants": participants,
        "MemberAddMode": meta.member_add_mode.map(|m| m.as_str()).unwrap_or_default(),
    })
}

pub async fn info(
    device: &DeviceInstance,
    group_id: &str,
    account_validation: bool,
) -> ApiResult<Value> {
    group_required(group_id)?;
    let client = must_login(device)?;
    let group = group_jid(&client, group_id, account_validation).await?;
    let meta = client
        .groups()
        .fetch_metadata(&group)
        .await
        .map_err(ApiError::internal)?;
    let mut phones = Vec::with_capacity(meta.participants.len());
    for p in &meta.participants {
        phones.push(phone_jid(&client, &p.jid, p.phone_number.as_ref()).await);
    }
    let lookups = phones.iter().flatten().map(|j| j.to_string()).collect();
    let names = contact_names(device, lookups).await;
    Ok(group_info_json(&meta, &phones, &names))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(r: ApiResult<()>) -> String {
        r.unwrap_err().message
    }

    #[test]
    fn participant_validation_matches_ozzo() {
        assert_eq!(
            err(group_and_participants("", &[])),
            "group_id: cannot be blank; participants: cannot be blank."
        );
        assert_eq!(
            err(group_and_participants(
                "1@g.us",
                &["62".into(), "".into(), " ".into()]
            )),
            "participants: (1: cannot be blank; 2: cannot be blank.)."
        );
        assert!(group_and_participants("1@g.us", &["62".into()]).is_ok());
    }

    #[test]
    fn lenient_bool_reads_json_and_forms() {
        let json: LockedRequest =
            serde_json::from_str(r#"{"group_id":"1","locked":true}"#).unwrap();
        assert!(json.locked);
        let form: LockedRequest = serde_html_form::from_str("group_id=1&locked=1").unwrap();
        assert!(form.locked);
        let form: AnnounceRequest = serde_html_form::from_str("group_id=1&announce=false").unwrap();
        assert!(!form.announce);
        let missing: AnnounceRequest = serde_json::from_str(r#"{"group_id":"1"}"#).unwrap();
        assert!(!missing.announce);
    }

    #[test]
    fn group_info_uses_whatsmeow_field_names() {
        let mut meta = GroupMetadata::new("1203@g.us".parse().unwrap());
        meta.subject = Some("Team".into());
        meta.is_locked = true;
        let v = group_info_json(&meta, &[], &HashMap::new());
        assert_eq!(v["JID"], "1203@g.us");
        assert_eq!(v["Name"], "Team");
        assert_eq!(v["IsLocked"], true);
        assert_eq!(v["GroupCreated"], "0001-01-01T00:00:00Z");
        assert_eq!(v["Participants"], json!([]));
    }
}

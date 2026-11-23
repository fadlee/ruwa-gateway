//! `/user/check` and `/user/info` (GOWA `usecase/user.go`).

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    names,
    send::{must_login, required, resolve_recipient},
};
use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    media::{self, Upload},
    storage::chat::Contact,
    wa::{
        Jid, Server, jid,
        sdk::{
            ProfilePicture, ProfilePictureLookup, ProfilePictureRequest, ProfilePictureTarget,
            ProfilePictureType, PushNameOutcome,
            prelude::Client,
            privacy_settings::{PrivacyCategory, PrivacyValue},
        },
    },
};

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub is_on_whatsapp: bool,
}

/// GOWA `IsOnWhatsApp`: non-phone JIDs (groups, LIDs, ...) count as present.
pub async fn check(device: &DeviceInstance, phone: &str) -> ApiResult<CheckResponse> {
    let client = must_login(device)?;
    let parsed = jid::parse_jid(&jid::sanitize_phone(phone)).map_err(ApiError::invalid_jid)?;
    if !jid::is_pn_user(&parsed) {
        return Ok(CheckResponse {
            is_on_whatsapp: true,
        });
    }
    let bare = Jid::pn(parsed.user.trim_start_matches('+'));
    if bare.user.is_empty() {
        return Ok(CheckResponse {
            is_on_whatsapp: false,
        });
    }
    let results = client
        .contacts()
        .is_on_whatsapp(std::slice::from_ref(&bare))
        .await
        .map_err(|e| {
            ApiError::internal(format!("failed to check {} on WhatsApp: {e}", bare.user))
        })?;
    Ok(CheckResponse {
        is_on_whatsapp: results.iter().any(|r| r.is_registered),
    })
}

/// GOWA `InfoResponseDataDevice` (untagged Go struct → capitalised keys).
#[derive(Debug, Serialize)]
#[allow(non_snake_case)]
pub struct InfoDevice {
    pub User: String,
    pub Agent: u8,
    pub Device: String,
    pub Server: String,
    pub AD: String,
}

#[derive(Debug, Serialize)]
pub struct InfoData {
    pub name: String,
    pub verified_name: String,
    pub status: String,
    pub picture_id: String,
    pub devices: Option<Vec<InfoDevice>>,
}

#[derive(Debug, Serialize)]
pub struct InfoResponse {
    pub data: Option<Vec<InfoData>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolved_phone: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolved_lid: String,
}

/// GOWA `GetPlatformName`.
pub fn platform_name(device: u16) -> &'static str {
    match device {
        0 => "UNKNOWN",
        1 => "CHROME",
        2 => "FIREFOX",
        3 => "IE",
        4 => "OPERA",
        5 => "SAFARI",
        6 => "EDGE",
        7 => "DESKTOP",
        8 => "IPAD",
        9 => "ANDROID_TABLET",
        10 => "OHANA",
        11 => "ALOHA",
        12 => "CATALINA",
        13 => "TCL_TV",
        14 => "IOS_PHONE",
        15 => "IOS_CATALYST",
        16 => "ANDROID_PHONE",
        17 => "ANDROID_AMBIGUOUS",
        18 => "WEAR_OS",
        19 => "AR_WRIST",
        20 => "AR_DEVICE",
        21 => "UWP",
        22 => "VR",
        _ => "UNKNOWN",
    }
}

fn info_device(j: &Jid) -> InfoDevice {
    InfoDevice {
        User: j.user.to_string(),
        Agent: j.agent,
        Device: platform_name(j.device).to_owned(),
        Server: j.server.as_str().to_owned(),
        AD: format!("{}.{}:{}@{}", j.user, j.agent, j.device, j.server.as_str()),
    }
}

pub async fn info(
    device: &DeviceInstance,
    phone: &str,
    account_validation: bool,
) -> ApiResult<InfoResponse> {
    let phone = jid::sanitize_phone(phone);
    required(&[("phone", &phone)])?;
    let client = must_login(device)?;
    let recipient = resolve_recipient(&client, &phone, account_validation).await?;

    let was_lid = jid::parse_jid(&phone).is_ok_and(|j| j.is_lid());
    let mut response = InfoResponse {
        data: None,
        resolved_phone: String::new(),
        resolved_lid: String::new(),
    };
    if was_lid && jid::is_pn_user(&recipient) {
        response.resolved_phone = recipient.user.to_string();
    }

    let infos = client
        .contacts()
        .get_user_info(std::slice::from_ref(&recipient))
        .await
        .map_err(ApiError::internal)?;
    let devices = client
        .signal()
        .get_user_devices(std::slice::from_ref(&recipient))
        .await
        .unwrap_or_default();

    let mut data = Vec::new();
    for (_, info) in infos {
        if jid::is_pn_user(&recipient)
            && let Some(lid) = &info.lid
        {
            response.resolved_lid = lid.to_non_ad().to_string();
        }
        let devices: Vec<_> = devices.iter().map(info_device).collect();
        data.push(InfoData {
            // Contact names come from chat storage (Fase 3).
            name: String::new(),
            verified_name: info
                .verified_name
                .as_ref()
                .and_then(|v| v.name.clone())
                .unwrap_or_default(),
            status: info.status.unwrap_or_default(),
            picture_id: info.picture_id.unwrap_or_default(),
            devices: (!devices.is_empty()).then_some(devices),
        });
    }
    response.data = (!data.is_empty()).then_some(data);
    Ok(response)
}

/// `GET /user/my/privacy` (GOWA `MyPrivacySetting`). GOWA never fills
/// `last_seen`, so it is always empty; a setting the server left out is empty too.
pub async fn my_privacy(device: &DeviceInstance) -> ApiResult<Value> {
    let client = must_login(device)?;
    let settings = client
        .fetch_privacy_settings()
        .await
        .map_err(ApiError::internal)?;
    let value = |category: PrivacyCategory| {
        settings
            .get_value(&category)
            .map(privacy_value)
            .unwrap_or_default()
    };
    Ok(json!({
        "group_add": value(PrivacyCategory::GroupAdd),
        "last_seen": "",
        "status": value(PrivacyCategory::Status),
        "profile": value(PrivacyCategory::Profile),
        "read_receipts": value(PrivacyCategory::ReadReceipts),
    }))
}

fn privacy_value(value: &PrivacyValue) -> String {
    match value {
        PrivacyValue::All => "all".into(),
        PrivacyValue::Contacts => "contacts".into(),
        PrivacyValue::None => "none".into(),
        PrivacyValue::ContactBlacklist => "contact_blacklist".into(),
        PrivacyValue::MatchLastSeen => "match_last_seen".into(),
        PrivacyValue::Known => "known".into(),
        PrivacyValue::Off => "off".into(),
        PrivacyValue::OnStandard => "on_standard".into(),
        PrivacyValue::Other(other) => other.clone(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct PushNameRequest {
    #[serde(default)]
    pub push_name: String,
}

/// `POST /user/pushname` (GOWA `ChangePushName`; no validation there). An empty
/// name is refused by whatsapp-rust and surfaces as a 500.
pub async fn change_push_name(device: &DeviceInstance, req: PushNameRequest) -> ApiResult<()> {
    let client = must_login(device)?;
    let outcome = client
        .profile()
        .set_push_name(&req.push_name)
        .await
        .map_err(ApiError::internal)?;
    // GOWA returns the app-state error; the name is only half applied then.
    if let PushNameOutcome::SyncPending { source } = outcome {
        return Err(ApiError::internal(source));
    }
    Ok(())
}

/// Go marshals a nil slice as `null`.
fn data_or_null(rows: Vec<Value>) -> Value {
    json!({ "data": if rows.is_empty() { Value::Null } else { Value::Array(rows) } })
}

/// `GET /user/my/contacts` (GOWA `MyListContacts`): the contact store with one
/// preferred display name each. GOWA iterates a Go map (random order); we sort.
pub async fn my_contacts(device: &DeviceInstance) -> ApiResult<Value> {
    must_login(device)?;
    let contacts = device
        .chats()
        .all_contacts(&device.storage_partition())
        .await
        .map_err(ApiError::internal)?;
    let rows = contacts
        .iter()
        .map(|c| json!({ "jid": c.jid, "name": names::preferred_contact_name(Some(c), "") }))
        .collect();
    Ok(data_or_null(rows))
}

/// whatsmeow `Jid.IsBot` for phone-number JIDs (Meta AI and friends).
fn is_bot(jid: &Jid) -> bool {
    jid.server == Server::Pn
        && jid.device == 0
        && (jid.user.starts_with("1313555") || jid.user.starts_with("131655500"))
}

fn non_blank(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

/// Person contacts keyed without device part, sorted by JID: phone-number and
/// LID keys only, no bots; of keys differing in the device part the bare key
/// wins, else the lowest device.
fn person_contacts(contacts: &[Contact]) -> Vec<(String, &Contact)> {
    let mut chosen: std::collections::BTreeMap<String, (u16, &Contact)> = Default::default();
    for contact in contacts {
        let Ok(parsed) = contact.jid.parse::<Jid>() else {
            continue;
        };
        if !(parsed.server == Server::Pn || parsed.is_lid()) || is_bot(&parsed) {
            continue;
        }
        let key = parsed.to_non_ad().to_string();
        if chosen
            .get(&key)
            .is_none_or(|(device, _)| parsed.device < *device)
        {
            chosen.insert(key, (parsed.device, contact));
        }
    }
    chosen.into_iter().map(|(k, (_, c))| (k, c)).collect()
}

/// `GET /user/my/contacts/detailed` (GOWA `buildDetailedContacts`): person
/// contacts only (phone-number and LID keys, no bots), each name in its own
/// field and omitted when blank, `lid` only for a phone-number key the LID
/// store maps, one row per user (key without device part wins, else the lowest
/// device), sorted by `jid`.
pub async fn my_contacts_detailed(device: &DeviceInstance) -> ApiResult<Value> {
    let client = must_login(device)?;
    let contacts = device
        .chats()
        .all_contacts(&device.storage_partition())
        .await
        .map_err(ApiError::internal)?;
    let chosen = person_contacts(&contacts);
    let mut rows = Vec::with_capacity(chosen.len());
    for (key, c) in chosen {
        let mut row = serde_json::Map::new();
        row.insert("jid".into(), json!(key));
        if key.ends_with("@s.whatsapp.net")
            && let Ok(parsed) = key.parse::<Jid>()
            && let Ok(Some(entry)) = client.get_lid_pn_entry(&parsed).await
        {
            row.insert("lid".into(), json!(format!("{}@lid", entry.lid)));
        }
        for (field, value) in [
            ("full_name", &c.full_name),
            ("first_name", &c.first_name),
            ("push_name", &c.push_name),
            ("business_name", &c.business_name),
        ] {
            if let Some(v) = non_blank(value) {
                row.insert(field.into(), json!(v));
            }
        }
        rows.push(Value::Object(row));
    }
    Ok(data_or_null(rows))
}

#[derive(Debug, Default, Deserialize)]
pub struct SubscribeRequest {
    #[serde(default)]
    pub phone: String,
}

/// `POST /user/presence/subscribe` (GOWA `SubscribePresence`): individual users
/// only; a group or newsletter JID is refused instead of a false success.
pub async fn subscribe_presence(
    device: &DeviceInstance,
    req: SubscribeRequest,
    account_validation: bool,
) -> ApiResult<Value> {
    required(&[("phone", req.phone.trim())])?;
    let client = must_login(device)?;
    let phone = jid::sanitize_phone(req.phone.trim());
    let target = resolve_recipient(&client, &phone, account_validation).await?;
    if target.server != Server::Pn {
        return Err(ApiError::invalid_jid(format!(
            "presence subscription requires a user phone number, got non-user JID {}",
            target.to_non_ad()
        )));
    }
    client
        .presence()
        .subscribe(target.clone())
        .await
        .map_err(ApiError::internal)?;
    Ok(json!({ "subscribed": true, "jid": target.to_non_ad().to_string() }))
}

/// `GET /user/avatar` query. Booleans stay strings: Fiber's `Bind().Query`
/// fails on a malformed one (a 500), which `parse_flag` reproduces.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AvatarQuery {
    pub phone: String,
    pub is_preview: String,
    pub is_community: String,
}

/// Go `strconv.ParseBool`; an absent value is false.
fn parse_flag(field: &str, raw: &str) -> ApiResult<bool> {
    match raw {
        "" | "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        _ => Err(ApiError::internal(format!(
            "failed to decode query: {field}: invalid boolean {raw:?}"
        ))),
    }
}

const AVATAR_NOT_SET: &str = "that user or group does not have a profile picture";
const AVATAR_HIDDEN: &str = "the user has hidden their profile picture from you";
const AVATAR_TIMEOUT: &str = "Error timeout get avatar!";

/// One picture lookup, with whatsmeow's three error sentinels kept apart.
#[derive(Debug, PartialEq, Eq)]
enum Fetched {
    Found(ProfilePicture),
    NotSet,
    Hidden,
    Timeout,
    Failed(String),
}

impl Fetched {
    fn into_result(self) -> ApiResult<ProfilePicture> {
        match self {
            Self::Found(pic) => Ok(pic),
            Self::NotSet => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "AVATAR_NOT_SET",
                AVATAR_NOT_SET,
            )),
            Self::Hidden => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "AVATAR_HIDDEN",
                AVATAR_HIDDEN,
            )),
            Self::Timeout => Err(ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "CONTEXT_ERROR",
                AVATAR_TIMEOUT,
            )),
            Self::Failed(message) => Err(ApiError::internal(message)),
        }
    }

    fn message(self) -> String {
        match self {
            Self::NotSet => AVATAR_NOT_SET.into(),
            Self::Hidden => AVATAR_HIDDEN.into(),
            Self::Timeout => AVATAR_TIMEOUT.into(),
            Self::Failed(message) => message,
            Self::Found(_) => "no avatar found".into(),
        }
    }
}

/// GOWA `Avatar` fallback: after a failed community lookup the plain one
/// decides. A "not set" / "hidden" answer is only reported as such when both
/// attempts agree (a community picture asked the plain way can read as
/// unauthorized), otherwise the fallback's error comes back unclassified.
fn combine(community: Fetched, plain: Fetched) -> ApiResult<ProfilePicture> {
    match (community, plain) {
        (_, Fetched::Found(pic)) => Ok(pic),
        (_, Fetched::Timeout) => Fetched::Timeout.into_result(),
        (Fetched::NotSet, Fetched::NotSet) => Fetched::NotSet.into_result(),
        (Fetched::Hidden, Fetched::Hidden) => Fetched::Hidden.into_result(),
        (_, other) => Err(ApiError::internal(other.message())),
    }
}

async fn fetch_avatar(
    client: &Client,
    target: &Jid,
    kind: ProfilePictureType,
    community: bool,
) -> Fetched {
    let route = if community {
        ProfilePictureTarget::Community(target)
    } else if target.server == Server::Group {
        ProfilePictureTarget::Group(target)
    } else {
        ProfilePictureTarget::Contact(target)
    };
    let pictures = client.pictures();
    let lookup = pictures.lookup(ProfilePictureRequest::new(route, kind));
    match tokio::time::timeout(std::time::Duration::from_secs(5), lookup).await {
        Err(_) => Fetched::Timeout,
        Ok(Err(error)) => Fetched::Failed(error.to_string()),
        Ok(Ok(ProfilePictureLookup::Found(pic))) => Fetched::Found(pic),
        Ok(Ok(ProfilePictureLookup::NotFound)) => Fetched::NotSet,
        Ok(Ok(ProfilePictureLookup::NotAuthorized)) => Fetched::Hidden,
        // Only a conditional lookup answers "unchanged", and we never send one.
        Ok(Ok(_)) => Fetched::Failed("no avatar found".into()),
    }
}

/// `GET /user/avatar` (GOWA `Avatar`).
pub async fn avatar(
    device: &DeviceInstance,
    query: AvatarQuery,
    account_validation: bool,
) -> ApiResult<Value> {
    let preview = parse_flag("is_preview", &query.is_preview)?;
    let community = parse_flag("is_community", &query.is_community)?;
    let phone = jid::sanitize_phone(&query.phone);
    required(&[("phone", &phone)])?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, &phone, account_validation).await?;
    let kind = if preview {
        ProfilePictureType::Preview
    } else {
        ProfilePictureType::Full
    };
    // A community route on a plain user JID only times out.
    let community = community && target.server != Server::Pn;
    let first = fetch_avatar(&client, &target, kind, community).await;
    let pic = match first {
        Fetched::Found(pic) => pic,
        Fetched::Timeout => return Fetched::Timeout.into_result().map(|_| Value::Null),
        first if community => combine(first, fetch_avatar(&client, &target, kind, false).await)?,
        first => first.into_result()?,
    };
    Ok(json!({
        "url": pic.url,
        "id": pic.id,
        "type": if preview { "preview" } else { "image" },
    }))
}

/// `POST /user/avatar` (GOWA `ChangeAvatar`): centre square crop, at most 640px,
/// JPEG quality 80, set as the account's own picture.
pub async fn change_avatar(device: &DeviceInstance, upload: Option<Upload>) -> ApiResult<()> {
    let upload = upload.ok_or_else(|| {
        ApiError::internal("there is no uploaded file associated with the given key")
    })?;
    let client = must_login(device)?;
    let jpeg = media::image::group_photo(&upload.bytes)
        .map_err(|e| ApiError::internal(format!("failed to decode image: {e}")))?;
    client
        .profile()
        .set_profile_picture(jpeg)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ad_string_like_whatsmeow() {
        let j: Jid = "628123:3@s.whatsapp.net".parse().unwrap();
        let d = info_device(&j);
        assert_eq!(d.AD, "628123.0:3@s.whatsapp.net");
        assert_eq!(d.Device, "IE");
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("User").is_some() && v.get("AD").is_some());
    }

    fn contact(jid: &str, full: &str) -> Contact {
        Contact {
            jid: jid.into(),
            full_name: full.into(),
            ..Default::default()
        }
    }

    #[test]
    fn person_contacts_filter_dedupe_and_sort() {
        let all = [
            contact("628222@s.whatsapp.net", "Bob"),
            contact("628111:5@s.whatsapp.net", "Alice device 5"),
            contact("628111:2@s.whatsapp.net", "Alice device 2"),
            contact("628111@s.whatsapp.net", "Alice"),
            contact("999@lid", "Lid person"),
            contact("1203@g.us", "A group"),
            contact("status@broadcast", "Broadcast"),
            contact("13135550123@s.whatsapp.net", "Meta AI"),
            contact("not a jid", "Junk"),
        ];
        let rows = person_contacts(&all);
        let keys: Vec<_> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["628111@s.whatsapp.net", "628222@s.whatsapp.net", "999@lid"]
        );
        // The bare key beats any device-suffixed one.
        assert_eq!(rows[0].1.full_name, "Alice");

        let no_bare = [
            contact("628111:5@s.whatsapp.net", "five"),
            contact("628111:2@s.whatsapp.net", "two"),
        ];
        assert_eq!(person_contacts(&no_bare)[0].1.full_name, "two");
    }

    #[test]
    fn privacy_values_use_wire_names() {
        assert_eq!(
            privacy_value(&PrivacyValue::ContactBlacklist),
            "contact_blacklist"
        );
        assert_eq!(privacy_value(&PrivacyValue::All), "all");
        assert_eq!(privacy_value(&PrivacyValue::Other("x".into())), "x");
        assert_eq!(non_blank("  "), None);
        assert_eq!(non_blank(" a "), Some(" a "));
    }

    fn pic(id: &str) -> ProfilePicture {
        ProfilePicture {
            id: id.into(),
            url: format!("https://pps/{id}"),
            direct_path: None,
            hash: None,
        }
    }

    #[test]
    fn avatar_flags_like_strconv_parse_bool() {
        assert!(parse_flag("f", "true").unwrap());
        assert!(parse_flag("f", "T").unwrap());
        assert!(parse_flag("f", "1").unwrap());
        assert!(!parse_flag("f", "").unwrap());
        assert!(!parse_flag("f", "False").unwrap());
        assert_eq!(
            parse_flag("f", "yes").unwrap_err().code,
            "INTERNAL_SERVER_ERROR"
        );
    }

    #[test]
    fn avatar_errors_use_gowa_codes() {
        let hidden = Fetched::Hidden.into_result().unwrap_err();
        assert_eq!(
            (hidden.status, hidden.code.as_str(), hidden.message.as_str()),
            (
                StatusCode::FORBIDDEN,
                "AVATAR_HIDDEN",
                "the user has hidden their profile picture from you"
            )
        );
        let unset = Fetched::NotSet.into_result().unwrap_err();
        assert_eq!(unset.status, StatusCode::NOT_FOUND);
        assert_eq!(unset.code, "AVATAR_NOT_SET");
        let slow = Fetched::Timeout.into_result().unwrap_err();
        assert_eq!(
            (slow.status, slow.code.as_str(), slow.message.as_str()),
            (
                StatusCode::REQUEST_TIMEOUT,
                "CONTEXT_ERROR",
                "Error timeout get avatar!"
            )
        );
        assert_eq!(
            Fetched::Failed("boom".into())
                .into_result()
                .unwrap_err()
                .message,
            "boom"
        );
    }

    #[test]
    fn community_fallback_only_classifies_when_both_agree() {
        assert_eq!(
            combine(Fetched::NotSet, Fetched::Found(pic("1")))
                .unwrap()
                .id,
            "1"
        );
        assert_eq!(
            combine(Fetched::NotSet, Fetched::NotSet).unwrap_err().code,
            "AVATAR_NOT_SET"
        );
        assert_eq!(
            combine(Fetched::Hidden, Fetched::Hidden).unwrap_err().code,
            "AVATAR_HIDDEN"
        );
        // Disagreement: the fallback's error, unclassified (a plain 500).
        let mixed = combine(Fetched::Hidden, Fetched::NotSet).unwrap_err();
        assert_eq!(mixed.code, "INTERNAL_SERVER_ERROR");
        assert_eq!(
            mixed.message,
            "that user or group does not have a profile picture"
        );
        assert_eq!(
            combine(Fetched::NotSet, Fetched::Timeout).unwrap_err().code,
            "CONTEXT_ERROR"
        );
    }
}

/// One `business_hours` row as the SDK reports it.
pub struct HoursRow<'a> {
    pub day: &'a str,
    pub mode: &'a str,
    pub open: Option<u32>,
    pub close: Option<u32>,
}

/// `HH:MM` from minutes past midnight. GOWA's `FormatBusinessHourTime`
/// divides by 100 (it expects `HHMM`), which misreads WhatsApp's minutes
/// (540 → "05:40"); minutes are the real unit, so "09:00" is what we print.
/// An absent time is `00:00`, like GOWA's zero value.
fn hhmm(minutes: Option<u32>) -> String {
    let m = minutes.unwrap_or(0);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// GOWA `BusinessProfileResponse`; empty collections are `null` (Go nil).
#[allow(clippy::too_many_arguments)]
fn business_profile_json(
    jid: &str,
    email: &str,
    address: &str,
    categories: &[(String, String)],
    options: &[(&str, String)],
    timezone: &str,
    hours: &[HoursRow<'_>],
) -> Value {
    let or_null = |v: Vec<Value>| {
        if v.is_empty() {
            Value::Null
        } else {
            Value::Array(v)
        }
    };
    let options: serde_json::Map<String, Value> = options
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| ((*k).to_owned(), json!(v)))
        .collect();
    json!({
        "jid": jid,
        "email": email,
        "address": address,
        "categories": or_null(categories.iter().map(|(id, name)| json!({"id": id, "name": name})).collect()),
        "profile_options": if options.is_empty() { Value::Null } else { Value::Object(options) },
        "business_hours_timezone": timezone,
        "business_hours": or_null(hours.iter().map(|h| json!({
            "day_of_week": h.day,
            "mode": h.mode,
            "open_time": hhmm(h.open),
            "close_time": hhmm(h.close),
        })).collect()),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct BusinessProfileQuery {
    pub phone: String,
}

/// `GET /user/business-profile` (GOWA `BusinessProfile`). `profile_options`
/// carries `description` and `website` (the SDK does not expose whatsmeow's
/// commerce flags).
pub async fn business_profile(
    device: &DeviceInstance,
    phone: &str,
    account_validation: bool,
) -> ApiResult<Value> {
    let phone = jid::sanitize_phone(phone);
    required(&[("phone", &phone)])?;
    let client = must_login(device)?;
    let target = resolve_recipient(&client, &phone, account_validation).await?;
    let profile = client
        .get_business_profile(&target)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::internal("business profile not found"))?;
    let categories: Vec<(String, String)> = profile
        .categories
        .iter()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    let configs = profile
        .business_hours
        .business_config
        .as_deref()
        .unwrap_or(&[]);
    let hours: Vec<HoursRow<'_>> = configs
        .iter()
        .map(|c| HoursRow {
            day: c.day_of_week.as_str(),
            mode: c.mode.as_str(),
            open: c.open_time,
            close: c.close_time,
        })
        .collect();
    Ok(business_profile_json(
        &target.to_string(),
        profile.email.as_deref().unwrap_or_default(),
        profile.address.as_deref().unwrap_or_default(),
        &categories,
        &[
            ("description", profile.description.clone()),
            ("website", profile.website.join(", ")),
        ],
        profile
            .business_hours
            .timezone
            .as_deref()
            .unwrap_or_default(),
        &hours,
    ))
}

/// Plain view of a channel for [`newsletter_json`].
pub struct ChannelView<'a> {
    pub id: String,
    pub state: &'a str,
    pub creation_time: Option<u64>,
    pub invite: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub subscribers: u64,
    pub verification: &'a str,
    pub picture_url: Option<&'a str>,
    pub preview_url: Option<&'a str>,
    pub mute: Option<&'a str>,
    pub role: Option<&'a str>,
}

/// whatsmeow `types.NewsletterMetadata` JSON (the shape in the OpenAPI
/// `Newsletter` schema). Fields the SDK does not expose are empty strings.
fn newsletter_json(c: &ChannelView<'_>) -> Value {
    let text = |t: &str| json!({"text": t, "id": "", "update_time": ""});
    let image = |url: Option<&str>, kind: &str| match url {
        Some(url) => json!({"url": url, "id": "", "type": kind, "direct_path": ""}),
        None => Value::Null,
    };
    json!({
        "id": c.id,
        "state": {"type": c.state},
        "thread_metadata": {
            "creation_time": c.creation_time.map(|t| t.to_string()).unwrap_or_default(),
            "invite": c.invite,
            "name": text(c.name),
            "description": text(c.description),
            "subscribers_count": c.subscribers.to_string(),
            "verification": c.verification,
            "picture": image(c.picture_url, "IMAGE"),
            "preview": image(c.preview_url, "PREVIEW"),
            "settings": {"reaction_codes": {"value": ""}},
        },
        "viewer_metadata": match (c.mute, c.role) {
            (None, None) => Value::Null,
            (mute, role) => json!({"mute": mute.unwrap_or_default(), "role": role.unwrap_or_default()}),
        },
    })
}

/// One channel as whatsmeow's `types.NewsletterMetadata` JSON.
pub(crate) fn newsletter_value(c: &crate::wa::sdk::NewsletterMetadata) -> Value {
    use crate::wa::sdk::{NewsletterRole, NewsletterState, features::NewsletterVerification};
    let state = match &c.state {
        NewsletterState::Active => "active".to_owned(),
        NewsletterState::Suspended => "suspended".to_owned(),
        NewsletterState::Geosuspended => "geosuspended".to_owned(),
        NewsletterState::Other(s) => s.to_lowercase(),
        _ => String::new(),
    };
    let verification = match &c.verification {
        NewsletterVerification::Verified => "verified".to_owned(),
        NewsletterVerification::Unverified => "unverified".to_owned(),
        NewsletterVerification::Other(s) => s.to_lowercase(),
        _ => String::new(),
    };
    let role = c.role.as_ref().map(|r| match r {
        NewsletterRole::Owner => "owner",
        NewsletterRole::Admin => "admin",
        NewsletterRole::Subscriber => "subscriber",
        _ => "guest",
    });
    let mute = c.muted.map(|m| if m { "on" } else { "off" });
    newsletter_json(&ChannelView {
        id: c.jid.to_string(),
        state: &state,
        creation_time: c.creation_time,
        invite: c.invite_code.as_deref().unwrap_or_default(),
        name: &c.name,
        description: c.description.as_deref().unwrap_or_default(),
        subscribers: c.subscriber_count,
        verification: &verification,
        picture_url: c.picture_url.as_deref(),
        preview_url: c.preview_url.as_deref(),
        mute,
        role,
    })
}

/// `GET /user/my/newsletters` (GOWA `MyListNewsletter`): subscribed channels,
/// filling a missing subscriber count with one detail query (5 at a time,
/// 5 s each; GOWA runs a sliding window, we run batches of 5).
pub async fn my_newsletters(device: &DeviceInstance) -> ApiResult<Value> {
    let client = must_login(device)?;
    let mut channels = client
        .newsletter()
        .list_subscribed()
        .await
        .map_err(ApiError::internal)?;
    let missing: Vec<(usize, Jid)> = channels
        .iter()
        .enumerate()
        .filter(|(_, c)| c.subscriber_count == 0)
        .map(|(i, c)| (i, c.jid.clone()))
        .collect();
    let mut counts: Vec<(usize, u64)> = Vec::new();
    for chunk in missing.chunks(5) {
        let mut set = tokio::task::JoinSet::new();
        for (i, jid) in chunk.iter().cloned() {
            let client = client.clone();
            set.spawn(async move {
                let detail = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    client.newsletter().get_metadata(&jid),
                )
                .await;
                match detail {
                    Ok(Ok(meta)) => Some((i, meta.subscriber_count)),
                    _ => None,
                }
            });
        }
        while let Some(done) = set.join_next().await {
            if let Ok(Some(entry)) = done {
                counts.push(entry);
            }
        }
    }
    for (i, count) in counts {
        channels[i].subscriber_count = count;
    }

    let rows: Vec<Value> = channels.iter().map(newsletter_value).collect();
    Ok(data_or_null(rows))
}

#[cfg(test)]
mod business_tests {
    use super::*;

    #[test]
    fn business_profile_shape() {
        let v = business_profile_json(
            "6289685028129@s.whatsapp.net",
            "b@example.com",
            "Street 1",
            &[("1".into(), "Retail".into())],
            &[("description", "Shop".into()), ("website", String::new())],
            "Asia/Jakarta",
            &[
                HoursRow {
                    day: "mon",
                    mode: "specific_hours",
                    open: Some(540),
                    close: Some(1020),
                },
                HoursRow {
                    day: "tue",
                    mode: "open_24h",
                    open: None,
                    close: None,
                },
            ],
        );
        assert_eq!(v["categories"][0], json!({"id": "1", "name": "Retail"}));
        assert_eq!(v["profile_options"], json!({"description": "Shop"}));
        assert_eq!(v["business_hours"][0]["open_time"], "09:00");
        assert_eq!(v["business_hours"][0]["close_time"], "17:00");
        assert_eq!(v["business_hours"][1]["open_time"], "00:00");

        let empty = business_profile_json("j", "", "", &[], &[], "", &[]);
        assert!(empty["categories"].is_null());
        assert!(empty["profile_options"].is_null());
        assert!(empty["business_hours"].is_null());
    }

    #[test]
    fn newsletter_shape() {
        let v = newsletter_json(&ChannelView {
            id: "120363144038483540@newsletter".into(),
            state: "active",
            creation_time: Some(1_688_746_895),
            invite: "0029Va",
            name: "WhatsApp",
            description: "Official",
            subscribers: 12,
            verification: "verified",
            picture_url: Some("https://x/p.jpg"),
            preview_url: None,
            mute: Some("off"),
            role: Some("subscriber"),
        });
        assert_eq!(v["state"]["type"], "active");
        assert_eq!(v["thread_metadata"]["creation_time"], "1688746895");
        assert_eq!(v["thread_metadata"]["name"]["text"], "WhatsApp");
        assert_eq!(v["thread_metadata"]["subscribers_count"], "12");
        assert_eq!(v["thread_metadata"]["picture"]["type"], "IMAGE");
        assert!(v["thread_metadata"]["preview"].is_null());
        assert_eq!(
            v["viewer_metadata"],
            json!({"mute": "off", "role": "subscriber"})
        );
    }
}

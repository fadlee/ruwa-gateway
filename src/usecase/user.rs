//! `/user/check` and `/user/info` (GOWA `usecase/user.go`).

use serde::Serialize;

use super::send::{must_login, required, resolve_recipient};
use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    wa::{Jid, jid},
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
}

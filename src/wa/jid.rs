//! JID parsing ported from GOWA `pkg/utils/whatsapp.go`
//! (`SanitizePhone`, `ParseJID`, `FormatJID`).

use std::str::FromStr;

use super::{Jid, Server};

/// Longest phone number (digits) before a bare id is treated as a group id.
const MAX_PHONE_NUMBER_LENGTH: usize = 15;

/// GOWA `SanitizePhone`: append a server suffix to a bare number.
pub fn sanitize_phone(phone: &str) -> String {
    if phone.is_empty() || phone.contains('@') {
        return phone.to_owned();
    }
    if phone.len() <= MAX_PHONE_NUMBER_LENGTH {
        format!("{phone}@s.whatsapp.net")
    } else {
        format!("{phone}@g.us")
    }
}

/// GOWA `ParseJID`: strip a leading `+`, default to the user server.
pub fn parse_jid(arg: &str) -> Result<Jid, String> {
    let arg = arg.strip_prefix('+').unwrap_or(arg);
    if !arg.contains('@') {
        return Ok(Jid::pn(arg));
    }
    let jid = Jid::from_str(arg).map_err(|e| format!("invalid JID {arg}: {e}"))?;
    if jid.user.is_empty() {
        return Err(format!("invalid JID {arg}: no server specified"));
    }
    Ok(jid)
}

/// Non-AD string form (`user@server`, device suffix dropped).
pub fn non_ad_string(jid: &Jid) -> String {
    jid.to_non_ad().to_string()
}

pub fn is_group(jid: &Jid) -> bool {
    jid.server == Server::Group
}

pub fn is_lid(jid: &Jid) -> bool {
    jid.is_lid()
}

pub fn is_pn_user(jid: &Jid) -> bool {
    jid.server == Server::Pn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_adds_user_server() {
        assert_eq!(sanitize_phone("628123"), "628123@s.whatsapp.net");
    }

    #[test]
    fn sanitize_long_id_is_group() {
        assert_eq!(
            sanitize_phone("120363402106123456"),
            "120363402106123456@g.us"
        );
    }

    #[test]
    fn sanitize_keeps_full_jid() {
        assert_eq!(sanitize_phone("x@g.us"), "x@g.us");
        assert_eq!(sanitize_phone(""), "");
    }

    #[test]
    fn parse_strips_plus() {
        assert_eq!(
            parse_jid("+628123").unwrap().to_string(),
            "628123@s.whatsapp.net"
        );
        assert_eq!(
            parse_jid("+628123@s.whatsapp.net").unwrap().to_string(),
            "628123@s.whatsapp.net"
        );
    }

    #[test]
    fn parse_group_and_lid() {
        assert!(is_group(&parse_jid("120363@g.us").unwrap()));
        assert!(is_lid(&parse_jid("251556368777322@lid").unwrap()));
    }

    #[test]
    fn non_ad_drops_device() {
        let jid = parse_jid("628123:5@s.whatsapp.net").unwrap();
        assert_eq!(non_ad_string(&jid), "628123@s.whatsapp.net");
    }
}

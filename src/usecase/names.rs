//! Display-name policy (GOWA `chat_display_name.go`,
//! `sender_display_name.go`). Pure: callers normalise LID → PN and look up
//! the contact first.

use crate::{
    storage::chat::Contact,
    wa::{Jid, Server},
};

fn has(value: &str) -> bool {
    !value.trim().is_empty()
}

/// whatsmeow `types.ParseJID` of a JID with a user part, device suffix
/// dropped. A bare string (no `@`) has no user, like in whatsmeow.
pub fn user_jid(raw: &str) -> Option<Jid> {
    if !raw.contains('@') {
        return None;
    }
    let jid = raw.parse::<Jid>().ok()?.to_non_ad();
    has(&jid.user).then_some(jid)
}

/// GOWA `PreferredContactDisplayName`.
pub fn preferred_contact_name(contact: Option<&Contact>, live_push: &str) -> String {
    let empty = Contact::default();
    let c = contact.unwrap_or(&empty);
    [
        c.full_name.as_str(),
        live_push,
        c.push_name.as_str(),
        c.business_name.as_str(),
    ]
    .into_iter()
    .find(|n| has(n))
    .unwrap_or_default()
    .to_owned()
}

/// GOWA `ChatDisplayNameResolver.Resolve`. `normalized` is the chat JID
/// with a LID mapped to its phone number (or the parsed JID itself).
pub fn chat_display_name(
    raw: &str,
    normalized: Option<&Jid>,
    stored: &str,
    contact: Option<&Contact>,
) -> String {
    if raw == "status@broadcast" {
        return "Status".to_owned();
    }
    let original = user_jid(raw);
    if let Some(jid) = normalized {
        let fallback = match jid.server {
            Server::Group => Some("Group"),
            Server::Newsletter => Some("Newsletter"),
            _ => None,
        };
        if let Some(prefix) = fallback {
            return if has(stored) {
                stored.to_owned()
            } else {
                format!("{prefix} {}", jid.user)
            };
        }
    }
    let name = stored.trim();
    let is_fallback = name.is_empty()
        || name == raw.trim()
        || [original.as_ref(), normalized]
            .into_iter()
            .flatten()
            .any(|j| name == j.user.as_str() || name == j.to_string());
    if has(stored) && !is_fallback {
        return stored.to_owned();
    }
    if let Some(jid) = normalized {
        let preferred = preferred_contact_name(contact, "");
        if !preferred.is_empty() {
            return preferred;
        }
        if has(&jid.user) {
            return jid.user.to_string();
        }
    }
    if has(stored) {
        stored.to_owned()
    } else {
        raw.to_owned()
    }
}

/// The account a sender label is resolved for.
#[derive(Debug, Clone, Default)]
pub struct Account {
    /// Non-AD phone JID of the account.
    pub jid: Option<Jid>,
    /// Device display name, else the account's push name.
    pub name: String,
}

/// GOWA `SenderDisplayNameResolver.Resolve`. `normalized` is the sender
/// with a LID mapped to its phone number (or the parsed JID itself).
pub fn sender_display_name(
    raw: &str,
    normalized: Option<&Jid>,
    is_from_me: bool,
    account: &Account,
    live_push: &str,
    contact: Option<&Contact>,
) -> String {
    let is_account =
        is_from_me || normalized.is_some_and(|j| account.jid.as_ref().is_some_and(|a| a == j));
    if is_account {
        if has(&account.name) {
            return account.name.clone();
        }
        if let Some(a) = &account.jid {
            if has(&a.user) {
                return a.user.to_string();
            }
            return a.to_string();
        }
        return match normalized {
            Some(j) if has(&j.user) => j.user.to_string(),
            Some(j) => j.to_string(),
            None => raw.to_owned(),
        };
    }
    if let Some(c) = contact
        && has(&c.full_name)
    {
        return c.full_name.clone();
    }
    if has(live_push) {
        return live_push.to_owned();
    }
    let preferred = preferred_contact_name(contact, "");
    if !preferred.is_empty() {
        return preferred;
    }
    match normalized {
        Some(j) if has(&j.user) => j.user.to_string(),
        _ => raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact(full: &str, push: &str) -> Contact {
        Contact {
            full_name: full.into(),
            push_name: push.into(),
            ..Default::default()
        }
    }

    #[test]
    fn chat_names() {
        let pn = user_jid("628222@s.whatsapp.net").unwrap();
        let raw = "628222@s.whatsapp.net";
        // Meaningful stored names win; JID-like ones fall back to contacts.
        assert_eq!(chat_display_name(raw, Some(&pn), "Bob", None), "Bob");
        let c = contact("Robert", "Bobby");
        assert_eq!(
            chat_display_name(raw, Some(&pn), "628222", Some(&c)),
            "Robert"
        );
        assert_eq!(
            chat_display_name(raw, Some(&pn), "", Some(&contact("", "Bobby"))),
            "Bobby"
        );
        assert_eq!(chat_display_name(raw, Some(&pn), raw, None), "628222");
        // LID chat normalised to PN: the stored LID user is a fallback name.
        let lid_raw = "999@lid";
        assert_eq!(
            chat_display_name(lid_raw, Some(&pn), "999", Some(&c)),
            "Robert"
        );
        let g = user_jid("1203@g.us").unwrap();
        assert_eq!(
            chat_display_name("1203@g.us", Some(&g), "", None),
            "Group 1203"
        );
        assert_eq!(
            chat_display_name("1203@g.us", Some(&g), "Team", None),
            "Team"
        );
        assert_eq!(
            chat_display_name("status@broadcast", None, "", None),
            "Status"
        );
        assert_eq!(chat_display_name("garbage", None, "", None), "garbage");
    }

    #[test]
    fn sender_names() {
        let me = Account {
            jid: user_jid("628111@s.whatsapp.net"),
            name: "Me".into(),
        };
        let bob = user_jid("628222@s.whatsapp.net").unwrap();
        let raw = "628222@s.whatsapp.net";
        assert_eq!(
            sender_display_name(raw, Some(&bob), true, &me, "", None),
            "Me"
        );
        let own = me.jid.clone().unwrap();
        assert_eq!(
            sender_display_name("628111@s.whatsapp.net", Some(&own), false, &me, "", None),
            "Me"
        );
        let nameless = Account {
            jid: me.jid.clone(),
            name: String::new(),
        };
        assert_eq!(
            sender_display_name(raw, Some(&bob), true, &nameless, "", None),
            "628111"
        );

        let c = contact("Robert", "Bobby");
        assert_eq!(
            sender_display_name(raw, Some(&bob), false, &me, "Live", Some(&c)),
            "Robert"
        );
        let c = contact("", "Bobby");
        assert_eq!(
            sender_display_name(raw, Some(&bob), false, &me, "Live", Some(&c)),
            "Live"
        );
        assert_eq!(
            sender_display_name(raw, Some(&bob), false, &me, "", Some(&c)),
            "Bobby"
        );
        assert_eq!(
            sender_display_name(raw, Some(&bob), false, &me, "", None),
            "628222"
        );
        assert_eq!(sender_display_name("x", None, false, &me, "", None), "x");
    }
}

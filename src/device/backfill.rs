//! Background fill-ins for names WhatsApp only hands over in pieces
//! (pattern from zapfast).
//!
//! * Group subjects: history sync brings groups without a name, so their chats
//!   carry the `Group <id>` placeholder. A paced queue asks for the metadata in
//!   small batches (a burst of queries hits the server's rate limit) and
//!   renames the chats that still carry the placeholder.
//! * Contacts: the phone sends a contact only when it changes, so a session
//!   linked before contacts were stored has none. One snapshot re-sync of the
//!   contact collection replays them all through `ContactUpdate`.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use super::{DeviceInstance, ingest::placeholder_group_name};
use crate::{
    storage::chat::ChatStore,
    wa::{
        Jid,
        jid::parse_jid,
        sdk::{AppStateResyncMode, GroupLookupResult, WAPatchName, prelude::Client},
    },
};

/// Slot-directory marker: the contacts were replayed from the phone.
const CONTACTS_RESYNCED: &str = "contacts-resynced";

/// How one group fared in a metadata query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Lookup {
    Name(String),
    /// Try again later (rate limit, timeout, missing from the answer).
    Retry,
    /// Asking again cannot help (forbidden, gone, no subject).
    Final,
}

pub(super) trait GroupNames {
    /// Subjects of `jids`; a group missing from the answer counts as `Retry`.
    async fn lookup(&self, jids: &[Jid]) -> Vec<(Jid, Lookup)>;
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Pacing {
    /// Pause between two queries.
    pub tick: Duration,
    /// Groups per query.
    pub batch: usize,
    /// First retry delay; doubles per failure.
    pub backoff: Duration,
    pub max_tries: u32,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
            batch: 20,
            backoff: Duration::from_secs(30),
            max_tries: 7,
        }
    }
}

fn subject_lookup(subject: Option<String>) -> Lookup {
    match subject.filter(|s| !s.is_empty()) {
        Some(name) => Lookup::Name(name),
        None => Lookup::Final,
    }
}

struct ClientNames(Arc<Client>);

impl GroupNames for ClientNames {
    async fn lookup(&self, jids: &[Jid]) -> Vec<(Jid, Lookup)> {
        let groups = self.0.groups();
        let Ok(results) = groups.fetch_metadata_batch(jids).await else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(results.len());
        for result in results {
            match result {
                GroupLookupResult::Found(meta) => {
                    out.push((meta.id.clone(), subject_lookup(meta.subject)));
                }
                // The batch cuts big groups short and drops the subject.
                GroupLookupResult::Truncated { id, .. } => {
                    let lookup = match groups.fetch_metadata(&id).await {
                        Ok(meta) => subject_lookup(meta.subject),
                        Err(_) => Lookup::Retry,
                    };
                    out.push((id, lookup));
                }
                GroupLookupResult::Forbidden(id) | GroupLookupResult::NotFound(id) => {
                    out.push((id, Lookup::Final));
                }
                _ => {}
            }
        }
        out
    }
}

struct Attempt {
    tries: u32,
    next_at: Instant,
}

/// Name every placeholder group chat of `partition`, then return. Stops early
/// when `alive` turns false.
pub(super) async fn fill_group_names<S: GroupNames>(
    chats: &ChatStore,
    partition: &str,
    source: &S,
    pacing: Pacing,
    alive: impl Fn() -> bool,
) {
    let mut attempts: HashMap<String, Attempt> = HashMap::new();
    let mut finished: HashSet<String> = HashSet::new();
    while alive() {
        let Ok(groups) = chats.group_chats(partition).await else {
            return;
        };
        let pending: Vec<Jid> = groups
            .into_iter()
            .filter(|(jid, _)| !finished.contains(jid))
            .filter_map(|(jid, name)| {
                let parsed = parse_jid(&jid).ok()?;
                (name == placeholder_group_name(&parsed)).then_some(parsed)
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        let now = Instant::now();
        let due: Vec<Jid> = pending
            .iter()
            .filter(|j| {
                attempts
                    .get(&j.to_string())
                    .is_none_or(|a| a.next_at <= now)
            })
            .take(pacing.batch)
            .cloned()
            .collect();
        if due.is_empty() {
            let wake = attempts
                .values()
                .map(|a| a.next_at)
                .min()
                .unwrap_or(now + pacing.tick);
            tokio::time::sleep(wake.saturating_duration_since(now)).await;
            continue;
        }
        let mut answers: HashMap<String, Lookup> = source
            .lookup(&due)
            .await
            .into_iter()
            .map(|(jid, lookup)| (jid.to_string(), lookup))
            .collect();
        for jid in &due {
            let key = jid.to_string();
            match answers.remove(&key).unwrap_or(Lookup::Retry) {
                Lookup::Name(name) => {
                    let _ = chats
                        .rename_chat_if(partition, &key, &placeholder_group_name(jid), &name)
                        .await;
                    finished.insert(key);
                }
                Lookup::Final => {
                    finished.insert(key);
                }
                Lookup::Retry => {
                    let tries = attempts.get(&key).map_or(0, |a| a.tries) + 1;
                    if tries >= pacing.max_tries {
                        finished.insert(key);
                    } else {
                        let delay = pacing.backoff * 2u32.pow(tries - 1);
                        attempts.insert(
                            key,
                            Attempt {
                                tries,
                                next_at: Instant::now() + delay,
                            },
                        );
                    }
                }
            }
        }
        tokio::time::sleep(pacing.tick).await;
    }
}

/// Start the group-name queue unless one already runs for this device.
pub(super) fn kick_group_names(device: &Arc<DeviceInstance>) {
    if device.group_names_running.swap(true, Ordering::SeqCst) {
        return;
    }
    let device = device.clone();
    tokio::spawn(async move {
        if let Some(client) = device.client() {
            let watched = device.clone();
            fill_group_names(
                device.chats(),
                &device.storage_partition(),
                &ClientNames(client),
                Pacing::default(),
                move || watched.is_logged_in(),
            )
            .await;
        }
        device.group_names_running.store(false, Ordering::SeqCst);
    });
}

/// Kick the queue the first time a group is seen in this process, so a group
/// that appears after the history sync is named too.
pub(super) fn note_group_seen(device: &Arc<DeviceInstance>, group: &Jid) {
    let first = device
        .seen_groups
        .lock()
        .expect("seen groups lock")
        .insert(group.to_string());
    if first {
        kick_group_names(device);
    }
}

/// Replay the phone's contacts once per link.
pub(super) fn resync_contacts(device: &Arc<DeviceInstance>) {
    if device.dir().join(CONTACTS_RESYNCED).exists() {
        return;
    }
    if device.contacts_resyncing.swap(true, Ordering::SeqCst) {
        return;
    }
    let device = device.clone();
    tokio::spawn(async move {
        if let Some(client) = device.client() {
            let wanted = WAPatchName::CriticalUnblockLow;
            match client
                .resync_app_state([wanted], AppStateResyncMode::Snapshot)
                .await
            {
                Ok(report) if report.synced.contains(&wanted) => {
                    mark_contacts_resynced(device.dir());
                    tracing::info!(device = %device.id(), "contacts replayed from the phone");
                }
                Ok(_) => {
                    tracing::warn!(device = %device.id(), "contacts not replayed yet; retrying on the next connection");
                }
                Err(error) => {
                    tracing::warn!(device = %device.id(), %error, "could not replay contacts");
                }
            }
        }
        device.contacts_resyncing.store(false, Ordering::SeqCst);
    });
}

/// A fresh link receives every contact with its first sync, so it needs no replay.
pub(super) fn mark_contacts_resynced(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(CONTACTS_RESYNCED), b"");
}

/// A new link starts over: forget that contacts were replayed.
pub(super) fn clear_contacts_marker(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(CONTACTS_RESYNCED));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::storage::{Db, chat::Chat};

    const DEV: &str = "628111@s.whatsapp.net";

    /// Answers by group id, so the order the queue picks groups in is free.
    struct Fake {
        answers: HashMap<String, Lookup>,
        asked: Mutex<Vec<usize>>,
    }

    impl Fake {
        fn new(answers: &[(&str, Lookup)]) -> Self {
            Self {
                answers: answers
                    .iter()
                    .map(|(user, l)| (format!("{user}@g.us"), l.clone()))
                    .collect(),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl GroupNames for Fake {
        async fn lookup(&self, jids: &[Jid]) -> Vec<(Jid, Lookup)> {
            self.asked.lock().unwrap().push(jids.len());
            jids.iter()
                .filter_map(|j| Some((j.clone(), self.answers.get(&j.to_string())?.clone())))
                .collect()
        }
    }

    fn fast() -> Pacing {
        Pacing {
            tick: Duration::from_millis(1),
            batch: 2,
            backoff: Duration::from_millis(1),
            max_tries: 3,
        }
    }

    async fn store_with(chats: &[(&str, &str)]) -> ChatStore {
        let s = ChatStore::new(Db::open(":memory:").unwrap());
        for (jid, name) in chats {
            s.store_chat(Chat {
                jid: (*jid).into(),
                device_id: DEV.into(),
                name: (*name).into(),
                ..Default::default()
            })
            .await
            .unwrap();
        }
        s
    }

    async fn name_of(s: &ChatStore, jid: &str) -> String {
        s.get_chat(DEV, jid).await.unwrap().unwrap().name
    }

    #[tokio::test]
    async fn names_placeholder_groups_in_batches() {
        let s = store_with(&[
            ("1@g.us", "Group 1"),
            ("2@g.us", "Group 2"),
            ("3@g.us", "Group 3"),
            ("4@g.us", "Already named"),
            ("628@s.whatsapp.net", "Group 628"),
        ])
        .await;
        let source = Fake::new(&[
            ("1", Lookup::Name("Family".into())),
            ("2", Lookup::Final),
            ("3", Lookup::Name("Work".into())),
        ]);
        fill_group_names(&s, DEV, &source, fast(), || true).await;
        assert_eq!(name_of(&s, "1@g.us").await, "Family");
        assert_eq!(name_of(&s, "2@g.us").await, "Group 2");
        assert_eq!(name_of(&s, "3@g.us").await, "Work");
        assert_eq!(name_of(&s, "4@g.us").await, "Already named");
        assert_eq!(name_of(&s, "628@s.whatsapp.net").await, "Group 628");
        // Batch size is honoured, and finished groups are not asked again.
        let mut asked = source.asked.lock().unwrap().clone();
        asked.sort_unstable();
        assert_eq!(asked, vec![1, 2]);
    }

    #[tokio::test]
    async fn retries_with_backoff_then_gives_up() {
        let s = store_with(&[("1@g.us", "Group 1")]).await;
        // Never answered: counts as Retry each time, three tries then stop.
        let source = Fake::new(&[]);
        fill_group_names(&s, DEV, &source, fast(), || true).await;
        assert_eq!(source.asked.lock().unwrap().len(), 3);
        assert_eq!(name_of(&s, "1@g.us").await, "Group 1");
    }

    #[tokio::test]
    async fn never_overwrites_a_name_set_meanwhile() {
        let s = store_with(&[("1@g.us", "Group 1")]).await;
        struct Racing<'a>(&'a ChatStore);
        impl GroupNames for Racing<'_> {
            async fn lookup(&self, jids: &[Jid]) -> Vec<(Jid, Lookup)> {
                self.0
                    .rename_chat_if(DEV, "1@g.us", "Group 1", "From history")
                    .await
                    .unwrap();
                jids.iter()
                    .map(|j| (j.clone(), Lookup::Name("From query".into())))
                    .collect()
            }
        }
        fill_group_names(&s, DEV, &Racing(&s), fast(), || true).await;
        assert_eq!(name_of(&s, "1@g.us").await, "From history");
    }

    #[tokio::test]
    async fn stops_when_the_device_goes_away() {
        let s = store_with(&[("1@g.us", "Group 1")]).await;
        let source = Fake::new(&[]);
        fill_group_names(&s, DEV, &source, fast(), || false).await;
        assert!(source.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn contacts_marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ruwa-marker-{}", std::process::id()));
        clear_contacts_marker(&dir);
        assert!(!dir.join(CONTACTS_RESYNCED).exists());
        mark_contacts_resynced(&dir);
        assert!(dir.join(CONTACTS_RESYNCED).exists());
        clear_contacts_marker(&dir);
        assert!(!dir.join(CONTACTS_RESYNCED).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

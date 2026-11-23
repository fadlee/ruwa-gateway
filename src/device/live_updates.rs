//! Newsletter live-updates subscriptions (GOWA `newsletter_live_updates.go`).
//!
//! WhatsApp grants live counter updates for a short, server-chosen time
//! (~90 s), so each subscription is renewed in the background until it is
//! removed, the account changes, or the slot is wiped. Kept in memory only.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use tokio::sync::Notify;

use super::DeviceInstance;
use crate::wa::Jid;

/// GOWA constants.
const RENEW_FRACTION: f64 = 0.8;
const MIN_RENEW_DELAY: Duration = Duration::from_secs(5);
const RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Snapshot of one subscription (unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub newsletter: String,
    pub duration_secs: u64,
    pub expires_at: i64,
    pub renewed_at: i64,
    pub last_error: String,
}

#[derive(Debug, Clone, PartialEq)]
struct State {
    duration_secs: u64,
    expires_at: i64,
    renewed_at: i64,
    last_error: String,
}

struct Entry {
    account: String,
    state: Arc<Mutex<State>>,
    stop: Arc<Notify>,
    kick: Arc<Notify>,
}

#[derive(Default)]
pub struct LiveUpdates {
    subs: Mutex<BTreeMap<String, Entry>>,
}

/// Seconds to wait before renewing: 80 % of the grant, minus what has
/// already elapsed, but never under 5 s.
pub(crate) fn next_renew_delay(duration_secs: u64, renewed_at: i64, now: i64) -> Duration {
    let target = Duration::from_secs_f64(duration_secs as f64 * RENEW_FRACTION);
    let elapsed = Duration::from_secs((now - renewed_at).max(0) as u64);
    target.saturating_sub(elapsed).max(MIN_RENEW_DELAY)
}

/// `30s, 60s, 120s, …` capped at 5 min.
pub(crate) fn backoff(failures: u32) -> Duration {
    let mut delay = RETRY_DELAY;
    for _ in 1..failures {
        if delay >= MAX_RETRY_DELAY {
            break;
        }
        delay *= 2;
    }
    delay.min(MAX_RETRY_DELAY)
}

/// Retry soon after a failure, but not past the end of the current grant.
pub(crate) fn retry_delay(failures: u32, expires_at: i64, now: i64) -> Duration {
    let mut delay = backoff(failures);
    if expires_at > now {
        delay = delay.min(Duration::from_secs((expires_at - now) as u64));
    }
    delay.max(MIN_RENEW_DELAY)
}

impl State {
    fn record_success(&mut self, now: i64, duration_secs: u64) {
        self.duration_secs = duration_secs;
        self.renewed_at = now;
        self.expires_at = now + duration_secs as i64;
        self.last_error.clear();
    }

    fn status(&self, jid: &str) -> Status {
        Status {
            newsletter: jid.to_owned(),
            duration_secs: self.duration_secs,
            expires_at: self.expires_at,
            renewed_at: self.renewed_at,
            last_error: self.last_error.clone(),
        }
    }
}

fn account(device: &DeviceInstance) -> Option<String> {
    device
        .client()
        .and_then(|c| c.pn())
        .map(|pn| pn.to_non_ad().to_string())
}

async fn request(device: &DeviceInstance, jid: &Jid) -> Result<u64, String> {
    let client = device
        .client()
        .filter(|c| c.is_socket_connected())
        .ok_or_else(|| "device is not connected".to_owned())?;
    match tokio::time::timeout(
        REQUEST_TIMEOUT,
        client.newsletter().subscribe_live_updates(jid.clone()),
    )
    .await
    {
        Ok(Ok(secs)) => Ok(secs),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("context deadline exceeded".to_owned()),
    }
}

impl LiveUpdates {
    /// Subscribe (or refresh) and keep renewing in the background.
    pub async fn subscribe(
        &self,
        device: &Arc<DeviceInstance>,
        jid: Jid,
    ) -> Result<Status, String> {
        let key = jid.to_string();
        let account_now = account(device).unwrap_or_default();
        let secs = request(device, &jid)
            .await
            .map_err(|e| format!("failed to subscribe to newsletter live updates: {e}"))?;
        if account(device).unwrap_or_default() != account_now {
            return Err("device is logged in as a different account".into());
        }
        let now = crate::device::now();

        let mut subs = self.subs.lock().expect("live updates");
        if let Some(existing) = subs.get(&key) {
            if existing.account == account_now {
                let mut state = existing.state.lock().expect("live state");
                state.record_success(now, secs);
                existing.kick.notify_one();
                return Ok(state.status(&key));
            }
            existing.stop.notify_one();
        }
        let mut state = State {
            duration_secs: 0,
            expires_at: 0,
            renewed_at: 0,
            last_error: String::new(),
        };
        state.record_success(now, secs);
        let status = state.status(&key);
        let entry = Entry {
            account: account_now.clone(),
            state: Arc::new(Mutex::new(state)),
            stop: Arc::new(Notify::new()),
            kick: Arc::new(Notify::new()),
        };
        tokio::spawn(renew_loop(
            Arc::downgrade(device),
            jid,
            account_now,
            entry.state.clone(),
            entry.stop.clone(),
            entry.kick.clone(),
        ));
        subs.insert(key, entry);
        Ok(status)
    }

    /// Stop renewing; `false` when there was no such subscription.
    pub fn unsubscribe(&self, jid: &Jid) -> bool {
        match self
            .subs
            .lock()
            .expect("live updates")
            .remove(&jid.to_string())
        {
            Some(entry) => {
                entry.stop.notify_one();
                true
            }
            None => false,
        }
    }

    pub fn list(&self) -> Vec<Status> {
        self.subs
            .lock()
            .expect("live updates")
            .iter()
            .map(|(jid, e)| e.state.lock().expect("live state").status(jid))
            .collect()
    }

    /// Renew everything now (after a reconnect the server-side grants are gone).
    pub fn renew_all(&self) {
        for entry in self.subs.lock().expect("live updates").values() {
            entry.kick.notify_one();
        }
    }

    /// Drop every subscription (logout, slot removal).
    pub fn stop_all(&self) {
        let mut subs = self.subs.lock().expect("live updates");
        for entry in subs.values() {
            entry.stop.notify_one();
        }
        subs.clear();
    }

    fn remove_if_current(&self, jid: &str, state: &Arc<Mutex<State>>) {
        let mut subs = self.subs.lock().expect("live updates");
        if subs.get(jid).is_some_and(|e| Arc::ptr_eq(&e.state, state))
            && let Some(entry) = subs.remove(jid)
        {
            entry.stop.notify_one();
        }
    }
}

async fn renew_loop(
    device: Weak<DeviceInstance>,
    jid: Jid,
    account_at_start: String,
    state: Arc<Mutex<State>>,
    stop: Arc<Notify>,
    kick: Arc<Notify>,
) {
    let key = jid.to_string();
    let mut failures = 0u32;
    let mut delay = {
        let s = state.lock().expect("live state");
        next_renew_delay(s.duration_secs, s.renewed_at, crate::device::now())
    };
    loop {
        tokio::select! {
            () = stop.notified() => return,
            () = kick.notified() => {}
            () = tokio::time::sleep(delay) => {}
        }
        let Some(device) = device.upgrade() else {
            return;
        };
        if account(&device).is_some_and(|a| a != account_at_start) {
            tracing::warn!(newsletter = %key, "live updates stopped: device is logged in as a different account");
            device.live_updates.remove_if_current(&key, &state);
            return;
        }
        let result = request(&device, &jid).await;
        let now = crate::device::now();
        let mut s = state.lock().expect("live state");
        match result {
            Ok(secs) => {
                failures = 0;
                s.record_success(now, secs);
                delay = next_renew_delay(s.duration_secs, s.renewed_at, now);
            }
            Err(error) => {
                failures += 1;
                delay = retry_delay(failures, s.expires_at, now);
                tracing::warn!(newsletter = %key, %error, ?delay, "live updates renewal failed");
                s.last_error = error;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renews_at_eighty_percent_with_a_floor() {
        assert_eq!(next_renew_delay(90, 1000, 1000), Duration::from_secs(72));
        assert_eq!(next_renew_delay(90, 1000, 1040), Duration::from_secs(32));
        // Already past the target, or a tiny grant: never under 5 s.
        assert_eq!(next_renew_delay(90, 1000, 1200), MIN_RENEW_DELAY);
        assert_eq!(next_renew_delay(2, 1000, 1000), MIN_RENEW_DELAY);
    }

    #[test]
    fn backoff_doubles_to_a_cap() {
        assert_eq!(backoff(1), Duration::from_secs(30));
        assert_eq!(backoff(2), Duration::from_secs(60));
        assert_eq!(backoff(3), Duration::from_secs(120));
        assert_eq!(backoff(5), Duration::from_secs(300));
        assert_eq!(backoff(50), Duration::from_secs(300));
    }

    #[test]
    fn retry_never_waits_past_expiry() {
        assert_eq!(
            retry_delay(1, 1010, 1000),
            MIN_RENEW_DELAY.max(Duration::from_secs(10))
        );
        assert_eq!(retry_delay(1, 1000, 1100), Duration::from_secs(30));
        assert_eq!(retry_delay(1, 1003, 1000), MIN_RENEW_DELAY);
    }

    #[test]
    fn state_tracks_success_and_clears_error() {
        let mut s = State {
            duration_secs: 0,
            expires_at: 0,
            renewed_at: 0,
            last_error: "boom".into(),
        };
        s.record_success(100, 90);
        let st = s.status("1@newsletter");
        assert_eq!(
            (st.renewed_at, st.expires_at, st.duration_secs),
            (100, 190, 90)
        );
        assert!(st.last_error.is_empty());
    }
}

//! Own presence: on-connect state and the periodic "pulse" (GOWA
//! `sendConfiguredPresence`, `presence_pulse.go`).
//!
//! WhatsApp only delivers contact presence / typing while the account is
//! available, but an available linked device suppresses phone notifications,
//! so the pulse marks it available briefly and then unavailable again.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use super::{DeviceInstance, DeviceManager};

const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// `WHATSAPP_PRESENCE_*`.
#[derive(Debug, Clone)]
pub struct PresenceOptions {
    /// `available`, `unavailable` (anything else but `none`) or `none`.
    pub on_connect: String,
    pub pulse_enabled: bool,
    pub pulse_interval: Duration,
    pub pulse_duration: Duration,
}

impl Default for PresenceOptions {
    fn default() -> Self {
        Self {
            on_connect: "unavailable".into(),
            pulse_enabled: false,
            pulse_interval: Duration::from_secs(24 * 3600),
            pulse_duration: Duration::from_secs(300),
        }
    }
}

/// GOWA `resolvePresenceOnConnect`: `None` means do not send anything.
pub fn on_connect_available(setting: &str) -> Option<bool> {
    match setting {
        "available" => Some(true),
        "none" => None,
        _ => Some(false),
    }
}

pub(super) async fn send_on_connect(device: &DeviceInstance, setting: &str) {
    let Some(available) = on_connect_available(setting) else {
        tracing::info!(device = %device.id(), "skipping presence on connect (configured: none)");
        return;
    };
    set(device, available).await;
}

async fn set(device: &DeviceInstance, available: bool) -> bool {
    let Some(client) = device.client() else {
        return false;
    };
    let result = if available {
        client.presence().set_available().await
    } else {
        client.presence().set_unavailable().await
    };
    match result {
        Ok(()) => {
            tracing::info!(device = %device.id(), available, "own presence set");
            true
        }
        Err(error) => {
            tracing::warn!(device = %device.id(), available, %error, "failed to set own presence");
            false
        }
    }
}

/// Which devices are due for a pulse: not already pulsing and at least
/// `interval` since the last successful one.
#[derive(Debug, Default)]
pub(crate) struct PulseClock {
    last: HashMap<String, Instant>,
    in_flight: std::collections::HashSet<String>,
}

impl PulseClock {
    pub fn start_if_due(&mut self, id: &str, now: Instant, interval: Duration) -> bool {
        if self.in_flight.contains(id) {
            return false;
        }
        if self
            .last
            .get(id)
            .is_some_and(|last| now.duration_since(*last) < interval)
        {
            return false;
        }
        self.in_flight.insert(id.to_owned());
        true
    }

    pub fn finish(&mut self, id: &str, succeeded_at: Option<Instant>) {
        self.in_flight.remove(id);
        if let Some(at) = succeeded_at {
            self.last.insert(id.to_owned(), at);
        }
    }
}

/// Start the scheduler: every minute, pulse each connected device that is due.
pub fn start_pulse(manager: Arc<DeviceManager>, opts: PresenceOptions) {
    if !opts.pulse_enabled {
        tracing::info!("presence pulse scheduler disabled");
        return;
    }
    tracing::info!(
        interval = ?opts.pulse_interval,
        duration = ?opts.pulse_duration,
        "presence pulse scheduler started"
    );
    let clock = Arc::new(std::sync::Mutex::new(PulseClock::default()));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(CHECK_INTERVAL);
        loop {
            tick.tick().await;
            for device in manager.list().await {
                if !(device.is_connected() && device.is_logged_in()) {
                    continue;
                }
                let due = clock.lock().expect("pulse clock").start_if_due(
                    device.id(),
                    Instant::now(),
                    opts.pulse_interval,
                );
                if due {
                    tokio::spawn(pulse(device, clock.clone(), opts.pulse_duration));
                }
            }
        }
    });
}

async fn pulse(
    device: Arc<DeviceInstance>,
    clock: Arc<std::sync::Mutex<PulseClock>>,
    duration: Duration,
) {
    let id = device.id().to_owned();
    if !set(&device, true).await {
        clock.lock().expect("pulse clock").finish(&id, None);
        return;
    }
    clock
        .lock()
        .expect("pulse clock")
        .finish(&id, Some(Instant::now()));
    tokio::time::sleep(duration).await;
    if device.is_connected() && device.is_logged_in() {
        set(&device, false).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_connect_setting() {
        assert_eq!(on_connect_available("available"), Some(true));
        assert_eq!(on_connect_available("unavailable"), Some(false));
        assert_eq!(on_connect_available("none"), None);
        assert_eq!(on_connect_available("whatever"), Some(false));
    }

    #[test]
    fn pulse_clock_respects_interval_and_in_flight() {
        let mut c = PulseClock::default();
        let t0 = Instant::now();
        let day = Duration::from_secs(86_400);
        assert!(c.start_if_due("a", t0, day));
        assert!(!c.start_if_due("a", t0, day), "already pulsing");
        // A failed pulse is retried at the next check.
        c.finish("a", None);
        assert!(c.start_if_due("a", t0, day));
        c.finish("a", Some(t0));
        assert!(!c.start_if_due("a", t0 + Duration::from_secs(3600), day));
        assert!(c.start_if_due("a", t0 + day, day));
        // Devices are tracked independently.
        assert!(c.start_if_due("b", t0, day));
    }
}

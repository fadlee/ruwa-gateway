//! Server acks of sent messages, so a send can report a nack.
//!
//! whatsapp-rust returns from `send` once the stanza is written; the server's
//! verdict arrives later as `Event::ServerAck`. whatsmeow (and so GOWA) blocks
//! on it, which is how error 463 reaches the REST caller. We keep the verdicts
//! of recent message ids and let `deliver` wait briefly for its own.

use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Duration,
};

use tokio::sync::oneshot;

/// How many finished verdicts are remembered (an ack can beat the waiter).
const KEEP: usize = 512;

/// Verdict for one message: `None` = acked, `Some(code)` = nacked.
pub type Verdict = Option<String>;

#[derive(Default)]
struct State {
    done: HashMap<String, Verdict>,
    order: VecDeque<String>,
    waiters: HashMap<String, oneshot::Sender<Verdict>>,
}

#[derive(Default)]
pub struct AckBook(Mutex<State>);

impl AckBook {
    pub fn record(&self, id: &str, verdict: Verdict) {
        let mut s = self.0.lock().expect("ack book");
        if let Some(waiter) = s.waiters.remove(id) {
            let _ = waiter.send(verdict);
            return;
        }
        if s.done.insert(id.to_owned(), verdict).is_none() {
            s.order.push_back(id.to_owned());
            if s.order.len() > KEEP
                && let Some(old) = s.order.pop_front()
            {
                s.done.remove(&old);
            }
        }
    }

    /// The verdict for `id`, or `None` if the server did not answer in time.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Option<Verdict> {
        let rx = {
            let mut s = self.0.lock().expect("ack book");
            if let Some(v) = s.done.remove(id) {
                s.order.retain(|o| o != id);
                return Some(v);
            }
            let (tx, rx) = oneshot::channel();
            s.waiters.insert(id.to_owned(), tx);
            rx
        };
        let verdict = tokio::time::timeout(timeout, rx).await;
        if !matches!(verdict, Ok(Ok(_))) {
            self.0.lock().expect("ack book").waiters.remove(id);
        }
        verdict.ok().and_then(Result::ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn ack_before_wait_is_remembered() {
        let b = AckBook::default();
        b.record("a", None);
        b.record("b", Some("463".into()));
        assert_eq!(b.wait("a", T).await, Some(None));
        assert_eq!(b.wait("b", T).await, Some(Some("463".into())));
        // Consumed once.
        assert_eq!(b.wait("a", Duration::from_millis(10)).await, None);
    }

    #[tokio::test]
    async fn waiter_is_woken_by_a_later_ack() {
        let b = std::sync::Arc::new(AckBook::default());
        let waiter = {
            let b = b.clone();
            tokio::spawn(async move { b.wait("x", Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        b.record("x", Some("463".into()));
        assert_eq!(waiter.await.unwrap(), Some(Some("463".into())));
    }

    #[tokio::test]
    async fn timeout_means_no_verdict_and_old_entries_are_evicted() {
        let b = AckBook::default();
        assert_eq!(b.wait("none", Duration::from_millis(10)).await, None);
        for i in 0..(KEEP + 10) {
            b.record(&format!("id{i}"), None);
        }
        assert_eq!(b.wait("id0", Duration::from_millis(10)).await, None);
        assert_eq!(b.wait(&format!("id{}", KEEP + 9), T).await, Some(None));
    }
}

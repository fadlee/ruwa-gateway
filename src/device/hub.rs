//! Dashboard event hub (GOWA `ui/websocket`): every `/ws` subscriber gets
//! every `{code, message, result}` broadcast. Like GOWA it is not scoped to a
//! device; integrations should use webhooks.

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

/// Per-subscriber backlog; a subscriber that falls further behind is
/// disconnected (GOWA drops slow clients) and the dashboard reconnects.
const CAPACITY: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct WsMessage {
    pub code: String,
    pub message: String,
    pub result: Value,
}

#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<String>,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(CAPACITY).0,
        }
    }
}

impl Hub {
    pub fn publish(&self, code: &str, message: impl Into<String>, result: Value) {
        let msg = WsMessage {
            code: code.to_owned(),
            message: message.into(),
            result,
        };
        if let Ok(text) = serde_json::to_string(&msg) {
            // No subscribers is not an error.
            let _ = self.tx.send(text);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fan_out() {
        let hub = Hub::default();
        let mut a = hub.subscribe();
        let mut b = hub.subscribe();
        hub.publish("LOGIN_SUCCESS", "ok", Value::Null);
        let expected = r#"{"code":"LOGIN_SUCCESS","message":"ok","result":null}"#;
        assert_eq!(a.recv().await.unwrap(), expected);
        assert_eq!(b.recv().await.unwrap(), expected);
    }
}

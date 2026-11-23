//! `/ws` dashboard events (GOWA `ui/websocket`). Device-scoped like in GOWA
//! (the dashboard passes `?device_id=`), but every subscriber receives every
//! broadcast. Browsers authenticate with `?authorization=<base64(user:pass)>`.
//!
//! Client → server: `{"code":"FETCH_DEVICES"}` → broadcast `LIST_DEVICES`.

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::{http::AppState, usecase};

pub fn router() -> Router<AppState> {
    Router::new().route("/ws", get(upgrade))
}

async fn upgrade(
    State(state): State<AppState>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| serve(socket, state)),
        Err(_) => (StatusCode::UPGRADE_REQUIRED, "Upgrade Required").into_response(),
    }
}

#[derive(Deserialize)]
struct Incoming {
    #[serde(default)]
    code: String,
}

async fn serve(mut socket: WebSocket, state: AppState) {
    let hub = state.devices.hub().clone();
    let mut rx = hub.subscribe();
    loop {
        tokio::select! {
            outgoing = rx.recv() => match outgoing {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    tracing::info!("disconnecting slow websocket client");
                    break;
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(msg) = serde_json::from_str::<Incoming>(&text) else { break };
                    if msg.code == "FETCH_DEVICES" {
                        let devices = usecase::app::device_list(&state.devices).await;
                        hub.publish("LIST_DEVICES", "Device found", devices);
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}

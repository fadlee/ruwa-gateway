//! HTTP Basic auth, matching GOWA's `APP_BASIC_AUTH` (Fiber `basicauth`):
//! a failure is a plain-text `401 Unauthorized` with a `WWW-Authenticate`
//! challenge, not a JSON envelope.
//!
//! The WebSocket endpoint also accepts `?authorization=<base64(user:pass)>`,
//! since browsers cannot set headers on WebSocket upgrades.

use axum::{
    extract::{Request, State},
    http::{
        StatusCode,
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use subtle::ConstantTimeEq;

use super::AppState;
use crate::config::Credential;

pub async fn basic_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.credentials.is_empty() {
        return next.run(req).await;
    }

    let from_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .map(str::to_owned);
    let from_query = || {
        if !req.uri().path().ends_with("/ws") {
            return None;
        }
        req.uri().query().and_then(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "authorization")
                .map(|(_, v)| {
                    percent_encoding::percent_decode_str(v)
                        .decode_utf8_lossy()
                        .into_owned()
                })
        })
    };

    let token = from_header.or_else(from_query);
    if token
        .as_deref()
        .is_some_and(|t| is_valid(t, &state.credentials))
    {
        next.run(req).await
    } else {
        unauthorized()
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            WWW_AUTHENTICATE,
            r#"Basic realm="Restricted", charset="UTF-8""#,
        )],
        "Unauthorized",
    )
        .into_response()
}

fn is_valid(token: &str, creds: &[Credential]) -> bool {
    let Ok(decoded) = STANDARD.decode(token.trim()) else {
        return false;
    };
    let Some((user, pass)) = std::str::from_utf8(&decoded)
        .ok()
        .and_then(|s| s.split_once(':'))
    else {
        return false;
    };
    // GOWA looks the user up, then compares the password in constant time.
    creds
        .iter()
        .find(|c| c.user == user)
        .is_some_and(|c| bool::from(c.pass.as_bytes().ct_eq(pass.as_bytes())))
}

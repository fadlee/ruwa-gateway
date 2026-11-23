//! HTTP Basic auth, matching GOWA's `APP_BASIC_AUTH`.
//! WebSocket clients may pass `?authorization=<base64(user:pass)>` instead,
//! since browsers cannot set headers on WebSocket upgrades.

use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use subtle::ConstantTimeEq;

use super::{AppState, response::ApiError};
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
        req.uri().query().and_then(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "authorization")
                .map(|(_, v)| v.to_owned())
        })
    };

    let token = from_header.or_else(from_query);
    if token
        .as_deref()
        .is_some_and(|t| is_valid(t, &state.credentials))
    {
        next.run(req).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

fn is_valid(token: &str, creds: &[Credential]) -> bool {
    let Ok(decoded) = STANDARD.decode(token.trim()) else {
        return false;
    };
    // Compare against every credential to keep timing independent of position.
    creds.iter().fold(false, |ok, c| {
        let expected = format!("{}:{}", c.user, c.pass);
        ok | bool::from(expected.as_bytes().ct_eq(&decoded))
    })
}

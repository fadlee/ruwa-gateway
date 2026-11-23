//! `/chats` and `/chat/{chat_jid}/*` (GOWA `ui/rest/chat.go`).
//!
//! Unlike `/send/*`, a body that cannot be bound is a 400 `BAD_REQUEST`
//! `Invalid request body` here, and query values that do not parse fall
//! back to their defaults (Fiber `Query[T]`).

use std::collections::HashMap;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::{Path, Query, State, rejection::PathRejection},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::Response,
    routing::{get, post},
};
use serde::de::DeserializeOwned;

use crate::{
    error::{ApiError, ApiResult},
    http::{AppState, device_scope::Device, response},
    usecase::chat::{self, ChatMessagesRequest, ListChatsRequest},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/chats", get(list_chats))
        .route("/chat/{chat_jid}/messages", get(messages))
        .route("/chat/{chat_jid}/pin", post(pin))
        .route("/chat/{chat_jid}/disappearing", post(disappearing))
        .route("/chat/{chat_jid}/archive", post(archive))
        .route("/chat/{chat_jid}/history", post(history))
}

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
}

/// Percent-decoded `chat_jid` (GOWA `chatJIDParam`).
fn chat_jid(path: Result<Path<String>, PathRejection>) -> ApiResult<String> {
    path.map(|Path(jid)| jid).map_err(|e| {
        bad_request(format!(
            "invalid chat_jid path parameter: {}",
            e.body_text()
        ))
    })
}

/// Fiber `Bind().Body`: JSON or url-encoded form; anything else, including
/// an empty body, is refused.
fn bind<T: DeserializeOwned>(headers: &HeaderMap, body: &Bytes) -> ApiResult<T> {
    let invalid = || bad_request("Invalid request body");
    if body.is_empty() {
        return Err(invalid());
    }
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.starts_with("application/x-www-form-urlencoded") {
        serde_html_form::from_bytes(body).map_err(|_| invalid())
    } else {
        serde_json::from_slice(body).map_err(|_| invalid())
    }
}

/// Fiber `Query[int]`: the default when absent or not a number.
fn q_int(q: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    q.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Go `strconv.ParseBool`.
fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Fiber `Query[bool]` with a default.
fn q_bool(q: &HashMap<String, String>, key: &str) -> bool {
    q.get(key).and_then(|v| parse_bool(v)).unwrap_or(false)
}

/// A present, non-empty value parsed as bool (unparseable → false), else `None`.
fn q_opt_bool(q: &HashMap<String, String>, key: &str) -> Option<bool> {
    q.get(key)
        .filter(|v| !v.is_empty())
        .map(|v| parse_bool(v).unwrap_or(false))
}

fn q_str(q: &HashMap<String, String>, key: &str) -> String {
    q.get(key).cloned().unwrap_or_default()
}

async fn list_chats(
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let req = ListChatsRequest {
        limit: q_int(&q, "limit", 25),
        offset: q_int(&q, "offset", 0),
        search: q_str(&q, "search"),
        has_media: q_bool(&q, "has_media"),
        archived: q_opt_bool(&q, "archived"),
    };
    let res = chat::list_chats(&device, req).await?;
    Ok(response::success("Success get chat list", res))
}

async fn messages(
    Extension(Device(device)): Extension<Device>,
    path: Result<Path<String>, PathRejection>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    let req = ChatMessagesRequest {
        chat_jid: chat_jid(path)?,
        limit: q_int(&q, "limit", 50),
        offset: q_int(&q, "offset", 0),
        start_time: q.get("start_time").filter(|v| !v.is_empty()).cloned(),
        end_time: q.get("end_time").filter(|v| !v.is_empty()).cloned(),
        media_only: q_bool(&q, "media_only"),
        is_from_me: q_opt_bool(&q, "is_from_me"),
        search: q_str(&q, "search"),
    };
    let res = chat::chat_messages(&device, req).await?;
    Ok(response::success("Success get chat messages", res))
}

async fn pin(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let jid = chat_jid(path)?;
    let req = bind(&headers, &body)?;
    let res = chat::pin(&device, &jid, req, state.settings.account_validation).await?;
    Ok(response::success(res.message.clone(), res))
}

async fn disappearing(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let jid = chat_jid(path)?;
    let req = bind(&headers, &body)?;
    let res = chat::set_disappearing(&device, &jid, req, state.settings.account_validation).await?;
    Ok(response::success(res.message.clone(), res))
}

async fn archive(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let jid = chat_jid(path)?;
    let req = bind(&headers, &body)?;
    let res = chat::archive(&device, &jid, req, state.settings.account_validation).await?;
    Ok(response::success(res.message.clone(), res))
}

async fn history(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let jid = chat_jid(path)?;
    // The body is optional here: an empty one uses the default count.
    let req = if body.is_empty() {
        Default::default()
    } else {
        bind(&headers, &body)?
    };
    let res = chat::request_history(&device, &jid, req, state.settings.account_validation).await?;
    Ok(response::success(
        "History sync requested successfully",
        res,
    ))
}

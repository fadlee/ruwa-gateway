//! `/group/*` endpoints (GOWA `ui/rest/group.go`).

use std::{collections::HashMap, sync::LazyLock};

use axum::{
    Extension, Router,
    extract::{Query, State},
    http::{
        StatusCode,
        header::{CONTENT_DISPOSITION, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use regex::Regex;
use serde::Deserialize;

use crate::{
    error::{ApiError, ApiResult},
    http::{
        AppState,
        device_scope::Device,
        extract::{Body, Payload},
        response,
    },
    media,
    usecase::group::{
        self as g, Action, AnnounceRequest, CreateRequest, JoinRequest, LeaveRequest,
        LockedRequest, NameRequest, ParticipantRequest, PhotoResponse, RequestAction, TopicRequest,
    },
    wa::jid::sanitize_phone,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/group", post(create))
        .route("/group/join-with-link", post(join_with_link))
        .route("/group/info-from-link", get(info_from_link))
        .route("/group/info", get(info))
        .route("/group/leave", post(leave))
        .route("/group/participants", get(participants).post(add))
        .route("/group/participants/export", get(export))
        .route("/group/participants/remove", post(remove))
        .route("/group/participants/promote", post(promote))
        .route("/group/participants/demote", post(demote))
        .route("/group/participant-requests", get(requests))
        .route("/group/participant-requests/approve", post(approve))
        .route("/group/participant-requests/reject", post(reject))
        .route("/group/photo", post(photo))
        .route("/group/name", post(name))
        .route("/group/locked", post(locked))
        .route("/group/announce", post(announce))
        .route("/group/topic", post(topic))
        .route("/group/invite-link", get(invite_link))
}

type Q = Query<HashMap<String, String>>;

fn q_str(q: &HashMap<String, String>, key: &str) -> String {
    q.get(key).cloned().unwrap_or_default()
}

/// A sanitised `group_id` query value.
fn q_group(q: &HashMap<String, String>) -> String {
    sanitize_phone(&q_str(q, "group_id"))
}

/// The handlers that reject an empty `group_id` themselves (before login).
fn require_group_id(group_id: &str) -> ApiResult<()> {
    if group_id.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_GROUP_ID",
            "Group ID cannot be empty",
        ));
    }
    Ok(())
}

fn av(state: &AppState) -> bool {
    state.settings.account_validation
}

async fn create(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<CreateRequest>,
) -> ApiResult<Response> {
    let id = g::create(&device, req).await?;
    Ok(response::success(
        format!("Success created group with id {id}"),
        serde_json::json!({ "group_id": id }),
    ))
}

async fn join_with_link(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<JoinRequest>,
) -> ApiResult<Response> {
    let id = g::join_with_link(&device, req).await?;
    Ok(response::success(
        "Success joined group",
        serde_json::json!({ "group_id": id }),
    ))
}

async fn info_from_link(
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let res = g::info_from_link(&device, &q_str(&q, "link")).await?;
    Ok(response::success("Success get group info from link", res))
}

async fn info(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let res = g::info(&device, &q_group(&q), av(&state)).await?;
    Ok(response::success("Success get group info", res))
}

async fn leave(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(mut req): Body<LeaveRequest>,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    g::leave(&device, req, av(&state)).await?;
    Ok(response::success(
        "Success leave group",
        serde_json::Value::Null,
    ))
}

async fn participants(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let group_id = q_group(&q);
    require_group_id(&group_id)?;
    let res = g::participants(&device, &group_id, av(&state)).await?;
    Ok(response::success("Success getting group participants", res))
}

/// GOWA `formulaAfterSeparator`.
static FORMULA_AFTER_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([;,\t\r\n])( *)([=+\-@\x{FF1D}\x{FF0B}\x{FF0D}\x{FF20}])").expect("static regex")
});

/// GOWA `spreadsheetSafeCell`: neutralise cells a spreadsheet would run as a
/// formula (including after a separator or line break inside the cell).
fn spreadsheet_safe_cell(value: &str) -> String {
    let trimmed = value.trim_start_matches([' ', '\t', '\r', '\n']);
    let safe = FORMULA_AFTER_SEPARATOR
        .replace_all(value, "${1}'${2}${3}")
        .into_owned();
    match trimmed.chars().next() {
        Some('=' | '+' | '-' | '@' | '\u{FF1D}' | '\u{FF0B}' | '\u{FF0D}' | '\u{FF20}') => {
            format!("'{safe}")
        }
        _ => safe,
    }
}

/// Go `encoding/csv` field quoting.
fn csv_field(field: &str) -> String {
    let needs_quotes =
        field == r"\." || field.contains([',', '"', '\r', '\n']) || field.starts_with([' ', '\t']);
    if !needs_quotes {
        return field.to_owned();
    }
    format!("\"{}\"", field.replace('"', "\"\""))
}

fn csv_row(fields: &[&str]) -> String {
    let mut line = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    line.push('\n');
    line
}

async fn export(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let group_id = q_group(&q);
    require_group_id(&group_id)?;
    let res = g::participants(&device, &group_id, av(&state)).await?;

    let mut csv = csv_row(&[
        "participant_jid",
        "phone_number",
        "lid",
        "display_name",
        "role",
    ]);
    for p in &res.participants {
        let role = if p.is_super_admin {
            "super_admin"
        } else if p.is_admin {
            "admin"
        } else {
            "member"
        };
        csv.push_str(&csv_row(&[
            &spreadsheet_safe_cell(&p.jid),
            &spreadsheet_safe_cell(&p.phone_number),
            &spreadsheet_safe_cell(&p.lid),
            &spreadsheet_safe_cell(&p.display_name),
            &spreadsheet_safe_cell(role),
        ]));
    }
    let file_name = format!("group-{}-participants.csv", res.group_id.replace('@', "_"));
    Ok((
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file_name}\""),
            ),
        ],
        csv,
    )
        .into_response())
}

async fn manage(
    state: &AppState,
    device: &crate::device::DeviceInstance,
    mut req: ParticipantRequest,
    action: Action,
    message: &'static str,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let res = g::manage_participants(device, req, action, av(state)).await?;
    Ok(response::success(message, res))
}

async fn add(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage(
        &state,
        &device,
        req,
        Action::Add,
        "Success add participants",
    )
    .await
}

async fn remove(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage(
        &state,
        &device,
        req,
        Action::Remove,
        "Success delete participants",
    )
    .await
}

async fn promote(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage(
        &state,
        &device,
        req,
        Action::Promote,
        "Success promote participants",
    )
    .await
}

async fn demote(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage(
        &state,
        &device,
        req,
        Action::Demote,
        "Success demote participants",
    )
    .await
}

async fn requests(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let group_id = q_group(&q);
    require_group_id(&group_id)?;
    let res = g::join_requests(&device, &group_id, av(&state)).await?;
    Ok(response::success(
        "Success getting list requested participants",
        res,
    ))
}

async fn manage_requests(
    state: &AppState,
    device: &crate::device::DeviceInstance,
    mut req: ParticipantRequest,
    action: RequestAction,
    message: &'static str,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let res = g::manage_join_requests(device, req, action, av(state)).await?;
    Ok(response::success(message, res))
}

async fn approve(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage_requests(
        &state,
        &device,
        req,
        RequestAction::Approve,
        "Success approve requested participants",
    )
    .await
}

async fn reject(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ParticipantRequest>,
) -> ApiResult<Response> {
    manage_requests(
        &state,
        &device,
        req,
        RequestAction::Reject,
        "Success reject requested participants",
    )
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PhotoForm {
    group_id: String,
}

async fn photo(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<PhotoForm>,
) -> ApiResult<Response> {
    let group_id = sanitize_phone(&body.value.group_id);
    let upload = body.file("photo");
    if let Some(u) = &upload {
        media::image::validate_group_photo(&u.content_type, &u.bytes).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "INVALID_IMAGE_FORMAT",
                format!("Image validation failed: {e}"),
            )
        })?;
    }
    let removing = upload.is_none();
    let picture_id = g::set_photo(&device, &group_id, upload, av(&state)).await?;
    let message = if removing {
        "Success remove group photo"
    } else {
        "Success update group photo"
    };
    Ok(response::success(
        message,
        PhotoResponse {
            picture_id,
            message: message.to_owned(),
        },
    ))
}

async fn name(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(mut req): Body<NameRequest>,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let new_name = req.name.clone();
    g::set_name(&device, req, av(&state)).await?;
    Ok(response::success(
        format!("Success update group name to '{new_name}'"),
        serde_json::Value::Null,
    ))
}

async fn locked(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(mut req): Body<LockedRequest>,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let message = if req.locked {
        "Success set group as locked"
    } else {
        "Success set group as unlocked"
    };
    g::set_locked(&device, req, av(&state)).await?;
    Ok(response::success(message, serde_json::Value::Null))
}

async fn announce(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(mut req): Body<AnnounceRequest>,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let message = if req.announce {
        "Success enable announce mode"
    } else {
        "Success disable announce mode"
    };
    g::set_announce(&device, req, av(&state)).await?;
    Ok(response::success(message, serde_json::Value::Null))
}

async fn topic(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(mut req): Body<TopicRequest>,
) -> ApiResult<Response> {
    req.group_id = sanitize_phone(&req.group_id);
    let message = if req.topic.is_empty() {
        "Success remove group topic"
    } else {
        "Success update group topic"
    };
    g::set_topic(&device, req, av(&state)).await?;
    Ok(response::success(message, serde_json::Value::Null))
}

async fn invite_link(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Q,
) -> ApiResult<Response> {
    let reset = matches!(
        q.get("reset").map(String::as_str),
        Some("1" | "t" | "T" | "TRUE" | "true" | "True")
    );
    let res = g::invite_link(&device, &q_group(&q), reset, av(&state)).await?;
    Ok(response::success("Success get group invite link", res))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_cells_are_neutralised() {
        assert_eq!(spreadsheet_safe_cell("=SUM(A1)"), "'=SUM(A1)");
        assert_eq!(spreadsheet_safe_cell("  +1"), "'  +1");
        assert_eq!(spreadsheet_safe_cell("\u{FF1D}x"), "'\u{FF1D}x");
        assert_eq!(spreadsheet_safe_cell("Alice"), "Alice");
        assert_eq!(spreadsheet_safe_cell("a,=b"), "a,'=b");
        assert_eq!(spreadsheet_safe_cell("a\n  @b"), "a\n'  @b");
    }

    #[test]
    fn csv_quoting_follows_go() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field(" lead"), "\" lead\"");
        assert_eq!(csv_row(&["a", "b c"]), "a,b c\n");
    }
}

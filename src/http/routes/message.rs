//! `/message/{message_id}/*` (GOWA `ui/rest/message.go`). Bodies bind like
//! `/send/*`: a body that cannot be bound is a 500.

use std::collections::HashMap;

use axum::{
    Extension, Router,
    extract::{Path, Query, State},
    response::Response,
    routing::{get, post},
};

use crate::{
    error::ApiResult,
    http::{
        AppState,
        device_scope::Device,
        extract::{Body, Payload},
        forwarded::PublicOrigin,
        response,
    },
    usecase::message::{
        self, ForwardRequest, GenericResponse, PhoneRequest, ReactionRequest, UpdateRequest,
    },
    wa::jid,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/message/{message_id}/reaction", post(react))
        .route("/message/{message_id}/revoke", post(revoke))
        .route("/message/{message_id}/delete", post(delete))
        .route("/message/{message_id}/update", post(update))
        .route("/message/{message_id}/read", post(read))
        .route("/message/{message_id}/played", post(played))
        .route("/message/{message_id}/star", post(star))
        .route("/message/{message_id}/unstar", post(unstar))
        .route("/message/{message_id}/forward", post(forward))
        .route("/message/{message_id}/download", get(download))
}

fn ok(res: GenericResponse) -> Response {
    response::success(res.status.clone(), res)
}

fn validation(state: &AppState) -> bool {
    state.settings.account_validation
}

async fn react(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<ReactionRequest>,
) -> ApiResult<Response> {
    Ok(ok(
        message::react(&device, &id, req, validation(&state)).await?
    ))
}

async fn revoke(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    Ok(ok(
        message::revoke(&device, &id, req, validation(&state)).await?
    ))
}

async fn delete(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    message::delete_for_me(&device, &id, req, validation(&state)).await?;
    Ok(response::success("Message deleted successfully", ()))
}

async fn update(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<UpdateRequest>,
) -> ApiResult<Response> {
    Ok(ok(
        message::update(&device, &id, req, validation(&state)).await?
    ))
}

async fn read(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    Ok(ok(message::mark_read(
        &device,
        &id,
        req,
        validation(&state),
    )
    .await?))
}

async fn played(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    Ok(ok(message::mark_played(
        &device,
        &id,
        req,
        validation(&state),
    )
    .await?))
}

async fn star(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    message::star(&device, &id, req, true, validation(&state)).await?;
    Ok(response::success("Starred message successfully", ()))
}

async fn unstar(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Body(req): Body<PhoneRequest>,
) -> ApiResult<Response> {
    message::star(&device, &id, req, false, validation(&state)).await?;
    Ok(response::success("Unstarred message successfully", ()))
}

async fn forward(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    body: Payload<ForwardRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    req.message_id = id.clone();
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        // Create-time checks (storage first, like GOWA), then store; the
        // validation stops at a dry-run marker instead of sending.
        let check = async {
            message::validate_scheduled_forward(&device, &req).await?;
            Err(crate::usecase::send::dry_run_ok())
        };
        return super::send::schedule(
            &state,
            &device,
            "forward",
            &req.phone,
            "Forwarded message",
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(
        message::forward(&device, &id, req, validation(&state)).await?
    ))
}

/// Go `url.PathEscape` (path segment): unreserved and `$&+,:;=@` stay.
const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b',')
    .remove(b':')
    .remove(b';')
    .remove(b'=')
    .remove(b'@');

/// GOWA `publicStaticPath`: `/statics/...` for a file under `statics/`.
pub(super) fn public_static_path(file_path: &str) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    let rel = normalized.strip_prefix("statics/")?;
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return None;
    }
    let escaped: Vec<String> = parts
        .iter()
        .map(|p| percent_encoding::utf8_percent_encode(p, PATH_SEGMENT).to_string())
        .collect();
    Some(format!("/statics/{}", escaped.join("/")))
}

async fn download(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    origin: PublicOrigin,
) -> ApiResult<Response> {
    let phone = q.get("phone").cloned().unwrap_or_default();
    let media_dir = state.devices.options().media_dir.clone();
    let mut res = message::download(&device, &id, &phone, &media_dir, validation(&state)).await?;
    if res.file_url.is_empty()
        && let Some(path) = public_static_path(&res.file_path)
    {
        res.file_url = origin.url(&state.settings.base_path, &path);
    }
    Ok(response::success(res.status.clone(), res))
}

#[cfg(test)]
mod tests {
    use super::public_static_path;

    #[test]
    fn static_paths() {
        assert_eq!(
            public_static_path("statics/media/628/2026-11-23/1-a b.jpg").as_deref(),
            Some("/statics/media/628/2026-11-23/1-a%20b.jpg")
        );
        assert_eq!(public_static_path("storages/x"), None);
        assert_eq!(public_static_path("statics/../etc"), None);
    }
}

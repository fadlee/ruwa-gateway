//! `/send/*` endpoints (GOWA `ui/rest/send.go`).

use std::{collections::HashMap, future::Future};

use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    response::Response,
    routing::{get, post},
};

use crate::{
    device::DeviceInstance,
    error::{ApiError, ApiResult},
    http::{
        AppState,
        device_scope::Device,
        extract::{Body, Payload},
        response,
    },
    media::{self, Upload},
    usecase::{
        schedule::ListQuery,
        schedule_time::ScheduleOptions,
        send::{self, DRY_RUN, DRY_RUN_CODE, MessageRequest, SendResponse},
        send_media::{
            self as sm, AudioRequest, ChatPresenceRequest, ContactRequest, FileRequest,
            ImageRequest, LinkRequest, LocationRequest, PollRequest, PresenceRequest, SendSettings,
            StickerRequest, VideoRequest,
        },
    },
    wa::jid,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/send/message", post(send_message))
        .route("/send/image", post(send_image))
        .route("/send/file", post(send_file))
        .route("/send/video", post(send_video))
        .route("/send/audio", post(send_audio))
        .route("/send/contact", post(send_contact))
        .route("/send/location", post(send_location))
        .route("/send/link", post(send_link))
        .route("/send/sticker", post(send_sticker))
        .route("/send/poll", post(send_poll))
        .route("/send/presence", post(send_presence))
        .route("/send/chat-presence", post(send_chat_presence))
        .route("/send/schedules", get(schedules_list))
        .route("/send/schedules/{schedule_id}", get(schedule_get))
        .route("/send/schedules/{schedule_id}/pause", post(schedule_pause))
        .route(
            "/send/schedules/{schedule_id}/resume",
            post(schedule_resume),
        )
        .route(
            "/send/schedules/{schedule_id}/cancel",
            post(schedule_cancel),
        )
        // GOWA sets Fiber's BodyLimit to the max video size.
        .layer(DefaultBodyLimit::max(media::MAX_VIDEO_SIZE + 1_000_000))
}

fn settings(state: &AppState) -> SendSettings {
    SendSettings {
        account_validation: state.settings.account_validation,
    }
}

fn ok(res: SendResponse) -> Response {
    let message = res.status.clone();
    response::success(message, res)
}

/// Scheduled request: check the schedule fields, then validate the payload
/// with the real send path (it stops at the login check), then store it.
#[allow(clippy::too_many_arguments)]
async fn schedule<T, F>(
    state: &AppState,
    device: &DeviceInstance,
    kind: &str,
    phone: &str,
    summary: &str,
    opts: &ScheduleOptions,
    payload: &T,
    check: F,
    assets: Vec<(&str, Option<Upload>)>,
) -> ApiResult<Response>
where
    T: serde::Serialize,
    F: Future<Output = ApiResult<SendResponse>>,
{
    state.scheduler.check_options(opts)?;
    match DRY_RUN.scope((), check).await {
        Err(e) if e.code == DRY_RUN_CODE => {}
        Err(e) => return Err(e),
        Ok(_) => {
            return Err(ApiError::internal(
                "scheduled request was sent instead of validated",
            ));
        }
    }
    let assets = assets
        .into_iter()
        .filter_map(|(field, upload)| upload.map(|u| (field.to_owned(), u)))
        .collect();
    let payload = serde_json::to_value(payload).map_err(ApiError::internal)?;
    let res = state
        .scheduler
        .create(device.id(), kind, phone, summary, opts, payload, assets)
        .await?;
    Ok(ok(res))
}

async fn send_message(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<MessageRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    let validation = state.settings.account_validation;
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = send::send_text(&device, req.clone(), validation);
        return schedule(
            &state,
            &device,
            "text",
            &req.phone,
            &req.message,
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(send::send_text(&device, req, validation).await?))
}

async fn send_image(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<ImageRequest>,
) -> ApiResult<Response> {
    let file = body.file("image");
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_image(&device, req.clone(), file.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "image",
            &req.phone,
            &req.caption,
            &opts,
            &req,
            check,
            vec![("image", file)],
        )
        .await;
    }
    Ok(ok(
        sm::send_image(&device, req, file, settings(&state)).await?
    ))
}

async fn send_file(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<FileRequest>,
) -> ApiResult<Response> {
    let file = body.file("file");
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_file(&device, req.clone(), file.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "file",
            &req.phone,
            &req.caption,
            &opts,
            &req,
            check,
            vec![("file", file)],
        )
        .await;
    }
    Ok(ok(
        sm::send_file(&device, req, file, settings(&state)).await?
    ))
}

async fn send_video(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<VideoRequest>,
) -> ApiResult<Response> {
    let file = body.file("video");
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_video(&device, req.clone(), file.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "video",
            &req.phone,
            &req.caption,
            &opts,
            &req,
            check,
            vec![("video", file)],
        )
        .await;
    }
    Ok(ok(
        sm::send_video(&device, req, file, settings(&state)).await?
    ))
}

async fn send_audio(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<AudioRequest>,
) -> ApiResult<Response> {
    let file = body.file("audio");
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_audio(&device, req.clone(), file.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "audio",
            &req.phone,
            "Audio",
            &opts,
            &req,
            check,
            vec![("audio", file)],
        )
        .await;
    }
    Ok(ok(
        sm::send_audio(&device, req, file, settings(&state)).await?
    ))
}

async fn send_contact(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<ContactRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_contact(&device, req.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "contact",
            &req.phone,
            &req.contact_name,
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(sm::send_contact(&device, req, settings(&state)).await?))
}

async fn send_location(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<LocationRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_location(&device, req.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "location",
            &req.phone,
            "Location",
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(sm::send_location(&device, req, settings(&state)).await?))
}

async fn send_link(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<LinkRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_link(&device, req.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "link",
            &req.phone,
            &req.caption,
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(sm::send_link(&device, req, settings(&state)).await?))
}

async fn send_presence(
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<PresenceRequest>,
) -> ApiResult<Response> {
    Ok(ok(sm::send_presence(&device, req).await?))
}

/// Typing indicators are momentary, so they cannot be scheduled.
async fn send_chat_presence(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<ChatPresenceRequest>,
) -> ApiResult<Response> {
    if body.schedule.is_scheduled() {
        return Err(ApiError::validation("chat presence cannot be scheduled"));
    }
    Ok(ok(sm::send_chat_presence(
        &device,
        body.value,
        settings(&state),
    )
    .await?))
}

async fn send_sticker(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<StickerRequest>,
) -> ApiResult<Response> {
    let file = body.file("sticker");
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_sticker(&device, req.clone(), file.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "sticker",
            &req.phone,
            "Sticker",
            &opts,
            &req,
            check,
            vec![("sticker", file)],
        )
        .await;
    }
    Ok(ok(
        sm::send_sticker(&device, req, file, settings(&state)).await?
    ))
}

async fn send_poll(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    body: Payload<PollRequest>,
) -> ApiResult<Response> {
    let (mut req, opts) = (body.value, body.schedule);
    if opts.is_scheduled() {
        req.phone = jid::sanitize_phone(&req.phone);
        let check = sm::send_poll(&device, req.clone(), settings(&state));
        return schedule(
            &state,
            &device,
            "poll",
            &req.phone,
            &req.question,
            &opts,
            &req,
            check,
            vec![],
        )
        .await;
    }
    Ok(ok(sm::send_poll(&device, req, settings(&state)).await?))
}

// ---- /send/schedules ----

async fn schedules_list(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Response> {
    // Fiber's `Query[int]` falls back to the default on a malformed number.
    let int = |key: &str, default: i64| q.get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
    let text = |key: &str| q.get(key).map(|v| v.trim().to_owned()).unwrap_or_default();
    let query = ListQuery {
        status: text("status"),
        search: text("search"),
        message_type: text("message_type"),
        limit: int("limit", 25),
        offset: int("offset", 0),
    };
    let res = state.scheduler.list(device.id(), query).await?;
    Ok(response::success("Schedules fetched", res))
}

async fn schedule_get(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let res = state.scheduler.get(device.id(), &id).await?;
    Ok(response::success("Schedule fetched", res))
}

async fn schedule_pause(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    state.scheduler.pause(device.id(), &id).await?;
    Ok(response::success(
        "Schedule paused",
        serde_json::Value::Null,
    ))
}

async fn schedule_resume(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    state.scheduler.resume(device.id(), &id).await?;
    Ok(response::success(
        "Schedule resumed",
        serde_json::Value::Null,
    ))
}

async fn schedule_cancel(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    state.scheduler.cancel(device.id(), &id).await?;
    Ok(response::success(
        "Schedule cancelled",
        serde_json::Value::Null,
    ))
}

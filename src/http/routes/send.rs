//! `/send/*` endpoints (GOWA `ui/rest/send.go`).

use axum::{
    Extension, Router,
    extract::{DefaultBodyLimit, State},
    response::Response,
    routing::post,
};

use crate::{
    error::ApiResult,
    http::{
        AppState,
        device_scope::Device,
        extract::{Body, Payload},
        response,
    },
    media,
    usecase::{
        send::{self, MessageRequest, SendResponse},
        send_media::{
            self as sm, AudioRequest, ContactRequest, FileRequest, ImageRequest, LinkRequest,
            LocationRequest, SendSettings, VideoRequest,
        },
    },
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

async fn send_message(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<MessageRequest>,
) -> ApiResult<Response> {
    Ok(ok(send::send_text(
        &device,
        req,
        state.settings.account_validation,
    )
    .await?))
}

async fn send_image(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<ImageRequest>,
) -> ApiResult<Response> {
    let file = body.file("image");
    Ok(ok(sm::send_image(
        &device,
        body.value,
        file,
        settings(&state),
    )
    .await?))
}

async fn send_file(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<FileRequest>,
) -> ApiResult<Response> {
    let file = body.file("file");
    Ok(ok(sm::send_file(
        &device,
        body.value,
        file,
        settings(&state),
    )
    .await?))
}

async fn send_video(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<VideoRequest>,
) -> ApiResult<Response> {
    let file = body.file("video");
    Ok(ok(sm::send_video(
        &device,
        body.value,
        file,
        settings(&state),
    )
    .await?))
}

async fn send_audio(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    mut body: Payload<AudioRequest>,
) -> ApiResult<Response> {
    let file = body.file("audio");
    Ok(ok(sm::send_audio(
        &device,
        body.value,
        file,
        settings(&state),
    )
    .await?))
}

async fn send_contact(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<ContactRequest>,
) -> ApiResult<Response> {
    Ok(ok(sm::send_contact(&device, req, settings(&state)).await?))
}

async fn send_location(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<LocationRequest>,
) -> ApiResult<Response> {
    Ok(ok(sm::send_location(&device, req, settings(&state)).await?))
}

async fn send_link(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<LinkRequest>,
) -> ApiResult<Response> {
    Ok(ok(sm::send_link(&device, req, settings(&state)).await?))
}

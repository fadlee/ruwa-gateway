//! `/send/*` endpoints (GOWA `ui/rest/send.go`).

use axum::{
    Extension, Router,
    extract::{FromRequest, Request, State},
    http::header::CONTENT_TYPE,
    response::Response,
    routing::post,
};
use serde::de::DeserializeOwned;

use crate::{
    error::{ApiError, ApiResult},
    http::{AppState, device_scope::Device, response},
    usecase::send::{self, MessageRequest},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/send/message", post(send_message))
}

/// Fiber `Bind().Body`: JSON, or url-encoded/multipart form. A body that
/// cannot be bound panics in GOWA and surfaces as a 500.
pub struct Body<T>(pub T);

impl<T, S> FromRequest<S> for Body<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with("application/x-www-form-urlencoded") {
            let axum::Form(value) = axum::Form::<T>::from_request(req, state)
                .await
                .map_err(|e| ApiError::internal(e.body_text()))?;
            return Ok(Self(value));
        }
        let axum::Json(value) = axum::Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::internal(e.body_text()))?;
        Ok(Self(value))
    }
}

async fn send_message(
    State(state): State<AppState>,
    Extension(Device(device)): Extension<Device>,
    Body(req): Body<MessageRequest>,
) -> ApiResult<Response> {
    let res = send::send_text(&device, req, state.settings.account_validation).await?;
    let message = res.status.clone();
    Ok(response::success(message, res))
}

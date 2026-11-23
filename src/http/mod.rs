//! HTTP layer: router, middleware, GOWA response envelope.
//!
//! Route groups (as in GOWA `cmd/rest.go`):
//! - `/health` — root path, ignores the base path, no auth, plain `OK`
//! - `{base}/statics/*` — QR images and media, no auth
//! - `{base}/app/info` — auth, no device
//! - everything else — auth + device scope (`X-Device-Id` / `?device_id`)

pub mod auth;
pub mod device_scope;
pub mod extract;
pub mod response;
pub mod routes;

use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderName, Method},
    middleware,
    routing::get,
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::ServeDir,
    trace::TraceLayer,
};

use crate::{config::Credential, device::DeviceManager};

#[derive(Debug, Clone)]
pub struct Settings {
    pub account_validation: bool,
    pub os: String,
    pub base_path: String,
    pub cors_allowed_origins: Vec<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub credentials: Arc<Vec<Credential>>,
    pub devices: Arc<DeviceManager>,
    pub settings: Arc<Settings>,
    pub ui: crate::ui::UiAsset,
}

/// Builds the full router.
pub fn router(state: AppState) -> Router {
    let base = state.settings.base_path.clone();
    let statics_dir = state
        .devices
        .options()
        .qr_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| "statics".into());

    let scoped = routes::app::scoped()
        .merge(routes::chat::router())
        .merge(routes::send::router())
        .merge(routes::user::router())
        .merge(routes::ws::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            device_scope::resolve,
        ));

    let authed = Router::new()
        .route("/", get(dashboard))
        .merge(routes::app::unscoped())
        .merge(routes::devices::router())
        .merge(scoped)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::basic_auth,
        ));

    let api = Router::new()
        .nest_service("/statics", ServeDir::new(statics_dir))
        .merge(authed)
        .fallback(response::not_found)
        .with_state(state.clone());

    let api = if base.is_empty() {
        api
    } else {
        Router::new().nest(&base, api)
    };

    Router::new()
        .route("/health", get(|| async { "OK" }))
        .merge(api)
        .layer(cors(&state.settings.cors_allowed_origins))
        .layer(TraceLayer::new_for_http())
}

/// gowa-ui at `{base}/` (behind basic auth, like GOWA).
async fn dashboard(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !state.ui.enabled() {
        return response::not_found().await.into_response();
    }
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        state.ui.content().as_ref().clone(),
    )
        .into_response()
}

/// GOWA CORS: any origin unless `APP_CORS_ALLOWED_ORIGINS` is set.
fn cors(origins: &[String]) -> CorsLayer {
    let origin = if origins
        .iter()
        .all(|o| o.trim().is_empty() || o.trim() == "*")
    {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(origins.iter().filter_map(|o| o.trim().parse().ok()))
    };
    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::HEAD,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            HeaderName::from_static("origin"),
            HeaderName::from_static("content-type"),
            HeaderName::from_static("accept"),
            HeaderName::from_static("authorization"),
            HeaderName::from_static("x-device-id"),
        ])
}

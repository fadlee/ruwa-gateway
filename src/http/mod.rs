//! HTTP layer: router, middleware, GOWA response envelope.

pub mod auth;
pub mod response;
pub mod routes;

use std::sync::Arc;

use axum::{Router, middleware};
use tower_http::trace::TraceLayer;

use crate::config::Credential;

#[derive(Clone)]
pub struct AppState {
    pub credentials: Arc<Vec<Credential>>,
}

/// Builds the full router, nested under the configured base path.
pub fn router(state: AppState, base_path: &str) -> Router {
    let protected = routes::app::router().layer(middleware::from_fn_with_state(
        state.clone(),
        auth::basic_auth,
    ));

    let api = Router::new()
        .merge(routes::health::router())
        .merge(protected)
        .fallback(response::not_found)
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    if base_path.is_empty() {
        api
    } else {
        Router::new().nest(base_path, api)
    }
}

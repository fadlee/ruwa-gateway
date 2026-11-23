use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{
    config::Config,
    http::{self, AppState},
};

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let credentials = cfg.validate()?;
    let state = AppState {
        credentials: Arc::new(credentials),
    };
    let app = http::router(state, &cfg.normalized_base_path());

    let addr = SocketAddr::new(cfg.host, cfg.port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, "ruwa-gateway listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

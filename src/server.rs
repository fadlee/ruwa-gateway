use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{
    config::Config,
    device::{DeviceManager, DeviceOptions},
    http::{self, AppState, Settings},
    storage::{Db, registry::Registry},
    webhook::{Dispatcher, WebhookConfig},
};

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let credentials = cfg.validate()?;

    let webhook = Dispatcher::new(WebhookConfig {
        urls: non_empty(&cfg.webhook),
        secret: cfg.webhook_secret.clone(),
        events: non_empty(&cfg.webhook_events),
        ignore_jids: non_empty(&cfg.webhook_ignore_jids),
        insecure_skip_verify: cfg.webhook_insecure_skip_verify,
    })?;
    let db = Db::open(&cfg.db_uri)?;
    let devices = DeviceManager::bootstrap(
        DeviceOptions {
            root: PathBuf::from("storages/devices"),
            qr_dir: PathBuf::from("statics/qrcode"),
            os: cfg.os.clone(),
        },
        Registry::new(db),
        webhook,
    )
    .await?;

    let state = AppState {
        credentials: Arc::new(credentials),
        devices: devices.clone(),
        settings: Arc::new(Settings {
            account_validation: cfg.account_validation,
            os: cfg.os.clone(),
            base_path: cfg.normalized_base_path(),
            cors_allowed_origins: cfg.cors_allowed_origins.clone(),
        }),
    };
    let app = http::router(state);

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

    for device in devices.list().await {
        device.stop().await;
    }
    Ok(())
}

fn non_empty(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .collect()
}

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use tokio::net::TcpListener;

use crate::{
    config::Config,
    device::{DeviceManager, DeviceOptions, presence::PresenceOptions},
    http::{self, AppState, Settings},
    storage::{Db, schedule::ScheduleStore},
    ui::{UiAsset, UiConfig},
    usecase::{
        schedule::{ScheduleService, Worker},
        schedule_dispatch::DeviceBackend,
        send_media::SendSettings,
    },
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
    let presence_opts = PresenceOptions {
        on_connect: cfg.presence_on_connect.clone(),
        pulse_enabled: cfg.presence_pulse_enabled,
        pulse_interval: cfg.presence_pulse_interval,
        pulse_duration: cfg.presence_pulse_duration,
    };
    let devices = DeviceManager::bootstrap(
        DeviceOptions {
            root: PathBuf::from("storages/devices"),
            qr_dir: PathBuf::from("statics/qrcode"),
            os: cfg.os.clone(),
            media_dir: PathBuf::from("statics/media"),
            auto_download_media: cfg.auto_download_media,
            auto_reject_call: cfg.auto_reject_call,
            auto_mark_read: cfg.auto_mark_read,
            auto_reply: cfg.auto_reply.clone(),
            presence: presence_opts.clone(),
        },
        db.clone(),
        webhook,
    )
    .await?;

    crate::device::presence::start_pulse(devices.clone(), presence_opts);

    let ui = UiAsset::new(UiConfig {
        enabled: cfg.ui_enabled,
        auto_update: cfg.ui_auto_update,
        repo: cfg.ui_repo.clone(),
        asset_name: cfg.ui_asset_name.clone(),
        cache_dir: PathBuf::from("storages/ui"),
    });
    ui.start();

    let scheduler =
        ScheduleService::new(ScheduleStore::new(db), PathBuf::from("storages/scheduled"));
    Arc::new(Worker::new(
        scheduler.clone(),
        DeviceBackend {
            devices: devices.clone(),
            settings: SendSettings {
                account_validation: cfg.account_validation,
            },
        },
    ))
    .start();

    let state = AppState {
        scheduler,
        ui,
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

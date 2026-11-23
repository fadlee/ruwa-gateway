//! Web dashboard (gowa-ui), served at `{base}/` like GOWA v9
//! (`infrastructure/uiasset`): the single-file `gowa-ui.html` from the latest
//! GitHub release, verified against the release's SHA-256 digest and cached
//! as `storages/ui/index.html`. Air-gapped: pre-seed that file and set
//! `APP_UI_AUTO_UPDATE=false`.

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct UiConfig {
    pub enabled: bool,
    pub auto_update: bool,
    pub repo: String,
    pub asset_name: String,
    pub cache_dir: PathBuf,
}

#[derive(Clone)]
pub struct UiAsset {
    config: UiConfig,
    html: Arc<RwLock<Option<Arc<Vec<u8>>>>>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

impl UiAsset {
    pub fn new(config: UiConfig) -> Self {
        Self {
            config,
            html: Arc::new(RwLock::new(None)),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn cache_file(&self) -> PathBuf {
        self.config.cache_dir.join("index.html")
    }

    /// Load the cached copy, then (if allowed) fetch the latest release in
    /// the background.
    pub fn start(&self) {
        if !self.config.enabled {
            return;
        }
        if let Ok(html) = std::fs::read(self.cache_file()) {
            *self.html.write().expect("ui lock") = Some(Arc::new(html));
            tracing::info!("dashboard loaded from cache");
        }
        if self.config.auto_update {
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(error) = this.update().await {
                    tracing::warn!(%error, "dashboard update failed");
                }
            });
        }
    }

    async fn update(&self) -> anyhow::Result<()> {
        let http = reqwest::Client::builder()
            .user_agent("ruwa-gateway")
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let release_body = http
            .get(format!(
                "https://api.github.com/repos/{}/releases/latest",
                self.config.repo
            ))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let release: Release = serde_json::from_slice(&release_body)?;
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == self.config.asset_name)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "release {} has no {}",
                    release.tag_name,
                    self.config.asset_name
                )
            })?;
        let expected = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(str::to_ascii_lowercase);

        if let (Some(expected), Some(current)) =
            (&expected, self.html.read().expect("ui lock").clone())
            && hex::encode(Sha256::digest(current.as_slice())) == *expected
        {
            tracing::info!(tag = %release.tag_name, "dashboard is up to date");
            return Ok(());
        }

        let html = http
            .get(&asset.browser_download_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec();
        let actual = hex::encode(Sha256::digest(&html));
        if let Some(expected) = expected
            && expected != actual
        {
            anyhow::bail!(
                "downloaded dashboard sha256 {actual} does not match release digest {expected}"
            );
        }
        std::fs::create_dir_all(&self.config.cache_dir)?;
        let tmp = self.cache_file().with_extension("tmp");
        std::fs::write(&tmp, &html)?;
        std::fs::rename(&tmp, self.cache_file())?;
        *self.html.write().expect("ui lock") = Some(Arc::new(html));
        tracing::info!(tag = %release.tag_name, "dashboard downloaded");
        Ok(())
    }

    /// The dashboard HTML, or a short explanation while it is unavailable.
    pub fn content(&self) -> Arc<Vec<u8>> {
        if let Some(html) = self.html.read().expect("ui lock").clone() {
            return html;
        }
        Arc::new(
            format!(
                "<!doctype html><meta charset=utf-8><title>ruwa-gateway</title>\
                 <p>The dashboard ({repo}) is not available yet. It is downloaded from GitHub at startup; \
                 refresh in a moment. Offline? Put <code>gowa-ui.html</code> at <code>{path}</code>.</p>",
                repo = self.config.repo,
                path = self.cache_file().display()
            )
            .into_bytes(),
        )
    }
}

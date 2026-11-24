//! Web dashboard (gowa-ui), served at `{base}/` like GOWA v9
//! (`infrastructure/uiasset`): the single-file `gowa-ui.html` from the latest
//! GitHub release, verified against the release's SHA-256 digest and cached
//! as `storages/ui/index.html`. Air-gapped: pre-seed that file and set
//! `APP_UI_AUTO_UPDATE=false`.
//!
//! Updates run in the background: the first check happens at start; after a
//! failure it is retried with a growing delay (so a network that is not up yet
//! does not leave the dashboard missing until the next restart), after a
//! success it repeats every `APP_UI_UPDATE_INTERVAL` ±10%. `APP_UI_ASSET_SHA256`
//! pins the exact build that may be served.

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
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
    /// Pause between successful update checks.
    pub update_interval: Duration,
    /// Sent as a bearer token on GitHub requests when non-empty.
    pub github_token: String,
    /// Lowercase hex SHA-256 the dashboard must match; empty = no pin.
    pub pinned_sha256: String,
    /// GitHub API root (overridable for tests).
    pub api_base: String,
}

/// What one update check did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    UpToDate(String),
    Updated(String),
}

/// First retry after a failed check; doubles up to [`MAX_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(30);
const MAX_RETRY: Duration = Duration::from_secs(30 * 60);

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

    /// Load the cached copy, then (if allowed) keep the dashboard up to date
    /// in the background.
    pub fn start(&self) {
        if !self.config.enabled {
            return;
        }
        if let Ok(html) = std::fs::read(self.cache_file()) {
            let actual = hex::encode(Sha256::digest(&html));
            if self.config.pinned_sha256.is_empty() || actual == self.config.pinned_sha256 {
                *self.html.write().expect("ui lock") = Some(Arc::new(html));
                tracing::info!("dashboard loaded from cache");
            } else {
                tracing::warn!(
                    %actual,
                    pinned = %self.config.pinned_sha256,
                    "cached dashboard does not match APP_UI_ASSET_SHA256; not serving it"
                );
            }
        }
        if self.config.auto_update {
            let this = self.clone();
            tokio::spawn(async move { this.update_loop().await });
        }
    }

    async fn update_loop(&self) {
        let mut failures = 0u32;
        loop {
            let wait = match self.update().await {
                Ok(Outcome::UpToDate(tag)) => {
                    failures = 0;
                    tracing::info!(%tag, "dashboard is up to date");
                    jittered(self.config.update_interval, random_unit())
                }
                Ok(Outcome::Updated(tag)) => {
                    failures = 0;
                    tracing::info!(%tag, "dashboard downloaded");
                    jittered(self.config.update_interval, random_unit())
                }
                Err(error) => {
                    failures += 1;
                    let wait = retry_delay(failures, self.config.update_interval);
                    // `{:#}` prints the whole cause chain (DNS, TLS, timeout, ...);
                    // the top-level reqwest message alone hides the reason.
                    tracing::warn!(
                        error = format!("{error:#}"),
                        retry_in_secs = wait.as_secs(),
                        "dashboard update failed"
                    );
                    wait
                }
            };
            tokio::time::sleep(wait).await;
        }
    }

    fn get(&self, http: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
        let request = http.get(url);
        if self.config.github_token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.config.github_token)
        }
    }

    async fn update(&self) -> anyhow::Result<Outcome> {
        let http = reqwest::Client::builder()
            .user_agent("ruwa-gateway")
            .timeout(Duration::from_secs(60))
            .build()?;
        let release_url = format!(
            "{}/repos/{}/releases/latest",
            self.config.api_base.trim_end_matches('/'),
            self.config.repo
        );
        let release_body = self
            .get(&http, &release_url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| anyhow::Error::new(e).context(format!("GET {release_url}")))?
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
        let pinned = &self.config.pinned_sha256;

        if let Some(expected) = &expected
            && !pinned.is_empty()
            && expected != pinned
        {
            anyhow::bail!(
                "release {} asset digest {expected} does not match the pinned sha256 {pinned}; refusing to download",
                release.tag_name
            );
        }
        if let (Some(expected), Some(current)) =
            (&expected, self.html.read().expect("ui lock").clone())
            && hex::encode(Sha256::digest(current.as_slice())) == *expected
        {
            return Ok(Outcome::UpToDate(release.tag_name));
        }

        let html = self
            .get(&http, &asset.browser_download_url)
            .send()
            .await
            .map_err(|e| {
                anyhow::Error::new(e).context(format!("GET {}", asset.browser_download_url))
            })?
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
        if !pinned.is_empty() && actual != *pinned {
            anyhow::bail!(
                "downloaded dashboard sha256 {actual} does not match the pinned sha256 {pinned}; refusing to serve"
            );
        }
        std::fs::create_dir_all(&self.config.cache_dir)?;
        let tmp = self.cache_file().with_extension("tmp");
        std::fs::write(&tmp, &html)?;
        std::fs::rename(&tmp, self.cache_file())?;
        *self.html.write().expect("ui lock") = Some(Arc::new(html));
        Ok(Outcome::Updated(release.tag_name))
    }

    /// The dashboard HTML, or a short explanation while it is unavailable.
    pub fn content(&self) -> Arc<Vec<u8>> {
        if let Some(html) = self.html.read().expect("ui lock").clone() {
            return html;
        }
        Arc::new(
            format!(
                "<!doctype html><meta charset=utf-8><title>ruwa-gateway</title>\
                 <p>The dashboard ({repo}) is not available yet. It is downloaded from GitHub in the background \
                 (retried automatically); refresh in a moment. Offline? Put <code>gowa-ui.html</code> at <code>{path}</code>.</p>",
                repo = self.config.repo,
                path = self.cache_file().display()
            )
            .into_bytes(),
        )
    }
}

/// Delay before retry number `failures` (1-based): 30s, 1m, 2m, ... capped at
/// 30 minutes and never longer than the regular check interval.
fn retry_delay(failures: u32, interval: Duration) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    let delay = FIRST_RETRY.saturating_mul(1 << shift).min(MAX_RETRY);
    delay.min(interval.max(FIRST_RETRY))
}

/// `interval` scaled by `0.9 + 0.2 * unit` (`unit` in `0..=1`), so several
/// instances do not hit GitHub at the same moment.
fn jittered(interval: Duration, unit: f64) -> Duration {
    interval.mul_f64(0.9 + 0.2 * unit.clamp(0.0, 1.0))
}

fn random_unit() -> f64 {
    (uuid::Uuid::new_v4().as_u128() >> 64) as f64 / u64::MAX as f64
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, extract::State, http::HeaderMap, routing::get};

    use super::*;

    #[test]
    fn retries_back_off_then_cap() {
        let day = Duration::from_secs(86_400);
        let secs = |n| retry_delay(n, day).as_secs();
        assert_eq!(
            [
                secs(1),
                secs(2),
                secs(3),
                secs(4),
                secs(7),
                secs(8),
                secs(50)
            ],
            [30, 60, 120, 240, 1800, 1800, 1800]
        );
        // Never longer than the regular interval...
        assert_eq!(retry_delay(5, Duration::from_secs(300)).as_secs(), 300);
        // ...but never hammering either.
        assert_eq!(retry_delay(1, Duration::from_secs(1)).as_secs(), 30);
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let hour = Duration::from_secs(3600);
        assert_eq!(jittered(hour, 0.0).as_secs(), 3240);
        assert_eq!(jittered(hour, 1.0).as_secs(), 3960);
        assert_eq!(jittered(hour, 5.0).as_secs(), 3960);
        let u = random_unit();
        assert!((0.0..=1.0).contains(&u));
    }

    /// A fake GitHub: release JSON at `/repos/o/r/releases/latest` and the asset at
    /// `/asset`. Records the `Authorization` header of every request.
    struct Fake {
        base: String,
        auth: Arc<Mutex<Vec<Option<String>>>>,
    }

    async fn fake_github(html: &'static str, digest: Option<String>) -> Fake {
        #[derive(Clone)]
        struct S {
            base: Arc<Mutex<String>>,
            html: &'static str,
            digest: Option<String>,
            auth: Arc<Mutex<Vec<Option<String>>>>,
        }
        fn note(s: &S, h: &HeaderMap) {
            let value = h
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            s.auth.lock().unwrap().push(value);
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = S {
            base: Arc::new(Mutex::new(base.clone())),
            html,
            digest,
            auth: Arc::default(),
        };
        let auth = state.auth.clone();
        let app = Router::new()
            .route(
                "/repos/o/r/releases/latest",
                get(|State(s): State<S>, h: HeaderMap| async move {
                    note(&s, &h);
                    let mut asset = serde_json::json!({
                        "name": "gowa-ui.html",
                        "browser_download_url": format!("{}/asset", s.base.lock().unwrap()),
                    });
                    if let Some(d) = &s.digest {
                        asset["digest"] = format!("sha256:{d}").into();
                    }
                    axum::Json(serde_json::json!({ "tag_name": "v9.9.9", "assets": [asset] }))
                }),
            )
            .route(
                "/asset",
                get(|State(s): State<S>, h: HeaderMap| async move {
                    note(&s, &h);
                    s.html
                }),
            )
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Fake { base, auth }
    }

    fn sha(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    fn ui(dir: &tempfile::TempDir, base: &str, token: &str, pin: &str) -> UiAsset {
        UiAsset::new(UiConfig {
            enabled: true,
            auto_update: true,
            repo: "o/r".into(),
            asset_name: "gowa-ui.html".into(),
            cache_dir: dir.path().to_owned(),
            update_interval: Duration::from_secs(3600),
            github_token: token.into(),
            pinned_sha256: pin.into(),
            api_base: base.into(),
        })
    }

    #[tokio::test]
    async fn downloads_verifies_caches_then_reports_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", Some(sha("<p>ui</p>"))).await;
        let ui = ui(&dir, &fake.base, "", "");
        assert_eq!(
            ui.update().await.unwrap(),
            Outcome::Updated("v9.9.9".into())
        );
        assert_eq!(ui.content().as_slice(), b"<p>ui</p>");
        assert_eq!(std::fs::read(ui.cache_file()).unwrap(), b"<p>ui</p>");
        assert_eq!(
            ui.update().await.unwrap(),
            Outcome::UpToDate("v9.9.9".into())
        );
        // The second check read the release only; no second download.
        assert_eq!(fake.auth.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn digest_mismatch_is_rejected_and_nothing_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", Some(sha("something else"))).await;
        let ui = ui(&dir, &fake.base, "", "");
        let error = ui.update().await.unwrap_err().to_string();
        assert!(error.contains("does not match release digest"), "{error}");
        assert!(!ui.cache_file().exists());
        assert!(String::from_utf8_lossy(&ui.content()).contains("not available yet"));
    }

    #[tokio::test]
    async fn pin_refuses_other_builds_before_and_after_download() {
        let good = sha("<p>ui</p>");
        // The release advertises a digest other than the pin: nothing is downloaded.
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", Some(good.clone())).await;
        let error = ui(&dir, &fake.base, "", &sha("pinned"))
            .update()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to download"), "{error}");
        assert_eq!(fake.auth.lock().unwrap().len(), 1);

        // No digest advertised: the downloaded bytes are checked against the pin.
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", None).await;
        let error = ui(&dir, &fake.base, "", &sha("pinned"))
            .update()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to serve"), "{error}");
        assert!(!dir.path().join("index.html").exists());

        // A matching pin is accepted.
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", None).await;
        let ui = ui(&dir, &fake.base, "", &good);
        assert_eq!(
            ui.update().await.unwrap(),
            Outcome::Updated("v9.9.9".into())
        );
    }

    #[tokio::test]
    async fn github_token_is_sent_as_bearer_only_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", Some(sha("<p>ui</p>"))).await;
        ui(&dir, &fake.base, "tok", "").update().await.unwrap();
        assert_eq!(
            *fake.auth.lock().unwrap(),
            vec![Some("Bearer tok".to_owned()); 2]
        );

        let dir = tempfile::tempdir().unwrap();
        let fake = fake_github("<p>ui</p>", Some(sha("<p>ui</p>"))).await;
        ui(&dir, &fake.base, "", "").update().await.unwrap();
        assert_eq!(*fake.auth.lock().unwrap(), vec![None, None]);
    }

    #[tokio::test]
    async fn a_cached_copy_that_breaks_the_pin_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<p>old</p>").unwrap();
        let mut config = ui(&dir, "http://127.0.0.1:1", "", &sha("<p>new</p>")).config;
        config.auto_update = false;
        let pinned = UiAsset::new(config.clone());
        pinned.start();
        assert!(String::from_utf8_lossy(&pinned.content()).contains("not available yet"));

        config.pinned_sha256 = sha("<p>old</p>");
        let ok = UiAsset::new(config);
        ok.start();
        assert_eq!(ok.content().as_slice(), b"<p>old</p>");
    }

    #[tokio::test]
    async fn failures_name_the_url_and_the_cause() {
        let dir = tempfile::tempdir().unwrap();
        // Nothing listens on port 1.
        let error = ui(&dir, "http://127.0.0.1:1", "", "")
            .update()
            .await
            .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains("GET http://127.0.0.1:1/repos/o/r/releases/latest"),
            "{text}"
        );
        assert!(text.contains("error sending request"), "{text}");
        // The chain goes deeper than the top-level reqwest message.
        assert!(text.matches(": ").count() >= 2, "{text}");
    }
}

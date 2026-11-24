//! Scheduled and recurring sends (GOWA `usecase/schedule.go`).
//!
//! [`ScheduleService`] is what the HTTP layer talks to (create, list, get,
//! pause, resume, cancel). [`Worker`] is the background loop that claims due
//! rows, sends them through a [`Backend`], and records the outcome. Rows live
//! in `scheduled_sends`; a claim is a lease, so a crash leaves the row to be
//! recovered, and the message id of an attempt is pinned so a retry never
//! becomes a second message.

use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{
    schedule_time::{ScheduleOptions, ScheduleSpec, next_occurrence, parse_options},
    send::{Pinned, SendResponse},
};
use crate::{
    error::{ApiError, ApiResult},
    events::rfc3339,
    media::Upload,
    storage::schedule::{
        ACTIVE, CANCELLED, COMPLETED, FAILED, PAUSED, RUNNING, ScheduleStore, ScheduledSend,
    },
};

/// Attempts an occurrence gets before it is skipped (recurring) or failed.
pub const MAX_ATTEMPTS: i32 = 10;
const LEASE_SECS: i64 = 120;
const TICK: Duration = Duration::from_secs(5);
const OFFLINE_RETRY_SECS: i64 = 30;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub const MESSAGE_TYPES: [&str; 11] = [
    "text", "image", "file", "video", "audio", "sticker", "contact", "link", "location", "poll",
    "forward",
];
const STATUSES: [&str; 6] = [ACTIVE, RUNNING, PAUSED, COMPLETED, FAILED, CANCELLED];

/// Why a dispatch did not send.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// The stored job itself is bad (validation, payload, media): retrying cannot help.
    Permanent(String),
    /// Offline device, network, WhatsApp error: worth another try.
    Transient(String),
}

/// What the worker needs from the outside world.
pub trait Backend: Send + Sync + 'static {
    /// Is the device connected and logged in?
    fn device_ready(&self, device_id: &str) -> impl Future<Output = bool> + Send;
    /// A fresh WhatsApp message id for a new occurrence.
    fn new_message_id(&self, device_id: &str) -> impl Future<Output = String> + Send;
    /// Send the occurrence under its pinned identity; returns the message id.
    fn dispatch(
        &self,
        job: &ScheduledSend,
        pinned: Pinned,
    ) -> impl Future<Output = Result<String, Failure>> + Send;
}

#[derive(Clone)]
pub struct ScheduleService {
    store: ScheduleStore,
    assets_root: PathBuf,
    now: Clock,
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub status: String,
    pub search: String,
    pub message_type: String,
    pub limit: i64,
    pub offset: i64,
}

fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(secs, 0).unwrap_or_default()
}

impl ScheduleService {
    pub fn new(store: ScheduleStore, assets_root: PathBuf) -> Self {
        Self::with_clock(store, assets_root, Arc::new(crate::device::now))
    }

    pub fn with_clock(store: ScheduleStore, assets_root: PathBuf, now: Clock) -> Self {
        Self {
            store,
            assets_root,
            now,
        }
    }

    fn now(&self) -> i64 {
        (self.now)()
    }

    pub fn store(&self) -> &ScheduleStore {
        &self.store
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        self.assets_root.join(id)
    }

    fn cleanup_assets(&self, id: &str) {
        let _ = std::fs::remove_dir_all(self.job_dir(id));
    }

    /// Persist an uploaded file under `<root>/<id>/<field>-<filename>`.
    async fn persist_assets(
        &self,
        id: &str,
        inputs: Vec<(String, Upload)>,
    ) -> ApiResult<BTreeMap<String, Value>> {
        let mut meta = BTreeMap::new();
        for (field, upload) in inputs {
            let name = &upload.filename;
            if name.is_empty() || name.contains(['/', '\\']) || name == ".." || name == "." {
                return Err(ApiError::internal("invalid uploaded filename"));
            }
            let dir = self.job_dir(id);
            tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                ApiError::internal(format!("create scheduled media directory: {e}"))
            })?;
            let path = dir.join(format!("{field}-{name}"));
            tokio::fs::write(&path, &upload.bytes)
                .await
                .map_err(|e| ApiError::internal(format!("persist uploaded media: {e}")))?;
            meta.insert(
                field,
                json!({
                    "path": path.to_string_lossy(),
                    "filename": upload.filename,
                    "content_type": upload.content_type,
                }),
            );
        }
        Ok(meta)
    }

    /// Validate only the schedule fields (GOWA runs this before the payload checks).
    pub fn check_options(&self, options: &ScheduleOptions) -> ApiResult<()> {
        parse_options(options, ts(self.now())).map(|_| ())
    }

    /// Store a validated request as a scheduled send (GOWA `Create`).
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        device_id: &str,
        message_type: &str,
        phone: &str,
        summary: &str,
        options: &ScheduleOptions,
        payload: Value,
        assets: Vec<(String, Upload)>,
    ) -> ApiResult<SendResponse> {
        let now = self.now();
        let spec = parse_options(options, ts(now))?
            .ok_or_else(|| ApiError::validation("scheduled_at is required"))?;
        if device_id.is_empty() {
            return Err(ApiError::internal("device identification required"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let assets_meta = match self.persist_assets(&id, assets).await {
            Ok(m) => m,
            Err(e) => {
                self.cleanup_assets(&id);
                return Err(e);
            }
        };
        let first = spec.scheduled_at.timestamp();
        let job = ScheduledSend {
            id: id.clone(),
            device_id: device_id.to_owned(),
            message_type: message_type.to_owned(),
            payload_json: payload.to_string(),
            assets_json: json!(assets_meta).to_string(),
            phone: phone.to_owned(),
            summary: summary.to_owned(),
            scheduled_at: first,
            next_run_at: first,
            timezone: spec.timezone.clone(),
            recurrence: spec.recurrence.clone(),
            weekdays_json: json!(spec.weekdays).to_string(),
            day_of_month: spec.day_of_month as i32,
            end_at: spec.end_at.map(|t| t.timestamp()),
            occurrence_limit: spec.occurrence_limit as i32,
            occurrence_count: 0,
            attempts: 0,
            status: ACTIVE.to_owned(),
            lease_token: String::new(),
            lease_until: None,
            last_run_at: None,
            last_message_id: String::new(),
            last_error: String::new(),
            created_at: now,
            updated_at: now,
            pending_message_id: String::new(),
            pending_poll_secret: None,
        };
        if let Err(error) = self.store.create(job).await {
            self.cleanup_assets(&id);
            return Err(ApiError::internal(format!(
                "create scheduled send: {error}"
            )));
        }
        Ok(SendResponse {
            status: "Message scheduled".into(),
            schedule_id: id,
            scheduled_at: rfc3339(first),
            next_run_at: rfc3339(first),
            ..Default::default()
        })
    }

    async fn load(&self, device_id: &str, id: &str) -> ApiResult<ScheduledSend> {
        if device_id.trim().is_empty() {
            return Err(ApiError::internal("device identification required"));
        }
        self.store
            .get(device_id, id)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("scheduled send not found"))
    }

    pub async fn list(&self, device_id: &str, mut q: ListQuery) -> ApiResult<Value> {
        if q.limit == 0 {
            q.limit = 25;
        }
        let (status, kind) = (q.status.trim(), q.message_type.trim());
        let mut errors = BTreeMap::new();
        if q.limit < 1 {
            errors.insert("limit", "must be no less than 1".to_owned());
        } else if q.limit > 100 {
            errors.insert("limit", "must be no greater than 100".to_owned());
        }
        if q.offset < 0 {
            errors.insert("offset", "must be no less than 0".to_owned());
        }
        if !status.is_empty() && !STATUSES.contains(&status) {
            errors.insert("status", "must be a valid value".to_owned());
        }
        if !kind.is_empty() && !MESSAGE_TYPES.contains(&kind) {
            errors.insert("message_type", "must be a valid value".to_owned());
        }
        super::chat::ozzo(errors)?;
        let filter = crate::storage::schedule::Filter {
            device_id: device_id.to_owned(),
            status: status.to_owned(),
            search: q.search.trim().to_owned(),
            message_type: kind.to_owned(),
            limit: q.limit,
            offset: q.offset,
        };
        let total = self
            .store
            .count(filter.clone())
            .await
            .map_err(ApiError::internal)?;
        let jobs = self.store.list(filter).await.map_err(ApiError::internal)?;
        Ok(json!({
            "data": jobs.iter().map(view).collect::<Vec<_>>(),
            "pagination": { "limit": q.limit, "offset": q.offset, "total": total },
        }))
    }

    pub async fn get(&self, device_id: &str, id: &str) -> ApiResult<Value> {
        Ok(view(&self.load(device_id, id).await?))
    }

    async fn change_status(
        &self,
        job: &ScheduledSend,
        action: &str,
        from: &[&str],
        status: &str,
        next_run_at: Option<i64>,
    ) -> ApiResult<()> {
        let changed = self
            .store
            .set_status(&job.device_id, &job.id, from, status, next_run_at)
            .await
            .map_err(ApiError::internal)?;
        if changed {
            Ok(())
        } else {
            Err(ApiError::validation(format!(
                "scheduled send cannot be {action} while {}",
                job.status
            )))
        }
    }

    pub async fn pause(&self, device_id: &str, id: &str) -> ApiResult<()> {
        let job = self.load(device_id, id).await?;
        self.change_status(&job, "paused", &[ACTIVE], PAUSED, None)
            .await
    }

    /// An overdue one-time job sends right away; an overdue recurring job
    /// continues at its next slot, or completes when none is left.
    pub async fn resume(&self, device_id: &str, id: &str) -> ApiResult<()> {
        let job = self.load(device_id, id).await?;
        let now = self.now();
        let mut next = job.next_run_at;
        if next <= now {
            next = now;
            if job.recurrence != "once" {
                match next_run(&job, job.occurrence_count, now) {
                    None => {
                        self.change_status(&job, "resumed", &[PAUSED], COMPLETED, None)
                            .await?;
                        self.cleanup_assets(&job.id);
                        return Ok(());
                    }
                    Some(slot) => {
                        next = slot;
                        // The missed occurrence may have reached WhatsApp under
                        // its pinned id; the next one must not be its duplicate.
                        self.store
                            .clear_pending_message_id(&job.device_id, &job.id)
                            .await
                            .map_err(ApiError::internal)?;
                    }
                }
            }
        }
        self.change_status(&job, "resumed", &[PAUSED], ACTIVE, Some(next))
            .await
    }

    pub async fn cancel(&self, device_id: &str, id: &str) -> ApiResult<()> {
        let job = self.load(device_id, id).await?;
        self.change_status(
            &job,
            "cancelled",
            &[ACTIVE, PAUSED, FAILED],
            CANCELLED,
            None,
        )
        .await?;
        self.cleanup_assets(&job.id);
        Ok(())
    }

    /// Remove media directories no live job owns. If the live set cannot be
    /// read nothing is deleted.
    pub async fn garbage_collect_assets(&self) {
        let Ok(entries) = std::fs::read_dir(&self.assets_root) else {
            return;
        };
        let Ok(live) = self.store.live_ids().await else {
            tracing::warn!("scheduled send media cleanup skipped");
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && !live.contains(&name) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Drop a removed device's schedules and their media.
    pub async fn delete_device(&self, device_id: &str) {
        let filter = crate::storage::schedule::Filter {
            device_id: device_id.to_owned(),
            ..Default::default()
        };
        if let Ok(jobs) = self.store.list(filter).await {
            for job in jobs {
                self.cleanup_assets(&job.id);
            }
        }
        if let Err(error) = self.store.delete_device(device_id).await {
            tracing::warn!(%device_id, %error, "failed to delete scheduled sends");
        }
    }
}

/// GOWA `scheduleView`; `next_run_at` is hidden once nothing is left to run.
pub fn view(job: &ScheduledSend) -> Value {
    let mut v = serde_json::Map::new();
    let mut put = |k: &str, val: Value| {
        v.insert(k.into(), val);
    };
    put("id", job.id.clone().into());
    put("message_type", job.message_type.clone().into());
    put("phone", job.phone.clone().into());
    if !job.summary.is_empty() {
        put("summary", job.summary.clone().into());
    }
    put("status", job.status.clone().into());
    put("scheduled_at", rfc3339(job.scheduled_at).into());
    if !matches!(job.status.as_str(), COMPLETED | FAILED | CANCELLED) {
        put("next_run_at", rfc3339(job.next_run_at).into());
    }
    put("timezone", job.timezone.clone().into());
    put("recurrence", job.recurrence.clone().into());
    let weekdays: Vec<i64> = serde_json::from_str(&job.weekdays_json).unwrap_or_default();
    if !weekdays.is_empty() {
        put("weekdays", json!(weekdays));
    }
    if job.day_of_month != 0 {
        put("day_of_month", job.day_of_month.into());
    }
    if let Some(end) = job.end_at {
        put("end_at", rfc3339(end).into());
    }
    if job.occurrence_limit != 0 {
        put("occurrence_limit", job.occurrence_limit.into());
    }
    put("occurrence_count", job.occurrence_count.into());
    put("attempts", job.attempts.into());
    if let Some(at) = job.last_run_at {
        put("last_run_at", rfc3339(at).into());
    }
    if !job.last_message_id.is_empty() {
        put("last_message_id", job.last_message_id.clone().into());
    }
    if !job.last_error.is_empty() {
        put("last_error", job.last_error.clone().into());
    }
    put("created_at", rfc3339(job.created_at).into());
    put("updated_at", rfc3339(job.updated_at).into());
    Value::Object(v)
}

/// The slot after `now` for a job that has completed `count` occurrences, or
/// `None` for one-time jobs, a reached limit, or the end of the series.
pub fn next_run(job: &ScheduledSend, count: i32, now: i64) -> Option<i64> {
    if job.recurrence == "once" || (job.occurrence_limit > 0 && count >= job.occurrence_limit) {
        return None;
    }
    let weekdays: Vec<i64> = serde_json::from_str(&job.weekdays_json).unwrap_or_default();
    let spec = ScheduleSpec {
        scheduled_at: ts(job.scheduled_at),
        timezone: job.timezone.clone(),
        tz: job.timezone.parse().ok()?,
        recurrence: job.recurrence.clone(),
        weekdays: weekdays.iter().map(|d| *d as u8).collect(),
        day_of_month: job.day_of_month as u32,
        end_at: job.end_at.map(ts),
        occurrence_limit: i64::from(job.occurrence_limit),
    };
    next_occurrence(&spec, ts(now)).map(|t| t.timestamp())
}

/// Exponential retry delay: 15 s · 2^attempts, capped at 5 min.
fn retry_delay(attempts: i32) -> i64 {
    (15i64 << attempts.clamp(0, 8)).min(300)
}

pub struct Worker<B: Backend> {
    svc: ScheduleService,
    backend: B,
}

impl<B: Backend> Worker<B> {
    pub fn new(svc: ScheduleService, backend: B) -> Self {
        Self { svc, backend }
    }

    /// Spawn the loop: every 5 s recover interrupted work, then drain what is due.
    pub fn start(self: Arc<Self>) {
        tokio::spawn(async move {
            self.svc.garbage_collect_assets().await;
            let mut tick = tokio::time::interval(TICK);
            loop {
                tick.tick().await;
                self.process_due().await;
            }
        });
    }

    pub async fn process_due(&self) {
        self.recover_interrupted().await;
        loop {
            let now = self.svc.now();
            let token = uuid::Uuid::new_v4().to_string();
            match self
                .svc
                .store
                .claim_next(now, now + LEASE_SECS, &token)
                .await
            {
                Ok(Some(job)) => {
                    if let Err(error) = self.process_job(job.clone()).await {
                        tracing::warn!(schedule = %job.id, %error, "scheduled send failed");
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    tracing::error!(%error, "scheduled send worker failed to claim jobs");
                    return;
                }
            }
        }
    }

    /// Return occurrences whose lease lapsed to the queue. The slot and the
    /// occurrence count stay, so a recurring job resumes the interrupted
    /// occurrence. A pinned occurrence is charged an attempt, so a send that
    /// keeps taking the process down runs out of budget.
    pub async fn recover_interrupted(&self) {
        const REASON: &str = "execution interrupted before completion";
        loop {
            let now = self.svc.now();
            let token = uuid::Uuid::new_v4().to_string();
            let job = match self
                .svc
                .store
                .claim_expired(now, now + LEASE_SECS, &token)
                .await
            {
                Ok(Some(job)) => job,
                Ok(None) => return,
                Err(error) => {
                    tracing::warn!(%error, "scheduled send worker failed to recover interrupted jobs");
                    return;
                }
            };
            let attempts = job.attempts + i32::from(!job.pending_message_id.is_empty());
            let result = if attempts >= MAX_ATTEMPTS {
                self.skip_occurrence(&job, REASON).await
            } else {
                self.svc
                    .store
                    .retry(&job.id, &job.lease_token, REASON, attempts, job.next_run_at)
                    .await
            };
            if let Err(error) = result {
                tracing::warn!(schedule = %job.id, %error, "failed to recover interrupted job");
            }
        }
    }

    async fn process_job(&self, job: ScheduledSend) -> anyhow::Result<()> {
        let now = self.svc.now();
        // An occurrence delayed by an offline device or retries must not go out
        // once the series has ended.
        if job.end_at.is_some_and(|end| now > end) {
            let changed = self
                .svc
                .store
                .set_status(&job.device_id, &job.id, &[RUNNING], COMPLETED, None)
                .await?;
            if changed {
                self.svc.cleanup_assets(&job.id);
            }
            return Ok(());
        }
        if !self.backend.device_ready(&job.device_id).await {
            // Waiting for the device is not the job's fault: no attempt is charged.
            return self
                .svc
                .store
                .retry(
                    &job.id,
                    &job.lease_token,
                    "device is offline",
                    job.attempts,
                    now + OFFLINE_RETRY_SECS,
                )
                .await;
        }
        self.deliver(job).await
    }

    async fn deliver(&self, mut job: ScheduledSend) -> anyhow::Result<()> {
        let store = &self.svc.store;
        // Pin the identity of this occurrence before dispatching.
        let message_id = if job.pending_message_id.is_empty() {
            self.backend.new_message_id(&job.device_id).await
        } else {
            job.pending_message_id.clone()
        };
        let poll_secret = if job.message_type == "poll" {
            job.pending_poll_secret.clone().or_else(|| {
                let mut secret = uuid::Uuid::new_v4().as_bytes().to_vec();
                secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
                Some(secret)
            })
        } else {
            None
        };
        let held = store
            .set_pending_attempt(
                &job.id,
                &job.lease_token,
                &message_id,
                poll_secret.clone(),
                self.svc.now(),
            )
            .await?;
        if !held {
            anyhow::bail!("scheduled send is no longer claimed by this worker");
        }
        job.pending_message_id = message_id.clone();
        job.pending_poll_secret = poll_secret.clone();

        let pinned = Pinned {
            message_id,
            poll_secret,
        };
        match self.backend.dispatch(&job, pinned).await {
            Ok(sent_id) => {
                let count = job.occurrence_count + 1;
                let next = next_run(&job, count, self.svc.now());
                let status = if next.is_some() { ACTIVE } else { COMPLETED };
                store
                    .complete(&job.id, &job.lease_token, status, &sent_id, count, next)
                    .await?;
                if status == COMPLETED && !self.still_pending(&job).await {
                    self.svc.cleanup_assets(&job.id);
                }
                Ok(())
            }
            Err(Failure::Permanent(reason)) => {
                store.fail(&job.id, &job.lease_token, &reason).await?;
                if !self.still_pending(&job).await {
                    self.svc.cleanup_assets(&job.id);
                }
                anyhow::bail!(reason)
            }
            Err(Failure::Transient(reason)) => {
                if job.attempts + 1 >= MAX_ATTEMPTS {
                    self.skip_occurrence(&job, &reason).await?;
                } else {
                    let at = self.svc.now() + retry_delay(job.attempts);
                    store
                        .retry(&job.id, &job.lease_token, &reason, job.attempts + 1, at)
                        .await?;
                }
                anyhow::bail!(reason)
            }
        }
    }

    /// Whether another worker may still send this job (our lease lapsed, so
    /// our terminal transition may not have been recorded).
    async fn still_pending(&self, job: &ScheduledSend) -> bool {
        match self.svc.store.get(&job.device_id, &job.id).await {
            Ok(Some(stored)) => matches!(stored.status.as_str(), ACTIVE | RUNNING | PAUSED),
            Ok(None) => false,
            Err(_) => true,
        }
    }

    /// Give up on the current occurrence: a recurring job moves on to its
    /// next slot with a fresh attempt budget, anything else fails.
    async fn skip_occurrence(&self, job: &ScheduledSend, reason: &str) -> anyhow::Result<()> {
        if let Some(next) = next_run(job, job.occurrence_count, self.svc.now()) {
            return self
                .svc
                .store
                .retry(&job.id, &job.lease_token, reason, 0, next)
                .await;
        }
        self.svc
            .store
            .fail(&job.id, &job.lease_token, reason)
            .await?;
        if !self.still_pending(job).await {
            self.svc.cleanup_assets(&job.id);
        }
        Ok(())
    }
}

/// Read a stored asset back as an upload (used when dispatching).
pub fn load_asset(assets_json: &str, field: &str) -> Result<Option<Upload>, String> {
    if assets_json.trim().is_empty() {
        return Ok(None);
    }
    let all: BTreeMap<String, Value> =
        serde_json::from_str(assets_json).map_err(|e| e.to_string())?;
    let Some(asset) = all.get(field) else {
        return Ok(None);
    };
    let path = asset["path"].as_str().unwrap_or_default();
    if path.is_empty() {
        return Ok(None);
    }
    let bytes = std::fs::read(Path::new(path)).map_err(|e| format!("read scheduled media: {e}"))?;
    let filename = asset["filename"].as_str().unwrap_or_default().to_owned();
    let declared = asset["content_type"].as_str().unwrap_or_default();
    let content_type = if declared.is_empty() {
        mime_guess::from_path(&filename)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_owned()
    } else {
        declared.to_owned()
    };
    Ok(Some(Upload {
        filename,
        content_type,
        bytes,
    }))
}

#[cfg(test)]
mod tests;

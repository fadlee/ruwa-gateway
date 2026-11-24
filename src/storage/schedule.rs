//! `scheduled_sends` (GOWA `sqlite_repository.go` scheduled-send section).

use diesel::{dsl::sql, prelude::*, sql_types::BigInt};

use super::Db;

diesel::table! {
    scheduled_sends (id) {
        id -> Text,
        device_id -> Text,
        message_type -> Text,
        payload_json -> Text,
        assets_json -> Text,
        phone -> Text,
        summary -> Text,
        scheduled_at -> BigInt,
        next_run_at -> BigInt,
        timezone -> Text,
        recurrence -> Text,
        weekdays_json -> Text,
        day_of_month -> Integer,
        end_at -> Nullable<BigInt>,
        occurrence_limit -> Integer,
        occurrence_count -> Integer,
        attempts -> Integer,
        status -> Text,
        lease_token -> Text,
        lease_until -> Nullable<BigInt>,
        last_run_at -> Nullable<BigInt>,
        last_message_id -> Text,
        last_error -> Text,
        created_at -> BigInt,
        updated_at -> BigInt,
        pending_message_id -> Text,
        pending_poll_secret -> Nullable<Binary>,
    }
}

pub const ACTIVE: &str = "active";
pub const RUNNING: &str = "running";
pub const PAUSED: &str = "paused";
pub const COMPLETED: &str = "completed";
pub const FAILED: &str = "failed";
pub const CANCELLED: &str = "cancelled";

#[derive(Debug, Clone, PartialEq, Queryable, Selectable, Insertable)]
#[diesel(table_name = scheduled_sends)]
pub struct ScheduledSend {
    pub id: String,
    pub device_id: String,
    pub message_type: String,
    pub payload_json: String,
    pub assets_json: String,
    pub phone: String,
    pub summary: String,
    pub scheduled_at: i64,
    pub next_run_at: i64,
    pub timezone: String,
    pub recurrence: String,
    pub weekdays_json: String,
    pub day_of_month: i32,
    pub end_at: Option<i64>,
    pub occurrence_limit: i32,
    pub occurrence_count: i32,
    pub attempts: i32,
    pub status: String,
    pub lease_token: String,
    pub lease_until: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_message_id: String,
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub pending_message_id: String,
    pub pending_poll_secret: Option<Vec<u8>>,
}

/// List filter; an empty field matches everything. `limit <= 0` returns all.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub device_id: String,
    pub status: String,
    pub search: String,
    pub message_type: String,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone)]
pub struct ScheduleStore {
    db: Db,
}

fn now() -> i64 {
    crate::device::now()
}

/// `LIKE` pattern matching `search` literally anywhere.
fn like(search: &str) -> String {
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

macro_rules! scoped {
    ($f:expr) => {{
        let f: &Filter = $f;
        let mut q = scheduled_sends::table
            .filter(scheduled_sends::device_id.eq(f.device_id.clone()))
            .into_boxed();
        if !f.status.trim().is_empty() {
            q = q.filter(scheduled_sends::status.eq(f.status.trim().to_owned()));
        }
        if !f.message_type.trim().is_empty() {
            q = q.filter(scheduled_sends::message_type.eq(f.message_type.trim().to_owned()));
        }
        if !f.search.trim().is_empty() {
            let pattern = like(f.search.trim());
            q = q.filter(
                scheduled_sends::phone
                    .like(pattern.clone())
                    .escape('\\')
                    .or(scheduled_sends::summary.like(pattern).escape('\\')),
            );
        }
        q
    }};
}

impl ScheduleStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn create(&self, mut job: ScheduledSend) -> anyhow::Result<ScheduledSend> {
        anyhow::ensure!(
            !job.id.trim().is_empty()
                && !job.device_id.trim().is_empty()
                && !job.message_type.trim().is_empty(),
            "scheduled send requires id, device id, and message type"
        );
        let t = now();
        job.created_at = t;
        job.updated_at = t;
        let row = job.clone();
        self.db
            .run(move |c| {
                diesel::insert_into(scheduled_sends::table)
                    .values(&row)
                    .execute(c)
            })
            .await?;
        Ok(job)
    }

    pub async fn get(&self, device: &str, id: &str) -> anyhow::Result<Option<ScheduledSend>> {
        let (device, id) = (device.to_owned(), id.to_owned());
        self.db
            .run(move |c| {
                scheduled_sends::table
                    .filter(scheduled_sends::device_id.eq(device))
                    .filter(scheduled_sends::id.eq(id))
                    .select(ScheduledSend::as_select())
                    .first(c)
                    .optional()
            })
            .await
    }

    /// Upcoming work first by due time, then history newest first.
    pub async fn list(&self, filter: Filter) -> anyhow::Result<Vec<ScheduledSend>> {
        self.db
            .run(move |c| {
                let mut q = scoped!(&filter)
                    .order((
                        sql::<BigInt>(
                            "CASE WHEN status IN ('active','running','paused') THEN 0 ELSE 1 END",
                        )
                        .asc(),
                        sql::<diesel::sql_types::Nullable<BigInt>>(
                            "CASE WHEN status IN ('active','running','paused') THEN next_run_at END",
                        )
                        .asc(),
                        scheduled_sends::updated_at.desc(),
                        scheduled_sends::id.asc(),
                    ))
                    .select(ScheduledSend::as_select());
                if filter.limit > 0 {
                    q = q.limit(filter.limit.min(1000)).offset(filter.offset.max(0));
                }
                q.load(c)
            })
            .await
    }

    pub async fn count(&self, filter: Filter) -> anyhow::Result<i64> {
        self.db
            .run(move |c| scoped!(&filter).count().get_result(c))
            .await
    }

    /// Ids of every job that may still send (media directory cleanup).
    pub async fn live_ids(&self) -> anyhow::Result<Vec<String>> {
        self.db
            .run(|c| {
                scheduled_sends::table
                    .filter(scheduled_sends::status.eq_any([ACTIVE, RUNNING, PAUSED]))
                    .select(scheduled_sends::id)
                    .load(c)
            })
            .await
    }

    /// Lease the earliest due `active` job (→ `running`).
    pub async fn claim_next(
        &self,
        now_s: i64,
        lease_until: i64,
        token: &str,
    ) -> anyhow::Result<Option<ScheduledSend>> {
        let token = token.to_owned();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    let id: Option<String> = scheduled_sends::table
                        .filter(scheduled_sends::status.eq(ACTIVE))
                        .filter(scheduled_sends::next_run_at.le(now_s))
                        .order((
                            scheduled_sends::next_run_at.asc(),
                            scheduled_sends::created_at.asc(),
                        ))
                        .select(scheduled_sends::id)
                        .first(c)
                        .optional()?;
                    let Some(id) = id else { return Ok(None) };
                    diesel::update(scheduled_sends::table.find(&id))
                        .set((
                            scheduled_sends::status.eq(RUNNING),
                            scheduled_sends::lease_token.eq(&token),
                            scheduled_sends::lease_until.eq(Some(lease_until)),
                            scheduled_sends::updated_at.eq(now_s),
                        ))
                        .execute(c)?;
                    scheduled_sends::table
                        .find(&id)
                        .select(ScheduledSend::as_select())
                        .first(c)
                        .optional()
                })
            })
            .await
    }

    /// Take over a `running` job whose lease lapsed (fences the old worker).
    pub async fn claim_expired(
        &self,
        now_s: i64,
        lease_until: i64,
        token: &str,
    ) -> anyhow::Result<Option<ScheduledSend>> {
        let token = token.to_owned();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    let id: Option<String> = scheduled_sends::table
                        .filter(scheduled_sends::status.eq(RUNNING))
                        .filter(scheduled_sends::lease_until.le(Some(now_s)))
                        .order((
                            scheduled_sends::lease_until.asc(),
                            scheduled_sends::id.asc(),
                        ))
                        .select(scheduled_sends::id)
                        .first(c)
                        .optional()?;
                    let Some(id) = id else { return Ok(None) };
                    diesel::update(scheduled_sends::table.find(&id))
                        .set((
                            scheduled_sends::lease_token.eq(&token),
                            scheduled_sends::lease_until.eq(Some(lease_until)),
                            scheduled_sends::updated_at.eq(now_s),
                        ))
                        .execute(c)?;
                    scheduled_sends::table
                        .find(&id)
                        .select(ScheduledSend::as_select())
                        .first(c)
                        .optional()
                })
            })
            .await
    }

    /// Pin the identity of the occurrence being attempted. `false` when this
    /// worker no longer holds the lease or another id is already pinned.
    pub async fn set_pending_attempt(
        &self,
        id: &str,
        token: &str,
        message_id: &str,
        poll_secret: Option<Vec<u8>>,
    ) -> anyhow::Result<bool> {
        let (id, token, message_id) = (id.to_owned(), token.to_owned(), message_id.to_owned());
        let t = now();
        self.db
            .run(move |c| {
                let held = scheduled_sends::table
                    .filter(scheduled_sends::id.eq(&id))
                    .filter(scheduled_sends::status.eq(RUNNING))
                    .filter(scheduled_sends::lease_token.eq(&token))
                    .filter(scheduled_sends::lease_until.gt(Some(t)))
                    .filter(
                        scheduled_sends::pending_message_id
                            .eq("")
                            .or(scheduled_sends::pending_message_id.eq(&message_id)),
                    );
                diesel::update(held)
                    .set((
                        scheduled_sends::pending_message_id.eq(&message_id),
                        scheduled_sends::pending_poll_secret.eq(poll_secret),
                        scheduled_sends::updated_at.eq(t),
                    ))
                    .execute(c)
                    .map(|n| n > 0)
            })
            .await
    }

    /// A paused job resumed at a new slot must not reuse the old occurrence's id.
    pub async fn clear_pending_message_id(&self, device: &str, id: &str) -> anyhow::Result<()> {
        let (device, id) = (device.to_owned(), id.to_owned());
        let t = now();
        self.db
            .run(move |c| {
                diesel::update(
                    scheduled_sends::table
                        .filter(scheduled_sends::device_id.eq(device))
                        .filter(scheduled_sends::id.eq(id))
                        .filter(scheduled_sends::status.eq(PAUSED)),
                )
                .set((
                    scheduled_sends::pending_message_id.eq(""),
                    scheduled_sends::pending_poll_secret.eq(None::<Vec<u8>>),
                    scheduled_sends::updated_at.eq(t),
                ))
                .execute(c)
            })
            .await?;
        Ok(())
    }

    /// Back to `active` for another try. With no attempts on record the next
    /// run has not been sent under any id yet, so the pinned id is dropped.
    pub async fn retry(
        &self,
        id: &str,
        token: &str,
        last_error: &str,
        attempts: i32,
        next_run_at: i64,
    ) -> anyhow::Result<()> {
        let (id, token, last_error) = (id.to_owned(), token.to_owned(), last_error.to_owned());
        let t = now();
        self.db
            .run(move |c| {
                let held = scheduled_sends::table
                    .filter(scheduled_sends::id.eq(&id))
                    .filter(scheduled_sends::status.eq(RUNNING))
                    .filter(scheduled_sends::lease_token.eq(&token));
                diesel::update(held)
                    .set((
                        scheduled_sends::status.eq(ACTIVE),
                        scheduled_sends::attempts.eq(attempts),
                        scheduled_sends::last_error.eq(&last_error),
                        scheduled_sends::next_run_at.eq(next_run_at),
                        scheduled_sends::lease_token.eq(""),
                        scheduled_sends::lease_until.eq(None::<i64>),
                        scheduled_sends::updated_at.eq(t),
                    ))
                    .execute(c)?;
                if attempts <= 0 {
                    diesel::update(scheduled_sends::table.find(&id))
                        .filter(scheduled_sends::status.eq(ACTIVE))
                        .set((
                            scheduled_sends::pending_message_id.eq(""),
                            scheduled_sends::pending_poll_secret.eq(None::<Vec<u8>>),
                        ))
                        .execute(c)?;
                }
                Ok(())
            })
            .await
    }

    pub async fn complete(
        &self,
        id: &str,
        token: &str,
        status: &str,
        last_message_id: &str,
        occurrence_count: i32,
        next_run_at: Option<i64>,
    ) -> anyhow::Result<()> {
        let (id, token, status, last_message_id) = (
            id.to_owned(),
            token.to_owned(),
            status.to_owned(),
            last_message_id.to_owned(),
        );
        let t = now();
        self.db
            .run(move |c| {
                let held = scheduled_sends::table
                    .filter(scheduled_sends::id.eq(&id))
                    .filter(scheduled_sends::status.eq(RUNNING))
                    .filter(scheduled_sends::lease_token.eq(&token));
                let current_next: Option<i64> = held
                    .clone()
                    .select(scheduled_sends::next_run_at)
                    .first(c)
                    .optional()?;
                let Some(current_next) = current_next else {
                    return Ok(());
                };
                diesel::update(held)
                    .set((
                        scheduled_sends::status.eq(&status),
                        scheduled_sends::occurrence_count.eq(occurrence_count),
                        scheduled_sends::attempts.eq(0),
                        scheduled_sends::last_message_id.eq(&last_message_id),
                        scheduled_sends::pending_message_id.eq(""),
                        scheduled_sends::pending_poll_secret.eq(None::<Vec<u8>>),
                        scheduled_sends::last_run_at.eq(Some(t)),
                        scheduled_sends::next_run_at.eq(next_run_at.unwrap_or(current_next)),
                        scheduled_sends::last_error.eq(""),
                        scheduled_sends::lease_token.eq(""),
                        scheduled_sends::lease_until.eq(None::<i64>),
                        scheduled_sends::updated_at.eq(t),
                    ))
                    .execute(c)
                    .map(|_| ())
            })
            .await
    }

    pub async fn fail(&self, id: &str, token: &str, last_error: &str) -> anyhow::Result<()> {
        let (id, token, last_error) = (id.to_owned(), token.to_owned(), last_error.to_owned());
        let t = now();
        self.db
            .run(move |c| {
                diesel::update(
                    scheduled_sends::table
                        .filter(scheduled_sends::id.eq(id))
                        .filter(scheduled_sends::status.eq(RUNNING))
                        .filter(scheduled_sends::lease_token.eq(token)),
                )
                .set((
                    scheduled_sends::status.eq(FAILED),
                    scheduled_sends::pending_message_id.eq(""),
                    scheduled_sends::pending_poll_secret.eq(None::<Vec<u8>>),
                    scheduled_sends::last_error.eq(last_error),
                    scheduled_sends::lease_token.eq(""),
                    scheduled_sends::lease_until.eq(None::<i64>),
                    scheduled_sends::updated_at.eq(t),
                ))
                .execute(c)
                .map(|_| ())
            })
            .await
    }

    /// Move a job between statuses only while it is in one of `from`, so the
    /// worker's `running` job can never be changed under it.
    pub async fn set_status(
        &self,
        device: &str,
        id: &str,
        from: &[&str],
        status: &str,
        next_run_at: Option<i64>,
    ) -> anyhow::Result<bool> {
        let (device, id, status) = (device.to_owned(), id.to_owned(), status.to_owned());
        let from: Vec<String> = from.iter().map(|s| (*s).to_owned()).collect();
        let t = now();
        self.db
            .run(move |c| {
                c.transaction(|c| {
                    let target = scheduled_sends::table
                        .filter(scheduled_sends::device_id.eq(&device))
                        .filter(scheduled_sends::id.eq(&id))
                        .filter(scheduled_sends::status.eq_any(&from));
                    let current: Option<i64> = target
                        .clone()
                        .select(scheduled_sends::next_run_at)
                        .first(c)
                        .optional()?;
                    let Some(current) = current else {
                        return Ok(false);
                    };
                    diesel::update(target)
                        .set((
                            scheduled_sends::status.eq(&status),
                            scheduled_sends::next_run_at.eq(next_run_at.unwrap_or(current)),
                            scheduled_sends::lease_token.eq(""),
                            scheduled_sends::lease_until.eq(None::<i64>),
                            scheduled_sends::updated_at.eq(t),
                        ))
                        .execute(c)?;
                    Ok(true)
                })
            })
            .await
    }

    /// Drop every job of a removed device.
    pub async fn delete_device(&self, device: &str) -> anyhow::Result<()> {
        let device = device.to_owned();
        self.db
            .run(move |c| {
                diesel::delete(scheduled_sends::table.filter(scheduled_sends::device_id.eq(device)))
                    .execute(c)
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ScheduleStore {
        ScheduleStore::new(Db::open(":memory:").unwrap())
    }

    fn job(id: &str, device: &str, next: i64) -> ScheduledSend {
        ScheduledSend {
            id: id.into(),
            device_id: device.into(),
            message_type: "text".into(),
            payload_json: "{}".into(),
            assets_json: String::new(),
            phone: "628111@s.whatsapp.net".into(),
            summary: format!("hello {id}"),
            scheduled_at: next,
            next_run_at: next,
            timezone: "UTC".into(),
            recurrence: "once".into(),
            weekdays_json: "[]".into(),
            day_of_month: 0,
            end_at: None,
            occurrence_limit: 0,
            occurrence_count: 0,
            attempts: 0,
            status: ACTIVE.into(),
            lease_token: String::new(),
            lease_until: None,
            last_run_at: None,
            last_message_id: String::new(),
            last_error: String::new(),
            created_at: 0,
            updated_at: 0,
            pending_message_id: String::new(),
            pending_poll_secret: None,
        }
    }

    #[tokio::test]
    async fn claim_takes_the_earliest_due_job_once() {
        let s = store();
        s.create(job("late", "d", 200)).await.unwrap();
        s.create(job("early", "d", 100)).await.unwrap();
        s.create(job("future", "d", 10_000)).await.unwrap();
        assert!(s.claim_next(50, 400, "t0").await.unwrap().is_none());
        let a = s.claim_next(300, 400, "t1").await.unwrap().unwrap();
        assert_eq!(
            (a.id.as_str(), a.status.as_str(), a.lease_token.as_str()),
            ("early", RUNNING, "t1")
        );
        let b = s.claim_next(300, 400, "t2").await.unwrap().unwrap();
        assert_eq!(b.id, "late");
        assert!(s.claim_next(300, 400, "t3").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_leases_are_recovered_and_fence_the_old_worker() {
        let s = store();
        s.create(job("a", "d", 100)).await.unwrap();
        s.claim_next(150, 160, "old").await.unwrap().unwrap();
        assert!(
            s.claim_expired(155, 400, "new").await.unwrap().is_none(),
            "lease still valid"
        );
        let r = s.claim_expired(170, 400, "new").await.unwrap().unwrap();
        assert_eq!(r.lease_token, "new");
        // The old worker's results are ignored.
        s.complete("a", "old", COMPLETED, "X", 1, None)
            .await
            .unwrap();
        assert_eq!(s.get("d", "a").await.unwrap().unwrap().status, RUNNING);
        s.complete("a", "new", COMPLETED, "X", 1, None)
            .await
            .unwrap();
        let done = s.get("d", "a").await.unwrap().unwrap();
        assert_eq!(
            (
                done.status.as_str(),
                done.last_message_id.as_str(),
                done.occurrence_count
            ),
            (COMPLETED, "X", 1)
        );
        assert_eq!(done.next_run_at, 100, "no next slot keeps the old one");
    }

    #[tokio::test]
    async fn pending_message_id_is_pinned_and_dropped_on_a_clean_retry() {
        let s = store();
        s.create(job("a", "d", 100)).await.unwrap();
        // `set_pending_attempt` checks the lease against the real clock.
        s.claim_next(150, 9_999_999_999, "t")
            .await
            .unwrap()
            .unwrap();
        assert!(
            s.set_pending_attempt("a", "t", "3EB0AA", None)
                .await
                .unwrap()
        );
        assert!(
            s.set_pending_attempt("a", "t", "3EB0AA", None)
                .await
                .unwrap(),
            "same id again is fine"
        );
        assert!(
            !s.set_pending_attempt("a", "t", "OTHER", None)
                .await
                .unwrap(),
            "another id is refused"
        );
        assert!(
            !s.set_pending_attempt("a", "stale", "3EB0AA", None)
                .await
                .unwrap()
        );
        // A failed attempt keeps the id for the retry …
        s.retry("a", "t", "boom", 1, 200).await.unwrap();
        let j = s.get("d", "a").await.unwrap().unwrap();
        assert_eq!(
            (j.status.as_str(), j.attempts, j.pending_message_id.as_str()),
            (ACTIVE, 1, "3EB0AA")
        );
        // … but a retry with no attempts on record starts a fresh identity.
        s.claim_next(300, 400, "t2").await.unwrap().unwrap();
        s.retry("a", "t2", "", 0, 300).await.unwrap();
        assert_eq!(
            s.get("d", "a").await.unwrap().unwrap().pending_message_id,
            ""
        );
    }

    #[tokio::test]
    async fn status_changes_only_from_allowed_states() {
        let s = store();
        s.create(job("a", "d", 100)).await.unwrap();
        assert!(
            s.set_status("d", "a", &[ACTIVE], PAUSED, None)
                .await
                .unwrap()
        );
        assert!(
            !s.set_status("d", "a", &[ACTIVE], PAUSED, None)
                .await
                .unwrap(),
            "already paused"
        );
        assert!(
            !s.set_status("other", "a", &[PAUSED], ACTIVE, None)
                .await
                .unwrap(),
            "device scoped"
        );
        assert!(
            s.set_status("d", "a", &[PAUSED], ACTIVE, Some(500))
                .await
                .unwrap()
        );
        assert_eq!(s.get("d", "a").await.unwrap().unwrap().next_run_at, 500);
        s.claim_next(600, 700, "t").await.unwrap().unwrap();
        assert!(
            !s.set_status("d", "a", &[ACTIVE, PAUSED], CANCELLED, None)
                .await
                .unwrap(),
            "running is untouchable"
        );
    }

    #[tokio::test]
    async fn list_orders_upcoming_first_and_filters() {
        let s = store();
        s.create(job("b", "d", 300)).await.unwrap();
        s.create(job("a", "d", 100)).await.unwrap();
        s.create(job("done", "d", 50)).await.unwrap();
        s.create(job("x", "other", 10)).await.unwrap();
        let f = |status: &str, search: &str| Filter {
            device_id: "d".into(),
            status: status.into(),
            search: search.into(),
            ..Default::default()
        };
        let ids: Vec<String> = s
            .list(f("", ""))
            .await
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(s.count(f("", "")).await.unwrap(), 3);
        assert_eq!(s.count(f("", "hello a")).await.unwrap(), 1);
        assert_eq!(
            s.count(f("", "628111")).await.unwrap(),
            3,
            "search matches the recipient"
        );
        assert_eq!(
            s.count(f("", "50%")).await.unwrap(),
            0,
            "wildcards are literal"
        );
        let page = s
            .list(Filter {
                limit: 1,
                offset: 1,
                ..f("", "")
            })
            .await
            .unwrap();
        assert_eq!(page.len(), 1);
    }

    #[tokio::test]
    async fn live_ids_and_device_cleanup() {
        let s = store();
        s.create(job("a", "d", 100)).await.unwrap();
        s.create(job("b", "d", 100)).await.unwrap();
        s.set_status("d", "b", &[ACTIVE], CANCELLED, None)
            .await
            .unwrap();
        assert_eq!(s.live_ids().await.unwrap(), vec!["a".to_owned()]);
        s.delete_device("d").await.unwrap();
        assert!(s.get("d", "a").await.unwrap().is_none());
    }
}

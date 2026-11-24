use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering},
    },
};

use super::*;
use crate::storage::Db;

/// 2027-01-15T08:00:00Z.
const T0: i64 = 1_800_000_000;

struct Fake {
    outcomes: Mutex<VecDeque<Result<String, Failure>>>,
    ready: AtomicBool,
    seen: Mutex<Vec<Pinned>>,
    ids: AtomicU32,
}

impl Fake {
    fn new(outcomes: Vec<Result<String, Failure>>) -> Arc<Self> {
        Arc::new(Self {
            outcomes: Mutex::new(outcomes.into()),
            ready: AtomicBool::new(true),
            seen: Mutex::new(Vec::new()),
            ids: AtomicU32::new(0),
        })
    }
}

impl Backend for Arc<Fake> {
    async fn device_ready(&self, _: &str) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
    async fn new_message_id(&self, _: &str) -> String {
        format!("ID{}", self.ids.fetch_add(1, Ordering::SeqCst))
    }
    async fn dispatch(&self, _: &ScheduledSend, pinned: Pinned) -> Result<String, Failure> {
        self.seen.lock().unwrap().push(pinned.clone());
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(format!("sent-{}", pinned.message_id)))
    }
}

struct Rig {
    svc: ScheduleService,
    clock: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(AtomicI64::new(T0));
        let c = clock.clone();
        let svc = ScheduleService::with_clock(
            ScheduleStore::new(Db::open(":memory:").unwrap()),
            dir.path().join("scheduled"),
            Arc::new(move || c.load(Ordering::SeqCst)),
        );
        Self {
            svc,
            clock,
            _dir: dir,
        }
    }

    fn at(&self, secs: i64) {
        self.clock.store(secs, Ordering::SeqCst);
    }

    fn worker(&self, fake: &Arc<Fake>) -> Worker<Arc<Fake>> {
        Worker::new(self.svc.clone(), fake.clone())
    }

    /// Schedule a text send `delay` seconds from now.
    async fn schedule(
        &self,
        delay: i64,
        recurrence: &str,
        extra: impl FnOnce(&mut ScheduleOptions),
    ) -> String {
        let mut o = ScheduleOptions {
            scheduled_at: rfc3339(T0 + delay),
            timezone: "UTC".into(),
            recurrence: recurrence.into(),
            ..Default::default()
        };
        extra(&mut o);
        self.svc
            .create(
                "dev",
                "text",
                "628111@s.whatsapp.net",
                "hello",
                &o,
                json!({"message":"hello"}),
                vec![],
            )
            .await
            .unwrap()
            .schedule_id
    }

    async fn job(&self, id: &str) -> ScheduledSend {
        self.svc.store().get("dev", id).await.unwrap().unwrap()
    }
}

#[tokio::test]
async fn create_returns_ids_and_the_view_matches_the_contract() {
    let rig = Rig::new();
    let res = rig
        .svc
        .create(
            "dev",
            "text",
            "628111@s.whatsapp.net",
            "selamat pagi",
            &ScheduleOptions {
                scheduled_at: rfc3339(T0 + 3600),
                timezone: "Asia/Jakarta".into(),
                recurrence: "weekly".into(),
                weekdays: vec![1, 3],
                occurrence_limit: 5,
                ..Default::default()
            },
            json!({"message": "selamat pagi"}),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(res.status, "Message scheduled");
    assert_eq!(res.scheduled_at, rfc3339(T0 + 3600));
    assert_eq!(res.next_run_at, res.scheduled_at);
    assert!(res.message_id.is_empty());

    let v = rig.svc.get("dev", &res.schedule_id).await.unwrap();
    assert_eq!(v["status"], "active");
    assert_eq!(v["message_type"], "text");
    assert_eq!(v["summary"], "selamat pagi");
    assert_eq!(v["recurrence"], "weekly");
    assert_eq!(v["weekdays"], json!([1, 3]));
    assert_eq!(v["occurrence_limit"], 5);
    assert_eq!(v["occurrence_count"], 0);
    assert_eq!(v["attempts"], 0);
    assert_eq!(v["timezone"], "Asia/Jakarta");
    assert!(
        v.get("day_of_month").is_none()
            && v.get("end_at").is_none()
            && v.get("last_error").is_none()
    );

    // Other devices cannot see it.
    assert_eq!(
        rig.svc
            .get("other", &res.schedule_id)
            .await
            .unwrap_err()
            .code,
        "NOT_FOUND"
    );
    // Past times are refused.
    let err = rig
        .svc
        .create(
            "dev",
            "text",
            "p",
            "s",
            &ScheduleOptions {
                scheduled_at: rfc3339(T0 - 1),
                timezone: "UTC".into(),
                ..Default::default()
            },
            json!({}),
            vec![],
        )
        .await
        .unwrap_err();
    assert_eq!(err.message, "scheduled_at must be in the future");
}

#[tokio::test]
async fn a_due_one_time_job_is_sent_once_and_completes() {
    let rig = Rig::new();
    let id = rig.schedule(60, "once", |_| {}).await;
    let fake = Fake::new(vec![]);
    let worker = rig.worker(&fake);

    worker.process_due().await;
    assert!(fake.seen.lock().unwrap().is_empty(), "not due yet");

    rig.at(T0 + 61);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(job.status, COMPLETED);
    assert_eq!(job.occurrence_count, 1);
    assert_eq!(job.last_message_id, "sent-ID0");
    assert_eq!(job.pending_message_id, "", "the pin is released");
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
    let v = view(&job);
    assert!(v.get("next_run_at").is_none(), "nothing left to run");

    // A second tick does not resend.
    worker.process_due().await;
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_recurring_job_moves_to_its_next_slot_until_the_limit() {
    let rig = Rig::new();
    let id = rig.schedule(60, "daily", |o| o.occurrence_limit = 2).await;
    let fake = Fake::new(vec![]);
    let worker = rig.worker(&fake);

    rig.at(T0 + 61);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!((job.status.as_str(), job.occurrence_count), (ACTIVE, 1));
    assert_eq!(
        job.next_run_at,
        T0 + 60 + 86_400,
        "same wall clock tomorrow"
    );

    rig.at(T0 + 60 + 86_400 + 1);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(
        (job.status.as_str(), job.occurrence_count),
        (COMPLETED, 2),
        "limit reached"
    );
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
    let seen = fake.seen.lock().unwrap();
    assert_ne!(
        seen[0].message_id, seen[1].message_id,
        "each occurrence has its own id"
    );
}

#[tokio::test]
async fn a_transient_failure_retries_under_the_same_message_id() {
    let rig = Rig::new();
    let id = rig.schedule(60, "once", |_| {}).await;
    let fake = Fake::new(vec![
        Err(Failure::Transient("network".into())),
        Err(Failure::Transient("network".into())),
    ]);
    let worker = rig.worker(&fake);

    rig.at(T0 + 61);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(
        (job.status.as_str(), job.attempts, job.last_error.as_str()),
        (ACTIVE, 1, "network")
    );
    assert_eq!(
        job.next_run_at,
        T0 + 61 + 15,
        "15s · 2^0 (attempts before the increment)"
    );
    assert_eq!(job.pending_message_id, "ID0", "id stays pinned");

    rig.at(T0 + 61 + 15);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(job.attempts, 2);
    assert_eq!(job.next_run_at, T0 + 61 + 15 + 30, "15s · 2^1");

    rig.at(job.next_run_at);
    worker.process_due().await;
    assert_eq!(rig.job(&id).await.status, COMPLETED);
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(
        seen.iter().all(|p| p.message_id == "ID0"),
        "no duplicate message identity"
    );
}

#[tokio::test]
async fn running_out_of_attempts_fails_a_one_time_job_but_skips_a_recurring_one() {
    let rig = Rig::new();
    let once = rig.schedule(60, "once", |_| {}).await;
    let daily = rig.schedule(60, "daily", |_| {}).await;
    let fail = |_: usize| Err(Failure::Transient("boom".into()));
    let fake = Fake::new((0..40).map(fail).collect());
    let worker = rig.worker(&fake);

    let mut t = T0 + 61;
    for _ in 0..MAX_ATTEMPTS {
        rig.at(t);
        worker.process_due().await;
        t += 301; // beyond the largest back-off
    }
    let once = rig.job(&once).await;
    assert_eq!(once.status, FAILED);
    assert_eq!(once.last_error, "boom");
    let daily = rig.job(&daily).await;
    assert_eq!(daily.status, ACTIVE, "a recurring job moves on");
    assert_eq!(daily.attempts, 0, "with a fresh budget");
    assert_eq!(
        daily.occurrence_count, 0,
        "the skipped occurrence is not counted"
    );
    assert!(daily.next_run_at > T0 + 86_400);
    assert_eq!(
        daily.pending_message_id, "",
        "the next occurrence gets its own id"
    );
}

#[tokio::test]
async fn a_permanent_failure_fails_at_once() {
    let rig = Rig::new();
    let id = rig.schedule(60, "daily", |_| {}).await;
    let fake = Fake::new(vec![Err(Failure::Permanent(
        "invalid scheduled payload".into(),
    ))]);
    rig.at(T0 + 61);
    rig.worker(&fake).process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(
        (job.status.as_str(), job.last_error.as_str()),
        (FAILED, "invalid scheduled payload")
    );
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_offline_device_costs_no_attempt() {
    let rig = Rig::new();
    let id = rig.schedule(60, "once", |_| {}).await;
    let fake = Fake::new(vec![]);
    fake.ready.store(false, Ordering::SeqCst);
    let worker = rig.worker(&fake);

    rig.at(T0 + 61);
    worker.process_due().await;
    let job = rig.job(&id).await;
    assert_eq!(
        (job.status.as_str(), job.attempts, job.last_error.as_str()),
        (ACTIVE, 0, "device is offline")
    );
    assert_eq!(job.next_run_at, T0 + 61 + 30);
    assert!(fake.seen.lock().unwrap().is_empty());

    fake.ready.store(true, Ordering::SeqCst);
    rig.at(T0 + 61 + 30);
    worker.process_due().await;
    assert_eq!(rig.job(&id).await.status, COMPLETED);
}

#[tokio::test]
async fn an_occurrence_after_end_at_is_never_sent() {
    let rig = Rig::new();
    let end = rfc3339(T0 + 120);
    let id = rig.schedule(60, "daily", |o| o.end_at = end).await;
    let fake = Fake::new(vec![]);
    rig.at(T0 + 3600);
    rig.worker(&fake).process_due().await;
    assert_eq!(rig.job(&id).await.status, COMPLETED);
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_interrupted_attempt_is_recovered_and_charged() {
    let rig = Rig::new();
    let id = rig.schedule(60, "once", |_| {}).await;
    let store = rig.svc.store();
    // A worker claimed it, pinned an id and died.
    store
        .claim_next(T0 + 61, T0 + 61 + 120, "dead")
        .await
        .unwrap()
        .unwrap();
    store
        .set_pending_attempt(&id, "dead", "ID9", None, T0 + 62)
        .await
        .unwrap();

    let fake = Fake::new(vec![]);
    let worker = rig.worker(&fake);
    rig.at(T0 + 61 + 121);
    worker.recover_interrupted().await;
    let job = rig.job(&id).await;
    assert_eq!(
        (job.status.as_str(), job.attempts),
        (ACTIVE, 1),
        "a pinned attempt costs one"
    );
    assert_eq!(job.pending_message_id, "ID9", "and keeps its identity");

    worker.process_due().await;
    assert_eq!(
        fake.seen.lock().unwrap()[0].message_id,
        "ID9",
        "the retry reuses the id"
    );
    assert_eq!(rig.job(&id).await.status, COMPLETED);
}

#[tokio::test]
async fn a_poll_keeps_its_secret_across_a_retry() {
    let rig = Rig::new();
    let o = ScheduleOptions {
        scheduled_at: rfc3339(T0 + 60),
        timezone: "UTC".into(),
        ..Default::default()
    };
    let id = rig
        .svc
        .create(
            "dev",
            "poll",
            "628@s.whatsapp.net",
            "q",
            &o,
            json!({}),
            vec![],
        )
        .await
        .unwrap()
        .schedule_id;
    let fake = Fake::new(vec![Err(Failure::Transient("x".into()))]);
    let worker = rig.worker(&fake);
    rig.at(T0 + 61);
    worker.process_due().await;
    rig.at(T0 + 61 + 30);
    worker.process_due().await;
    {
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].poll_secret.as_ref().map(Vec::len), Some(32));
        assert_eq!(seen[0].poll_secret, seen[1].poll_secret);
    }
    assert_eq!(rig.job(&id).await.status, COMPLETED);
}

#[tokio::test]
async fn pause_resume_and_cancel_follow_the_state_rules() {
    let rig = Rig::new();
    let id = rig.schedule(60, "once", |_| {}).await;

    rig.svc.pause("dev", &id).await.unwrap();
    assert_eq!(rig.job(&id).await.status, PAUSED);
    assert_eq!(
        rig.svc.pause("dev", &id).await.unwrap_err().message,
        "scheduled send cannot be paused while paused"
    );
    assert_eq!(
        rig.svc.resume("dev", "nope").await.unwrap_err().code,
        "NOT_FOUND"
    );

    // Not overdue: keeps its slot.
    rig.svc.resume("dev", &id).await.unwrap();
    let job = rig.job(&id).await;
    assert_eq!((job.status.as_str(), job.next_run_at), (ACTIVE, T0 + 60));
    assert_eq!(
        rig.svc.resume("dev", &id).await.unwrap_err().message,
        "scheduled send cannot be resumed while active"
    );

    // Overdue one-time job sends right away.
    rig.svc.pause("dev", &id).await.unwrap();
    rig.at(T0 + 500);
    rig.svc.resume("dev", &id).await.unwrap();
    assert_eq!(rig.job(&id).await.next_run_at, T0 + 500);

    rig.svc.cancel("dev", &id).await.unwrap();
    assert_eq!(rig.job(&id).await.status, CANCELLED);
    assert_eq!(
        rig.svc.cancel("dev", &id).await.unwrap_err().message,
        "scheduled send cannot be cancelled while cancelled"
    );
}

#[tokio::test]
async fn resuming_an_overdue_recurring_job_skips_the_missed_slot() {
    let rig = Rig::new();
    let id = rig.schedule(60, "daily", |_| {}).await;
    let store = rig.svc.store();
    // Pretend the missed occurrence had been pinned.
    store
        .claim_next(T0 + 61, T0 + 9999, "t")
        .await
        .unwrap()
        .unwrap();
    store
        .set_pending_attempt(&id, "t", "ID1", None, T0 + 62)
        .await
        .unwrap();
    store.retry(&id, "t", "x", 1, T0 + 100).await.unwrap();
    rig.svc.pause("dev", &id).await.unwrap();

    // A series whose end passes while paused.
    let end = rfc3339(T0 + 200);
    let ended = rig.schedule(100, "daily", |o| o.end_at = end).await;
    rig.svc.pause("dev", &ended).await.unwrap();

    rig.at(T0 + 100_000);
    rig.svc.resume("dev", &id).await.unwrap();
    let job = rig.job(&id).await;
    assert_eq!(job.status, ACTIVE);
    assert!(job.next_run_at > T0 + 100_000, "continues at the next slot");
    assert_eq!(job.pending_message_id, "", "the old id is not reused");

    // …completes on resume instead of sending past its end.
    rig.svc.resume("dev", &ended).await.unwrap();
    assert_eq!(rig.job(&ended).await.status, COMPLETED);
}

#[tokio::test]
async fn media_is_stored_for_the_job_and_removed_when_it_ends() {
    let rig = Rig::new();
    let o = ScheduleOptions {
        scheduled_at: rfc3339(T0 + 60),
        timezone: "UTC".into(),
        ..Default::default()
    };
    let upload = Upload {
        filename: "a.png".into(),
        content_type: "image/png".into(),
        bytes: vec![1, 2, 3],
    };
    let id = rig
        .svc
        .create(
            "dev",
            "image",
            "628@s.whatsapp.net",
            "cap",
            &o,
            json!({}),
            vec![("image".into(), upload.clone())],
        )
        .await
        .unwrap()
        .schedule_id;
    let job = rig.job(&id).await;
    let back = load_asset(&job.assets_json, "image").unwrap().unwrap();
    assert_eq!(
        (
            back.filename.as_str(),
            back.content_type.as_str(),
            back.bytes.as_slice()
        ),
        ("a.png", "image/png", &[1u8, 2, 3][..])
    );
    assert!(load_asset(&job.assets_json, "other").unwrap().is_none());

    // Path tricks in the filename are refused and leave nothing behind.
    let evil = Upload {
        filename: "../x.png".into(),
        ..upload
    };
    let err = rig
        .svc
        .create(
            "dev",
            "image",
            "p",
            "s",
            &o,
            json!({}),
            vec![("image".into(), evil)],
        )
        .await
        .unwrap_err();
    assert_eq!(err.message, "invalid uploaded filename");

    let fake = Fake::new(vec![]);
    rig.at(T0 + 61);
    rig.worker(&fake).process_due().await;
    assert!(
        !rig.svc.job_dir(&id).exists(),
        "media removed once the job completed"
    );
}

#[tokio::test]
async fn startup_cleanup_keeps_only_live_jobs_media() {
    let rig = Rig::new();
    let o = ScheduleOptions {
        scheduled_at: rfc3339(T0 + 60),
        timezone: "UTC".into(),
        ..Default::default()
    };
    let up = || Upload {
        filename: "a.png".into(),
        content_type: String::new(),
        bytes: vec![0],
    };
    let live = rig
        .svc
        .create(
            "dev",
            "image",
            "p",
            "s",
            &o,
            json!({}),
            vec![("image".into(), up())],
        )
        .await
        .unwrap()
        .schedule_id;
    let orphan = "orphan-id";
    std::fs::create_dir_all(rig.svc.job_dir(orphan)).unwrap();
    rig.svc.garbage_collect_assets().await;
    assert!(rig.svc.job_dir(&live).exists());
    assert!(!rig.svc.job_dir(orphan).exists());
}

#[tokio::test]
async fn list_validates_and_paginates() {
    let rig = Rig::new();
    for _ in 0..3 {
        rig.schedule(60, "once", |_| {}).await;
    }
    let v = rig.svc.list("dev", ListQuery::default()).await.unwrap();
    assert_eq!(
        v["pagination"],
        json!({"limit": 25, "offset": 0, "total": 3})
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 3);
    let page = rig
        .svc
        .list(
            "dev",
            ListQuery {
                limit: 2,
                offset: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    let none = rig
        .svc
        .list(
            "dev",
            ListQuery {
                status: "paused".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(none["data"], json!([]), "an empty page is [] not null");
    assert_eq!(none["pagination"]["total"], 0);

    let msg = |q: ListQuery| {
        let svc = rig.svc.clone();
        async move { svc.list("dev", q).await.unwrap_err().message }
    };
    assert_eq!(
        msg(ListQuery {
            limit: 101,
            ..Default::default()
        })
        .await,
        "limit: must be no greater than 100."
    );
    assert_eq!(
        msg(ListQuery {
            offset: -1,
            ..Default::default()
        })
        .await,
        "offset: must be no less than 0."
    );
    assert_eq!(
        msg(ListQuery {
            status: "x".into(),
            ..Default::default()
        })
        .await,
        "status: must be a valid value."
    );
    assert_eq!(
        msg(ListQuery {
            message_type: "x".into(),
            ..Default::default()
        })
        .await,
        "message_type: must be a valid value."
    );
}

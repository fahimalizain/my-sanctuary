//! Tick logic against an in-memory [`DigestRepo`].
//!
//! The fake stores days, dirty rows, job rows and category stamps in maps and
//! is deliberately dumb: it never re-derives anything, so a passing test says
//! the *orchestration* (backfill cursor, reclassify cursor, dirty clearing,
//! budget) is right, while the SQL round-trip lives in `cargo test -p
//! d1-sync`.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;

use super::repo::{DigestRepo, JobStatus, ReclassifyRow};
use super::sleep::resolve_sleep_category_id;
use super::tests::support::{day_events, event, kolkata_event, tick_now, utc};
use super::tick::{JOB_BACKFILL, JOB_DONE, JOB_RECLASSIFY, JOB_RUNNING, RECLASSIFY_BATCH_LIMIT};
use super::*;
use crate::models::{
    NewTaskCategory, NewTaskCategoryPattern, TaskCategory, TaskCategoryPattern,
    UpdateTaskCategory,
};
use crate::repo::{RepoError, TaskCategoryRepo};

fn backend(message: &str) -> RepoError {
    RepoError::Backend(message.to_string())
}

/// One stored reclassify candidate.
#[derive(Debug, Clone)]
pub struct FakeRow {
    pub id: String,
    pub title: String,
    pub google_calendar_id: String,
    pub start_time: String,
    pub end_time: String,
    pub category_id: Option<String>,
}

impl FakeRow {
    fn to_batch_row(&self) -> ReclassifyRow {
        ReclassifyRow {
            id: self.id.clone(),
            title: self.title.clone(),
            google_calendar_id: self.google_calendar_id.clone(),
            start_time: self.start_time.clone(),
            end_time: self.end_time.clone(),
            start_time_zone: "UTC".to_string(),
            end_time_zone: "UTC".to_string(),
            category_id: self.category_id.clone(),
        }
    }
}

#[derive(Default)]
pub struct FakeState {
    pub user_id: String,
    pub dirty: BTreeMap<String, String>,
    pub jobs: BTreeMap<String, JobStatus>,
    pub days: BTreeMap<String, DayDigest>,
    pub replace_day_calls: Vec<String>,
    pub primary_tz: Option<String>,
    pub events: Vec<DigestEvent>,
    pub rows: Vec<FakeRow>,
    pub stamps: BTreeMap<String, Option<String>>,
    /// Every `(from, to)` window `load_events_touching` was asked for.
    pub load_windows: Vec<(String, String)>,
    pub mark_dirty_fails: bool,
    pub rules_broken: Option<String>,
    /// A taxonomy edit that lands **while** the tick is rebuilding: `replace_day`
    /// flips the reclassify job back to `running`, so the clear step must see a
    /// restamp that started mid-rebuild (F10).
    pub reclassify_on_replace_day: bool,
    /// The user's `sleep_duration` rule target in seconds (F15); `None` = no
    /// such rule, so the caller keeps `DEFAULT_SLEEP_SECS`.
    pub sleep_target: Option<i64>,
}

pub struct FakeDigestRepo {
    pub state: Mutex<FakeState>,
}

impl FakeDigestRepo {
    pub fn new(user_id: &str) -> Self {
        Self {
            state: Mutex::new(FakeState {
                user_id: user_id.to_string(),
                primary_tz: Some("Asia/Kolkata".to_string()),
                ..FakeState::default()
            }),
        }
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().expect("fake lock")
    }

    pub fn mutate(&self, edit: impl FnOnce(&mut FakeState)) {
        edit(&mut self.state.lock().expect("fake lock"));
    }

    pub fn dirty_dates(&self) -> Vec<String> {
        self.state.lock().expect("fake lock").dirty.keys().cloned().collect()
    }

    pub fn job(&self, kind: &str) -> Option<JobStatus> {
        self.state.lock().expect("fake lock").jobs.get(kind).cloned()
    }
}

#[async_trait(?Send)]
impl DigestRepo for FakeDigestRepo {
    async fn mark_dirty(
        &self,
        user_id: &str,
        dates: &[String],
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        if state.mark_dirty_fails {
            return Err(backend("mark_dirty failed"));
        }
        assert_eq!(user_id, state.user_id, "one user per fake");
        for date in dates {
            state.dirty.insert(date.clone(), now_rfc3339.to_string());
        }
        Ok(())
    }

    async fn list_dirty(&self, user_id: &str, limit: usize) -> Result<Vec<String>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(state.dirty.keys().take(limit).cloned().collect())
    }

    async fn count_dirty(&self, user_id: &str) -> Result<i64, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(state.dirty.len() as i64)
    }

    async fn clear_dirty(&self, user_id: &str, dates: &[String]) -> Result<(), RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        for date in dates {
            state.dirty.remove(date);
        }
        Ok(())
    }

    async fn users_with_work(&self, _limit: usize) -> Result<Vec<String>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        let busy = !state.dirty.is_empty()
            || state.jobs.values().any(JobStatus::is_running)
            || state.rules_broken.is_some();
        Ok(if busy { vec![state.user_id.clone()] } else { Vec::new() })
    }

    async fn replace_day(&self, user_id: &str, day: &DayDigest) -> Result<(), RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        state.replace_day_calls.push(day.local_date.clone());
        state.days.insert(day.local_date.clone(), day.clone());
        if state.reclassify_on_replace_day {
            // A taxonomy edit lands mid-rebuild: the restamp is now in flight
            // and this tick's stamps may already be stale.
            state.jobs.insert(
                JOB_RECLASSIFY.to_string(),
                JobStatus {
                    status: JOB_RUNNING.to_string(),
                    cursor: String::new(),
                },
            );
        }
        Ok(())
    }

    async fn load_events_touching(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<DigestEvent>, RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        // The window is recorded (not filtered): the tests assert how many
        // loads a dirty set produced and how wide they were.
        state
            .load_windows
            .push((from_date.to_string(), to_date.to_string()));
        Ok(state.events.clone())
    }

    async fn primary_time_zone(&self, _user_id: &str) -> Result<Option<String>, RepoError> {
        Ok(self.state.lock().expect("fake lock").primary_tz.clone())
    }

    async fn latest_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        // The persisted meta rows ARE `state.days` in this fake.
        Ok(state.days.values().next_back().map(|day| day.time_zone.clone()))
    }

    async fn latest_zone_before(
        &self,
        user_id: &str,
        date: &str,
    ) -> Result<Option<String>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        // The persisted meta rows ARE `state.days` in this fake.
        Ok(state
            .days
            .range(..date.to_string())
            .next_back()
            .map(|(_, day)| day.time_zone.clone()))
    }

    async fn job_status(&self, user_id: &str, kind: &str) -> Result<Option<JobStatus>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(state.jobs.get(kind).cloned())
    }

    async fn upsert_job_status(
        &self,
        user_id: &str,
        kind: &str,
        status: &str,
        cursor: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        let _ = now_rfc3339;
        state.jobs.insert(
            kind.to_string(),
            JobStatus {
                status: status.to_string(),
                cursor: cursor.to_string(),
            },
        );
        Ok(())
    }

    async fn reclassify_batch(
        &self,
        user_id: &str,
        after_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        if state.rules_broken.as_deref() == Some("reclassify") {
            return Err(backend("reclassify batch failed"));
        }
        Ok(state
            .rows
            .iter()
            .filter(|row| row.id.as_str() > after_id)
            .take(limit)
            .map(FakeRow::to_batch_row)
            .collect())
    }

    async fn set_category(&self, id: &str, category_id: Option<&str>) -> Result<(), RepoError> {
        let mut state = self.state.lock().expect("fake lock");
        let stamp = category_id.map(|value| value.to_string());
        state.stamps.insert(id.to_string(), stamp.clone());
        // The verification scan reads the **stored** stamp, so the fake stores
        // it too — recording the write is not enough.
        if let Some(row) = state.rows.iter_mut().find(|row| row.id == id) {
            row.category_id = stamp;
        }
        Ok(())
    }

    async fn unstamped_batch(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<ReclassifyRow>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        if state.rules_broken.as_deref() == Some("verify") {
            return Err(backend("unstamped verification failed"));
        }
        Ok(state
            .rows
            .iter()
            .filter(|row| row.category_id.is_none())
            .take(limit)
            .map(FakeRow::to_batch_row)
            .collect())
    }

    async fn has_unstamped_event(&self, user_id: &str) -> Result<bool, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(state.rows.iter().any(|row| row.category_id.is_none()))
    }

    async fn sleep_target_secs(&self, _user_id: &str) -> Result<Option<i64>, RepoError> {
        Ok(self.state.lock().expect("fake lock").sleep_target)
    }

    async fn has_meta(&self, user_id: &str) -> Result<bool, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(!state.days.is_empty())
    }

    async fn has_stale_meta(
        &self,
        user_id: &str,
        algo_version: i64,
    ) -> Result<bool, RepoError> {
        let state = self.state.lock().expect("fake lock");
        assert_eq!(user_id, state.user_id);
        Ok(state
            .days
            .values()
            .any(|day| day.computed_at.is_empty() || algo_version != ALGO_VERSION))
    }

    async fn earliest_event_date(&self, _user_id: &str) -> Result<Option<String>, RepoError> {
        let state = self.state.lock().expect("fake lock");
        if let Some(broken) = state.rules_broken.as_deref() {
            if broken == "floor" {
                return Err(backend("earliest event date failed"));
            }
        }
        Ok(state
            .events
            .iter()
            .filter_map(|event| event.start_rfc3339.get(0..10).map(str::to_string))
            .min())
    }
}

// ──────────────────────────────────────────
// Taxonomy fake
// ──────────────────────────────────────────

pub fn category(id: &str, title: &str, sort_order: i64) -> TaskCategory {
    TaskCategory {
        id: id.to_string(),
        user_id: "u-1".to_string(),
        list_id: None,
        parent_id: None,
        title: title.to_string(),
        slug: title.to_lowercase(),
        color: String::new(),
        is_productive: false,
        google_calendar_id: None,
        sort_order,
        is_untracked: false,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        deleted_at: None,
    }
}

pub fn pattern(category_id: &str, regex: &str, sort_order: i64) -> TaskCategoryPattern {
    TaskCategoryPattern {
        id: format!("{category_id}-p{sort_order}"),
        category_id: category_id.to_string(),
        regex: regex.to_string(),
        google_calendar_id: None,
        sort_order,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

pub struct FakeTaxonomyRepo {
    pub categories: Vec<TaskCategory>,
    pub patterns: Vec<TaskCategoryPattern>,
    pub fail: Mutex<bool>,
}

impl FakeTaxonomyRepo {
    pub fn empty() -> Self {
        Self {
            categories: Vec::new(),
            patterns: Vec::new(),
            fail: Mutex::new(false),
        }
    }

    pub fn with(categories: Vec<TaskCategory>, patterns: Vec<TaskCategoryPattern>) -> Self {
        Self {
            categories,
            patterns,
            fail: Mutex::new(false),
        }
    }
}

#[async_trait(?Send)]
impl TaskCategoryRepo for FakeTaxonomyRepo {
    async fn list_by_user_id(&self, _user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
        Ok(self.categories.clone())
    }
    async fn get_by_id(&self, _id: &str) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn insert(&self, _category: NewTaskCategory) -> Result<TaskCategory, RepoError> {
        Err(backend("not a writer"))
    }
    async fn update(
        &self,
        _id: &str,
        _updates: &UpdateTaskCategory,
    ) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn soft_delete(&self, _id: &str, _now: &str) -> Result<(), RepoError> {
        Ok(())
    }
    async fn count_by_user_id(&self, _user_id: &str) -> Result<i64, RepoError> {
        Ok(self.categories.len() as i64)
    }
    async fn count_children(&self, _id: &str) -> Result<i64, RepoError> {
        Ok(0)
    }
    async fn get_untracked(&self, _user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn list_patterns_by_category_id(
        &self,
        category_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        Ok(self
            .patterns
            .iter()
            .filter(|pattern| pattern.category_id == category_id)
            .cloned()
            .collect())
    }
    async fn list_patterns_by_user_id(
        &self,
        _user_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        if *self.fail.lock().expect("fake lock") {
            return Err(backend("taxonomy load failed"));
        }
        Ok(self.patterns.clone())
    }
    async fn replace_patterns(
        &self,
        _category_id: &str,
        _patterns: Vec<NewTaskCategoryPattern>,
    ) -> Result<(), RepoError> {
        Err(backend("not a writer"))
    }
    async fn delete_patterns_by_category_id(
        &self,
        _category_id: &str,
    ) -> Result<(), RepoError> {
        Ok(())
    }
}

/// A repo whose *categories* read fails — the sleep-category lookup path.
pub struct FailingCategoryListRepo;

#[async_trait(?Send)]
impl TaskCategoryRepo for FailingCategoryListRepo {
    async fn list_by_user_id(&self, _user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
        Err(backend("categories failed"))
    }
    async fn get_by_id(&self, _id: &str) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn insert(&self, _category: NewTaskCategory) -> Result<TaskCategory, RepoError> {
        Err(backend("not a writer"))
    }
    async fn update(
        &self,
        _id: &str,
        _updates: &UpdateTaskCategory,
    ) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn soft_delete(&self, _id: &str, _now: &str) -> Result<(), RepoError> {
        Ok(())
    }
    async fn count_by_user_id(&self, _user_id: &str) -> Result<i64, RepoError> {
        Ok(0)
    }
    async fn count_children(&self, _id: &str) -> Result<i64, RepoError> {
        Ok(0)
    }
    async fn get_untracked(&self, _user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
        Ok(None)
    }
    async fn list_patterns_by_category_id(
        &self,
        _category_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        Err(backend("patterns failed"))
    }
    async fn list_patterns_by_user_id(
        &self,
        _user_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        Err(backend("patterns failed"))
    }
    async fn replace_patterns(
        &self,
        _category_id: &str,
        _patterns: Vec<NewTaskCategoryPattern>,
    ) -> Result<(), RepoError> {
        Err(backend("not a writer"))
    }
    async fn delete_patterns_by_category_id(
        &self,
        _category_id: &str,
    ) -> Result<(), RepoError> {
        Ok(())
    }
}

// ──────────────────────────────────────────
// Tests
// ──────────────────────────────────────────

fn poll<F: std::future::Future>(future: F) -> F::Output {
    pollster::block_on(future)
}

fn today() -> i64 {
    tick_now()
}

#[test]
fn the_first_tick_starts_a_backfill_and_rebuilds_a_budget() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-01", "2026-01-02");
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(
        &repo,
        &taxonomy,
        "u-1",
        2,
        today(),
    ));

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.backfill_started, "no meta + an event ⇒ backfill starts");
    assert_eq!(report.rebuilt_days, 2, "the budget caps the days");
    assert_eq!(report.reclassified, 0);
    assert!(!report.backfill_done, "more days remain");
    assert!(report.remaining, "the backfill is still running");

    let job = repo.job(JOB_BACKFILL).expect("backfill row");
    assert_eq!(job.status, JOB_RUNNING);
    assert_eq!(job.cursor, "2026-01-02", "the cursor advances past the rebuilt days");
    assert_eq!(
        repo.state().replace_day_calls,
        vec!["2025-12-31".to_string(), "2026-01-01".to_string()],
        "the backfill starts one day before the earliest event"
    );
}

#[test]
fn the_backfill_runs_to_today_and_finishes() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-05", "2026-01-05");
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_RUNNING.to_string(),
                cursor: "2026-01-01".to_string(),
            },
        );
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    // A 100-day budget still stops at today (2026-01-10).
    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 100, today()));
    assert!(report.backfill_done, "{:?}", report.errors);
    assert_eq!(report.rebuilt_days, 10, "Jan 1 through today");
    assert_eq!(
        repo.job(JOB_BACKFILL).map(|job| job.status),
        Some(JOB_DONE.to_string())
    );
    let calls = repo.state().replace_day_calls.clone();
    assert_eq!(calls.first().map(String::as_str), Some("2026-01-01"));
    assert_eq!(calls.last().map(String::as_str), Some("2026-01-10"));
    assert_eq!(calls.len(), 10, "no future day is ever rebuilt");
}

#[test]
fn dirty_days_are_rebuilt_and_cleared() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.dirty.insert("2026-01-02".to_string(), "2026-01-01T00:00:00Z".to_string());
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.backfill_started == false, "a done job is never restarted");
    // The dirty past day plus today, which every tick keeps open.
    assert_eq!(report.rebuilt_days, 2);
    assert_eq!(
        repo.dirty_dates(),
        vec!["2026-01-10".to_string()],
        "a finished day is cleared; today stays dirty for the next tick"
    );
    assert!(!report.remaining, "today alone is not a reason to come back");
    let day = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert!(day.tracked_seconds > 0);
}


#[test]
fn a_future_event_is_not_rebuilt_until_its_date() {
    // A future-dated event dirties a future local date. Rebuilding it now
    // would clip every segment at `now`, write a zero day, and clear the dirty
    // row — so the date would never be recomputed once it arrives.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-02-01", "2026-02-01");
        state.dirty.insert("2026-02-01".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rebuilt_days, 1, "only today is rebuilt");
    assert_eq!(
        repo.state().replace_day_calls,
        vec!["2026-01-10".to_string()],
        "the future date is never replaced"
    );
    assert_eq!(
        repo.dirty_dates(),
        vec!["2026-01-10".to_string(), "2026-02-01".to_string()],
        "the future row waits for the tick that runs on its own date"
    );
}

#[test]
fn todays_tail_stays_dirty_until_the_day_ends() {
    // Today is always clipped at `now`: clearing its dirty row would freeze the
    // tail at the last write.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-10", "2026-01-10");
        state.dirty.insert("2026-01-10".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rebuilt_days, 1);
    assert_eq!(repo.dirty_dates(), vec!["2026-01-10".to_string()]);
    assert!(
        !report.remaining,
        "today's own dirty row must not re-enqueue the tick forever"
    );

    // A quiet user — every event in the past — still gets today's tail
    // recomputed, off the persisted `daily_meta` row.
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.dirty.clear();
        state.replace_day_calls.clear();
    });
    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rebuilt_days, 1, "today alone");
    assert_eq!(
        repo.state().replace_day_calls,
        vec!["2026-01-10".to_string()],
        "a quiet user still gets today recomputed"
    );
    assert_eq!(repo.dirty_dates(), vec!["2026-01-10".to_string()]);
}

#[test]
fn the_day_that_just_ended_is_finalized_on_the_next_tick() {
    // Yesterday stays dirty across local midnight (it was today's row, and
    // today is never cleared). The next tick rebuilds it **unclipped** — the
    // whole event is in the past now — and only then clears it, leaving the
    // new today dirty in its place.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.primary_tz = Some("UTC".to_string());
        state.events = vec![event(
            "evening",
            "evening",
            "2026-01-02T20:00:00Z",
            "2026-01-02T22:00:00Z",
            "UTC",
            Some("work"),
        )];
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    // 21:00 on the 2nd: the day is still running, so the event is clipped.
    let report = poll(run_digest_tick_user(
        &repo,
        &taxonomy,
        "u-1",
        7,
        utc("2026-01-02T21:00:00Z"),
    ));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let clipped = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert_eq!(work_seconds(&clipped), 3_600, "one hour has elapsed");
    assert_eq!(repo.dirty_dates(), vec!["2026-01-02".to_string()]);

    // 06:00 on the 3rd: the 2nd is a finished day.
    let report = poll(run_digest_tick_user(
        &repo,
        &taxonomy,
        "u-1",
        7,
        utc("2026-01-03T06:00:00Z"),
    ));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let finished = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert_eq!(work_seconds(&finished), 7_200, "the whole event, no clip");
    assert_eq!(
        repo.dirty_dates(),
        vec!["2026-01-03".to_string()],
        "the finished day is cleared; the new today is left dirty"
    );
    assert!(!report.remaining);
}

#[test]
fn a_sparse_dirty_set_rebuilds_in_separate_runs() {
    // Two dirty dates three years apart must not load (or share a digest pool
    // with) everything in between.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = vec![
            event(
                "old",
                "old",
                "2023-01-05T10:00:00Z",
                "2023-01-05T11:00:00Z",
                "UTC",
                Some("work"),
            ),
            event(
                "new",
                "new",
                "2026-01-07T10:00:00Z",
                "2026-01-07T11:00:00Z",
                "UTC",
                Some("work"),
            ),
        ];
        state.dirty.insert("2023-01-05".to_string(), "x".to_string());
        state.dirty.insert("2026-01-07".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let windows = repo.state().load_windows.clone();
    assert_eq!(windows.len(), 2, "one load per run: {windows:?}");
    assert_eq!(
        windows[0],
        ("2023-01-03".to_string(), "2023-01-07".to_string()),
        "the sparse run is loaded alone, padded by ±2"
    );
    assert_eq!(
        windows[1],
        ("2026-01-05".to_string(), "2026-01-12".to_string()),
        "2026-01-07 and today are one run (gap ≤ 4)"
    );
    assert!(
        windows[0].1.as_str() < "2024-01-01",
        "the first load stops in 2023: {windows:?}"
    );
    assert!(
        windows[1].0.as_str() > "2025-01-01",
        "the second load starts in 2026: {windows:?}"
    );
    assert_eq!(
        repo.state().replace_day_calls,
        vec![
            "2023-01-05".to_string(),
            "2026-01-07".to_string(),
            "2026-01-10".to_string()
        ]
    );
    let old = repo.state().days.get("2023-01-05").cloned().expect("day");
    assert_eq!(work_seconds(&old), 3_600, "the 2023 event still lands");
}

#[test]
fn the_rebuild_digests_the_neighbour_wake_and_persists_only_the_dirty_day() {
    // dirty {2026-01-02}; events on the 2nd evening and the 3rd morning. The
    // night that wakes on the 3rd owns a pre-midnight half on the 2nd, so the
    // rebuild has to digest the 3rd — but it must not write it.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.primary_tz = Some("UTC".to_string());
        state.events = vec![
            event(
                "evening",
                "evening",
                "2026-01-02T19:00:00Z",
                "2026-01-02T20:00:00Z",
                "UTC",
                Some("work"),
            ),
            event(
                "morning",
                "morning",
                "2026-01-03T09:00:00Z",
                "2026-01-03T10:00:00Z",
                "UTC",
                Some("work"),
            ),
        ];
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let day = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert_eq!(
        day.sleep_seconds, 23_400,
        "the neighbour's 21:00–24:00 half (10_800) joins the night that wakes on D"
    );
    assert!(
        !repo.state().days.contains_key("2026-01-03"),
        "the digested neighbour is never persisted"
    );
}

#[test]
fn a_rebuilt_day_inherits_the_persisted_zone() {
    // The day before the dirty day is persisted as Asia/Kolkata and the dirty
    // day itself has no events: the rebuild must inherit the stored zone
    // instead of falling through to the primary calendar's UTC.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.primary_tz = Some("UTC".to_string());
        state
            .days
            .insert("2026-01-01".to_string(), stored_day("2026-01-01", "Asia/Kolkata"));
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let day = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert_eq!(
        day.time_zone, "Asia/Kolkata",
        "the zone comes from persisted history, not from the batch"
    );
}

#[test]
fn a_stale_algo_version_restarts_the_backfill_but_not_the_reclassify() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-05", "2026-01-05");
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
        // A finished restamp: its stamps are still valid.
        state.jobs.insert(
            JOB_RECLASSIFY.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
        // A day computed by an older algorithm.
        state
            .days
            .insert("2026-01-05".to_string(), stale_day("2026-01-05"));
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.backfill_started, "stale meta ⇒ backfill runs again");
    assert_eq!(repo.job(JOB_BACKFILL).map(|job| job.cursor), Some(String::new()));
    assert_eq!(
        repo.job(JOB_RECLASSIFY).map(|job| job.status),
        Some(JOB_DONE.to_string()),
        "only the rollup changed: the stamps must not be rescanned"
    );
    assert_eq!(report.reclassified, 0);
    assert!(repo.state().stamps.is_empty(), "no stamp was rewritten");
}

fn stale_day(date: &str) -> DayDigest {
    DayDigest {
        local_date: date.to_string(),
        time_zone: "Asia/Kolkata".to_string(),
        tracked_seconds: 0,
        sleep_seconds: 0,
        // `has_stale_meta` in the fake reports a blank stamp as stale.
        computed_at: String::new(),
        category_seconds: Vec::new(),
        hour_seconds: Vec::new(),
        nights: Vec::new(),
    }
}

/// One unstamped "Deep Work" row, matched by the `work` pattern below.
fn deep_work_row(id: &str) -> FakeRow {
    FakeRow {
        id: id.to_string(),
        title: "Deep Work".to_string(),
        google_calendar_id: "primary@example.com".to_string(),
        start_time: "2026-01-02T10:00:00Z".to_string(),
        end_time: "2026-01-02T11:00:00Z".to_string(),
        category_id: None,
    }
}

fn running_job() -> JobStatus {
    JobStatus {
        status: JOB_RUNNING.to_string(),
        cursor: String::new(),
    }
}

/// A finished job: the tick will not restart it, so the rebuild branch runs.
fn done_job() -> JobStatus {
    JobStatus {
        status: JOB_DONE.to_string(),
        cursor: String::new(),
    }
}

/// One already-computed day, as the fake's `daily_meta` row.
fn stored_day(date: &str, zone: &str) -> DayDigest {
    DayDigest {
        time_zone: zone.to_string(),
        computed_at: "2026-01-01T00:00:00Z".to_string(),
        ..stale_day(date)
    }
}

/// The `work` category's seconds on a rebuilt day.
fn work_seconds(day: &DayDigest) -> i64 {
    day.category_seconds
        .iter()
        .filter(|row| row.category_id == "work")
        .map(|row| row.seconds)
        .sum()
}

fn work_taxonomy() -> FakeTaxonomyRepo {
    FakeTaxonomyRepo::with(
        vec![category("work", "Work", 0)],
        vec![pattern("work", "^Deep Work$", 0)],
    )
}

#[test]
fn the_first_backfill_also_queues_a_reclassify() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
    });

    let job = poll(ensure_backfill_job(&repo, "u-1", "2026-01-10T06:00:00Z")).expect("job");

    assert!(job.started);
    assert_eq!(repo.job(JOB_BACKFILL), Some(running_job()));
    assert_eq!(
        repo.job(JOB_RECLASSIFY),
        Some(running_job()),
        "the events are unstamped until a taxonomy edit: history is restamped first"
    );
}

#[test]
fn a_running_reclassify_is_not_reset_by_the_first_backfill() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.jobs.insert(
            JOB_RECLASSIFY.to_string(),
            JobStatus {
                status: JOB_RUNNING.to_string(),
                cursor: "e-9".to_string(),
            },
        );
    });

    let job = poll(ensure_backfill_job(&repo, "u-1", "2026-01-10T06:00:00Z")).expect("job");

    assert!(job.started, "the backfill is still due");
    assert_eq!(
        repo.job(JOB_RECLASSIFY),
        Some(JobStatus {
            status: JOB_RUNNING.to_string(),
            cursor: "e-9".to_string(),
        }),
        "a mid-scan cursor is never rewound to the first event"
    );
}

#[test]
fn an_unfinished_reclassify_holds_the_backfill_until_it_drains() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.jobs.insert(JOB_BACKFILL.to_string(), running_job());
        state.jobs.insert(JOB_RECLASSIFY.to_string(), running_job());
        // A full batch: the scan is not finished, so no day may be rolled up.
        state.rows = (0..RECLASSIFY_BATCH_LIMIT)
            .map(|index| deep_work_row(&format!("e-{index:03}")))
            .collect();
    });
    let taxonomy = work_taxonomy();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.reclassified, RECLASSIFY_BATCH_LIMIT);
    assert_eq!(report.rebuilt_days, 0, "no day is rolled up mid-restamp");
    assert!(
        repo.state().replace_day_calls.is_empty(),
        "an unfinished reclassify owns the whole tick"
    );
    assert!(!report.backfill_done);
    assert_eq!(
        repo.job(JOB_BACKFILL).map(|job| job.cursor),
        Some(String::new()),
        "the backfill did not move"
    );
    assert!(report.remaining, "the caller must come back");

    // The next tick drains the empty batch and hands the budget to the backfill.
    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.reclassify_done);
    assert_eq!(report.reclassified, 0);
    assert_eq!(report.rebuilt_days, 7, "the backfill finally spends the budget");
    assert_eq!(
        repo.job(JOB_BACKFILL).map(|job| job.cursor),
        Some("2026-01-08".to_string())
    );
}

#[test]
fn reclassify_restamps_changed_rows_and_marks_their_days_dirty() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.jobs.insert(
            JOB_RECLASSIFY.to_string(),
            JobStatus {
                status: JOB_RUNNING.to_string(),
                cursor: String::new(),
            },
        );
        state.rows = vec![
            FakeRow {
                id: "e-1".to_string(),
                title: "Deep Work".to_string(),
                google_calendar_id: "primary@example.com".to_string(),
                start_time: "2026-01-02T10:00:00Z".to_string(),
                end_time: "2026-01-02T11:00:00Z".to_string(),
                category_id: None,
            },
            FakeRow {
                id: "e-2".to_string(),
                title: "Deep Work".to_string(),
                google_calendar_id: "primary@example.com".to_string(),
                start_time: "2026-01-03T10:00:00Z".to_string(),
                end_time: "2026-01-03T11:00:00Z".to_string(),
                // Already stamped with the winning category: no write, no
                // dirty mark.
                category_id: Some("work".to_string()),
            },
        ];
    });
    let taxonomy = FakeTaxonomyRepo::with(
        vec![category("work", "Work", 0)],
        vec![pattern("work", "^Deep Work$", 0)],
    );

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.reclassified, 1);
    assert_eq!(repo.state().stamps.get("e-1"), Some(&Some("work".to_string())));
    assert_eq!(repo.state().stamps.get("e-2"), None, "an unchanged stamp is skipped");
    assert_eq!(
        repo.job(JOB_RECLASSIFY).map(|job| job.cursor),
        Some("e-2".to_string()),
        "the cursor advances to the last id"
    );
    // `e-1` is on 2026-01-02, so the dirty mark covers its day and the ±1
    // sleep neighbours — and nothing else: had the unchanged `e-2` (on the
    // 3rd) been restamped, its neighbours would appear here too.
    assert_eq!(
        repo.state().replace_day_calls,
        vec![
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-03".to_string()
        ],
        "the drained reclassify falls through to the dirty rebuild"
    );

    // The next tick drains the empty batch and marks the job done.
    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.reclassify_done, "{:?}", report.errors);
    assert_eq!(
        repo.job(JOB_RECLASSIFY).map(|job| job.status),
        Some(JOB_DONE.to_string())
    );
    assert!(!report.remaining);
}

#[test]
fn a_taxonomy_load_failure_leaves_reclassify_running_and_the_cursor_unmoved() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.jobs.insert(
            JOB_RECLASSIFY.to_string(),
            JobStatus {
                status: JOB_RUNNING.to_string(),
                cursor: "e-0".to_string(),
            },
        );
        state.rows = vec![FakeRow {
            id: "e-1".to_string(),
            title: "Deep Work".to_string(),
            google_calendar_id: "primary@example.com".to_string(),
            start_time: "2026-01-02T10:00:00Z".to_string(),
            end_time: "2026-01-02T11:00:00Z".to_string(),
            category_id: Some("work".to_string()),
        }];
    });
    let taxonomy = FakeTaxonomyRepo::with(
        vec![category("work", "Work", 0)],
        vec![pattern("work", "^Deep Work$", 0)],
    );
    *taxonomy.fail.lock().expect("fake lock") = true;

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(!report.errors.is_empty(), "the failure is reported");
    assert_eq!(
        repo.job(JOB_RECLASSIFY),
        Some(JobStatus {
            status: JOB_RUNNING.to_string(),
            cursor: "e-0".to_string(),
        }),
        "the job stays running with its cursor so cron retries"
    );
    assert!(repo.state().stamps.is_empty(), "no stamp was blanked");
}

#[test]
fn status_reports_the_jobs_and_the_dirty_count() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.dirty.insert("2026-01-02".to_string(), "2026-01-01T00:00:00Z".to_string());
    });

    let status = poll(digest_status(&repo, "u-1", today())).expect("status");
    assert!(status.backfilling, "the first status call starts the backfill");
    assert!(
        status.reclassifying,
        "the first-run backfill queues the restamp it depends on"
    );
    assert_eq!(status.dirty_days, 1);

    // A second call sees the running job and does not re-derive anything.
    let status = poll(digest_status(&repo, "u-1", today())).expect("status");
    assert!(status.backfilling);
    assert!(status.reclassifying);
}

#[test]
fn a_sleep_category_is_resolved_for_the_synthetic_rows() {
    let categories = vec![category("cat-1", "Sleep", 0)];
    assert_eq!(resolve_sleep_category_id(&categories).as_deref(), Some("cat-1"));
    assert_eq!(resolve_sleep_category_id(&[]), None);
    assert_eq!(
        resolve_sleep_category_id(&[category("cat-2", "Work", 1)]),
        None
    );
}

#[test]
fn one_tick_at_most_one_user_and_a_no_op_when_idle() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    // No dirty days and no running job: nothing happens at all.
    let report = poll(run_digest_tick(&repo, &taxonomy, None, 7, today()));
    assert_eq!(report.user_id, None);
    assert_eq!(report.rebuilt_days, 0);
    assert!(repo.state().replace_day_calls.is_empty());

    repo.mutate(|state| {
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
    });
    let report = poll(run_digest_tick(&repo, &taxonomy, None, 7, today()));
    assert_eq!(report.user_id.as_deref(), Some("u-1"));
    assert_eq!(report.rebuilt_days, 1);
}

#[test]
fn a_failing_sleep_category_read_still_rebuilds() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
    });

    let report = poll(run_digest_tick_user(
        &repo,
        &FailingCategoryListRepo,
        "u-1",
        7,
        today(),
    ));
    assert_eq!(
        report.rebuilt_days, 2,
        "the rebuild degrades, it does not stall (the dirty day and today)"
    );
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("sleep category"));
}

#[test]
fn a_rebuild_is_idempotent_across_a_changed_event_set() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let mut payloads = Vec::new();
    for round in 0..2 {
        repo.mutate(|state| {
            state.dirty.insert("2026-01-02".to_string(), "x".to_string());
            if round == 1 {
                // A different event set for the second rebuild: an unrelated
                // event on another date. The stored day must still be identical
                // — idempotence is not just "the same call twice".
                state.events.push(event(
                    "unrelated",
                    "unrelated",
                    "2026-01-05T10:00:00Z",
                    "2026-01-05T11:00:00Z",
                    "UTC",
                    Some("work"),
                ));
            }
        });
        let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        payloads.push(
            repo.state()
                .days
                .get("2026-01-02")
                .cloned()
                .expect("day"),
        );
    }
    assert_eq!(
        repo.state()
            .replace_day_calls
            .iter()
            .filter(|date| date.as_str() == "2026-01-02")
            .count(),
        2,
        "the dirty day is rebuilt once per tick (today is rebuilt too)"
    );
    // The second rebuild digested an extra event and still wrote the same day.
    assert_eq!(payloads[0], payloads[1]);
    assert!(payloads[0].tracked_seconds > 0);
}

#[test]
fn a_reclassify_that_starts_mid_rebuild_keeps_the_days_dirty() {
    // A taxonomy edit from another request can enqueue a restamp while this
    // tick is writing days. Those days may have been rebuilt from the
    // pre-reclassify stamps, so clearing them would freeze a stale rollup: the
    // clear step re-reads the job status and keeps them dirty instead.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state
            .dirty
            .insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
        // No reclassify at the top of the tick: it starts inside `replace_day`.
        state.reclassify_on_replace_day = true;
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.rebuilt_days, 2, "the dirty day and today");
    assert_eq!(report.kept_dirty, 1, "the finished day stays dirty");
    assert_eq!(
        repo.dirty_dates(),
        vec!["2026-01-02".to_string(), "2026-01-10".to_string()],
        "a restamp that started mid-rebuild keeps the days dirty"
    );
    assert!(report.remaining, "the next tick must redo those days");
}

#[test]
fn reclassify_does_not_finish_while_null_category_rows_remain() {
    // The walk is `e.id > ?`, and ids are random UUIDs: a row that lands during
    // the scan can sort **before** the cursor and is never visited again. The
    // verification scan must find it before the job may be called done.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.jobs.insert(
            JOB_RECLASSIFY.to_string(),
            JobStatus {
                status: JOB_RUNNING.to_string(),
                cursor: "m".to_string(),
            },
        );
        state.rows = vec![
            // Sorts before the cursor: the walk cannot see it.
            deep_work_row("a-hidden"),
            // The walk's last row (a short batch).
            deep_work_row("z-visible"),
        ];
    });
    let taxonomy = work_taxonomy();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        repo.state().stamps.get("a-hidden"),
        Some(&Some("work".to_string())),
        "the hidden row is stamped before the job may finish"
    );
    assert_eq!(
        repo.state().stamps.get("z-visible"),
        Some(&Some("work".to_string()))
    );
    assert!(report.reclassify_done, "nothing unstamped is left");
    assert_eq!(
        repo.job(JOB_RECLASSIFY).map(|job| job.status),
        Some(JOB_DONE.to_string())
    );
    // The stamped row's day and its ±1 neighbours were queued for a rebuild —
    // the whole point of stamping it before finishing.
    assert_eq!(
        repo.state().replace_day_calls,
        vec![
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-03".to_string()
        ],
        "the hidden row's days are dirty and get rebuilt"
    );
}

#[test]
fn a_done_reclassify_restarts_only_while_unstamped_events_remain() {
    // Done + a living event with no leaf stamp: the scan must run again — an
    // event synced in after it finished would otherwise never be classified.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.rows = vec![deep_work_row("e-1")];
        state.jobs.insert(JOB_RECLASSIFY.to_string(), done_job());
    });
    let job = poll(ensure_backfill_job(&repo, "u-1", "2026-01-10T06:00:00Z")).expect("job");
    assert!(job.started);
    assert_eq!(
        repo.job(JOB_RECLASSIFY),
        Some(running_job()),
        "an unstamped row restarts the finished scan"
    );

    // Done + every row stamped: the stamps are valid, so nothing is restarted.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = day_events("2026-01-02", "2026-01-02");
        state.rows = vec![FakeRow {
            category_id: Some("work".to_string()),
            ..deep_work_row("e-1")
        }];
        state.jobs.insert(JOB_RECLASSIFY.to_string(), done_job());
    });
    let job = poll(ensure_backfill_job(&repo, "u-1", "2026-01-10T06:00:00Z")).expect("job");
    assert!(job.started, "the first-run backfill is still due");
    assert_eq!(
        repo.job(JOB_RECLASSIFY),
        Some(done_job()),
        "a finished scan with nothing left is left alone"
    );
}

#[test]
fn the_rebuild_uses_the_users_sleep_duration_target() {
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.primary_tz = Some("Asia/Kolkata".to_string());
        state.sleep_target = Some(8 * 3600);
        state.events = vec![
            event(
                "last",
                "last",
                "2026-01-01T18:00:00+05:30",
                "2026-01-01T20:00:00+05:30",
                "Asia/Kolkata",
                Some("work"),
            ),
            event(
                "first",
                "first",
                "2026-01-02T09:00:00+05:30",
                "2026-01-02T10:00:00+05:30",
                "Asia/Kolkata",
                Some("work"),
            ),
        ];
        state
            .dirty
            .insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(JOB_BACKFILL.to_string(), done_job());
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let day = repo.state().days.get("2026-01-02").cloned().expect("day");
    let night = day
        .nights
        .iter()
        .find(|night| night.wake_date == "2026-01-02")
        .expect("the estimated night");
    assert_eq!(night.source, SleepSource::EstNone);
    assert_eq!(
        night.seconds,
        8 * 3600,
        "the rule's target, not the 6.5h fallback"
    );
}

#[test]
fn kolkata_events_reach_the_stored_day() {
    // A thin end-to-end check that the tick's events really flow through the
    // pipeline into `replace_day`.
    let repo = FakeDigestRepo::new("u-1");
    repo.mutate(|state| {
        state.events = vec![kolkata_event(
            "k-1",
            "2026-01-02T10:00:00+05:30",
            "2026-01-02T11:00:00+05:30",
        )];
        state.dirty.insert("2026-01-02".to_string(), "x".to_string());
        state.jobs.insert(
            JOB_BACKFILL.to_string(),
            JobStatus {
                status: JOB_DONE.to_string(),
                cursor: String::new(),
            },
        );
    });
    let taxonomy = FakeTaxonomyRepo::empty();

    let report = poll(run_digest_tick_user(&repo, &taxonomy, "u-1", 7, today()));
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let day = repo.state().days.get("2026-01-02").cloned().expect("day");
    assert_eq!(day.time_zone, "Asia/Kolkata");
    let work: i64 = day
        .category_seconds
        .iter()
        .filter(|row| row.category_id == "work")
        .map(|row| row.seconds)
        .sum();
    assert_eq!(work, 3600, "one hour of tracked work");
}

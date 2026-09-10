use std::collections::HashMap;
use std::sync::Mutex;

use crate::calendar::apply::row_from_new_event;
use crate::config::OAuthConfig;
use crate::models::{
    CalendarEvent, CalendarEventOperation, GoogleCalendar, GoogleOAuthToken, NewCalendar,
    NewCalendarEvent, NewCalendarEventOperation, NewEventInput, NewToken, NewWatchChannel,
    OP_STATUS_GOOGLE_COMMITTED, OP_STATUS_PENDING, WatchChannel,
};
use crate::oauth::{HttpClient, HttpError};
use crate::repo::{
    CalendarEventOperationRepo, CalendarEventRepo, CalendarRepo, RepoError, TokenRepo,
    WatchChannelRepo,
};
use crate::token::GoogleAccess;

// ──────────────────────────────────────────
// Fakes
// ──────────────────────────────────────────

/// Scripted HTTP fake: `routes` are `(url-substring, status, body)` in
/// match order (permanent — not consumed; replica/window pagination reuses
/// them). `one_shots` are checked first and **removed** on match (412
/// sequences). Every call is recorded for assertions.
pub(crate) struct FakeHttp {
    pub(crate) routes: Vec<(String, u16, String)>,
    /// Consumed on first substring match (FIFO among matches).
    pub(crate) one_shots: Mutex<Vec<(String, u16, String)>>,
    pub(crate) gets: Mutex<Vec<String>>,
    pub(crate) posts: Mutex<Vec<(String, String)>>,
    pub(crate) patches: Mutex<Vec<(String, String)>>,
    /// Extra headers passed to each `patch_json_with_headers` (parallel to
    /// `patches`).
    pub(crate) patch_headers: Mutex<Vec<Vec<(String, String)>>>,
}

impl FakeHttp {
    pub(crate) fn new(routes: Vec<(&str, u16, &str)>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|(substr, status, body)| {
                    (substr.to_string(), status, body.to_string())
                })
                .collect(),
            one_shots: Mutex::new(Vec::new()),
            gets: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            patches: Mutex::new(Vec::new()),
            patch_headers: Mutex::new(Vec::new()),
        }
    }

    /// Attach one-shot routes (consumed on match) for 412/retry sequences.
    pub(crate) fn with_one_shots(self, shots: Vec<(&str, u16, &str)>) -> Self {
        *self.one_shots.lock().unwrap() = shots
            .into_iter()
            .map(|(substr, status, body)| {
                (substr.to_string(), status, body.to_string())
            })
            .collect();
        self
    }

    pub(crate) fn route(&self, url: &str) -> (u16, Vec<u8>) {
        // One-shots first: first substring match is removed and returned.
        {
            let mut shots = self.one_shots.lock().unwrap();
            if let Some(idx) = shots.iter().position(|(substr, _, _)| url.contains(substr.as_str()))
            {
                let (_substr, status, body) = shots.remove(idx);
                return (status, body.into_bytes());
            }
        }
        for (substr, status, body) in &self.routes {
            if url.contains(substr) {
                return (*status, body.clone().into_bytes());
            }
        }
        // Default for the `calendars.get` event-label backfill: any URL
        // that is a bare calendar resource (not `.../events`, not the
        // calendarList) returns "no labels", so first-import and sync tests
        // do not need to script a route for it.
        if url.contains("/calendar/v3/calendars/")
            && !url.contains("/events")
            && !url.contains("calendarList")
        {
            return (200, br#"{"labelProperties":{"eventLabels":[]}}"#.to_vec());
        }
        // Default for unscripted incremental calendarList ticks (cron):
        // empty delta, no nextSyncToken — no-op merge. Full-list URLs
        // (no syncToken) still panic so first-import tests keep their
        // explicit route.
        if url.contains("calendarList") && url.contains("syncToken=") {
            return (200, br#"{"items":[]}"#.to_vec());
        }
        panic!("no route for {url}");
    }
}

#[async_trait::async_trait(?Send)]
impl HttpClient for FakeHttp {
    async fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<Vec<u8>, HttpError> {
        Ok(Vec::new())
    }

    async fn get_bearer(&self, _url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
        Ok(Vec::new())
    }

    async fn get_bearer_raw(
        &self,
        url: &str,
        _token: &str,
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.gets.lock().unwrap().push(url.to_string());
        let (status, response) = self.route(url);
        // events.get by id: echo the path id into the JSON body (mirrors
        // Google returning the client-supplied id after a 409 insert).
        Ok((status, echo_get_event_id(url, response)))
    }

    async fn post_json(
        &self,
        url: &str,
        _token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.posts
            .lock()
            .unwrap()
            .push((url.to_string(), String::from_utf8_lossy(body).to_string()));
        let (status, response) = self.route(url);
        // Google echoes a client-supplied event id on insert. Rewrite the
        // scripted response id so journaled mint and cache key stay aligned.
        Ok((status, echo_insert_event_id(url, body, response)))
    }

    async fn patch_json(
        &self,
        url: &str,
        token: &str,
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.patch_json_with_headers(url, token, body, &[]).await
    }

    async fn patch_json_with_headers(
        &self,
        url: &str,
        _token: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<(u16, Vec<u8>), HttpError> {
        self.patches
            .lock()
            .unwrap()
            .push((url.to_string(), String::from_utf8_lossy(body).to_string()));
        self.patch_headers.lock().unwrap().push(
            extra_headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        );
        Ok(self.route(url))
    }
}

/// `events.insert` URL (collection), not `events/{id}` or `events/watch`.
fn is_events_insert_url(url: &str) -> bool {
    let Some(idx) = url.find("/events") else {
        return false;
    };
    let rest = &url[idx + "/events".len()..];
    rest.is_empty() || rest.starts_with('?')
}

/// When the request body has a string `id` and the routed response is JSON
/// with an `id`, rewrite the response id to the request id (insert only).
fn echo_insert_event_id(url: &str, request_body: &[u8], response: Vec<u8>) -> Vec<u8> {
    if !is_events_insert_url(url) {
        return response;
    }
    let Ok(req) = serde_json::from_slice::<serde_json::Value>(request_body) else {
        return response;
    };
    let Some(req_id) = req.get("id").and_then(|v| v.as_str()) else {
        return response;
    };
    rewrite_json_id(response, req_id)
}

/// `.../events/{id}` GET: rewrite response `id` to the path segment.
fn echo_get_event_id(url: &str, response: Vec<u8>) -> Vec<u8> {
    // Path form: .../events/{id} optionally followed by ?query
    let Some(idx) = url.find("/events/") else {
        return response;
    };
    let rest = &url[idx + "/events/".len()..];
    let id = rest.split('?').next().unwrap_or(rest);
    if id.is_empty() || id.contains('/') {
        return response;
    }
    // Percent-decode is unnecessary for our minted ids (hex).
    rewrite_json_id(response, id)
}

fn rewrite_json_id(response: Vec<u8>, id: &str) -> Vec<u8> {
    let Ok(mut resp) = serde_json::from_slice::<serde_json::Value>(&response) else {
        return response;
    };
    if resp.get("id").is_none() {
        return response;
    }
    resp["id"] = serde_json::json!(id);
    serde_json::to_vec(&resp).unwrap_or(response)
}

/// In-memory calendar repo: `upsert_batch` upserts by natural key
/// `(user_id, google_calendar_id)` (like D1), so incremental calendarList
/// tests do not invent duplicate rows. Lease methods mirror the V1 SQL
/// semantics so replica tests exercise real fencing.
pub(crate) struct FakeCalendarRepo {
    pub(crate) stored: Mutex<Vec<GoogleCalendar>>,
    pub(crate) upserted: Mutex<Vec<NewCalendar>>,
    pub(crate) sync_states: Mutex<Vec<(String, String, String)>>,
    pub(crate) disabled: Mutex<Vec<(String, bool)>>,
    pub(crate) label_updates: Mutex<Vec<(String, String)>>,
    pub(crate) next_id: Mutex<u64>,
    /// Per-user calendarList.list sync tokens.
    pub(crate) calendar_list_tokens: Mutex<HashMap<String, String>>,
    /// Count of `get_by_id` calls (for mid-walk lease-steal hooks).
    pub(crate) get_by_id_count: Mutex<usize>,
    /// After this many `get_by_id` calls, force `lease_owner` to `"thief"`
    /// on the matched row (None = disabled).
    pub(crate) steal_lease_after_get_by_id: Mutex<Option<usize>>,
    /// When true, `bump_dirty_requested` returns `RepoError::Backend`.
    pub(crate) fail_bump_dirty: Mutex<bool>,
    /// When true, `set_sync_enabled` returns `RepoError::Backend`.
    pub(crate) fail_set_sync_enabled: Mutex<bool>,
    /// After this many `get_by_id` calls, increment
    /// `dirty_requested_generation` on the matched row (None = disabled).
    /// Snapshot is taken before the bump so the caller still sees the
    /// pre-bump generation (mirrors mid-run dirty enqueue).
    pub(crate) bump_dirty_after_get_by_id: Mutex<Option<usize>>,
}

impl FakeCalendarRepo {
    pub(crate) fn with(calendars: Vec<GoogleCalendar>) -> Self {
        // Seed a non-empty list token for every distinct user so existing
        // cron tests take the incremental-empty path (FakeHttp default)
        // and do not orphan calendars on an unscripted full list.
        // `with(vec![])` seeds nothing → first-import tests stay full-list.
        let mut tokens = HashMap::new();
        let mut max_numeric_id = 0u64;
        for cal in &calendars {
            tokens
                .entry(cal.user_id.clone())
                .or_insert_with(|| "test-list-token".to_string());
            if let Some(n) = cal.id.strip_prefix("cal-").and_then(|s| s.parse().ok()) {
                max_numeric_id = max_numeric_id.max(n);
            }
        }
        Self {
            stored: Mutex::new(calendars),
            upserted: Mutex::new(Vec::new()),
            sync_states: Mutex::new(Vec::new()),
            disabled: Mutex::new(Vec::new()),
            label_updates: Mutex::new(Vec::new()),
            // Avoid colliding with fixture ids like `cal-1`.
            next_id: Mutex::new(max_numeric_id.saturating_add(1).max(1)),
            calendar_list_tokens: Mutex::new(tokens),
            get_by_id_count: Mutex::new(0),
            steal_lease_after_get_by_id: Mutex::new(None),
            fail_bump_dirty: Mutex::new(false),
            fail_set_sync_enabled: Mutex::new(false),
            bump_dirty_after_get_by_id: Mutex::new(None),
        }
    }

    /// Test helper: plant a foreign or same-owner lease on a stored row.
    pub(crate) fn force_lease(&self, id: &str, owner: &str, expires_rfc3339: Option<&str>) {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.lease_owner = owner.to_string();
            cal.lease_expires_at = expires_rfc3339.map(str::to_string);
        }
    }

    pub(crate) fn apply_success_fields(
        cal: &mut GoogleCalendar,
        sync_token: &str,
        query_fingerprint: &str,
        now_rfc3339: &str,
    ) {
        cal.sync_token = sync_token.to_string();
        cal.last_synced_at = Some(now_rfc3339.to_string());
        cal.last_success_at = Some(now_rfc3339.to_string());
        cal.last_attempt_at = Some(now_rfc3339.to_string());
        cal.last_error_code = String::new();
        cal.failure_streak = 0;
        cal.next_retry_at = None;
        cal.initial_sync_complete = true;
        cal.sync_status = "ready".to_string();
        cal.sync_query_fingerprint = query_fingerprint.to_string();
        cal.cache_revision += 1;
        cal.full_sync_requested = false;
        cal.updated_at = now_rfc3339.to_string();
    }

    pub(crate) fn lease_held(cal: &GoogleCalendar, owner: &str, now_rfc3339: &str) -> bool {
        if cal.lease_owner != owner {
            return false;
        }
        match cal.lease_expires_at.as_deref() {
            None => true,
            Some(exp) => exp >= now_rfc3339,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarRepo for FakeCalendarRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|cal| cal.user_id == user_id && cal.deleted_at.is_none())
            .cloned()
            .collect())
    }

    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|cal| cal.sync_enabled && cal.deleted_at.is_none())
            .cloned()
            .collect())
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let mut count = self.get_by_id_count.lock().unwrap();
        *count += 1;
        let n = *count;
        drop(count);

        let mut stored = self.stored.lock().unwrap();
        // Snapshot first so the Nth call still sees our lease / dirty gen;
        // subsequent reads observe the post-hook mutation.
        // Match D1: only living rows.
        let result = stored
            .iter()
            .find(|cal| cal.id == id && cal.deleted_at.is_none())
            .cloned();
        if let Some(threshold) = *self.steal_lease_after_get_by_id.lock().unwrap() {
            if n == threshold {
                if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                    cal.lease_owner = "thief".to_string();
                    cal.lease_expires_at = Some("2099-01-01T00:00:00Z".to_string());
                }
            }
        }
        if let Some(threshold) = *self.bump_dirty_after_get_by_id.lock().unwrap() {
            if n == threshold {
                if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
                    cal.dirty_requested_generation =
                        cal.dirty_requested_generation.saturating_add(1);
                }
            }
        }
        Ok(result)
    }

    async fn get_by_id_unfiltered(
        &self,
        id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|cal| cal.id == id)
            .cloned())
    }

    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|cal| {
                cal.user_id == user_id
                    && cal.google_calendar_id == google_cal_id
                    && cal.deleted_at.is_none()
            })
            .cloned())
    }

    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
        self.upsert_batch(vec![calendar]).await
    }

    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
        for cal in calendars {
            self.upserted.lock().unwrap().push(cal.clone());
            let mut stored = self.stored.lock().unwrap();
            if let Some(existing) = stored.iter_mut().find(|row| {
                row.user_id == cal.user_id && row.google_calendar_id == cal.google_calendar_id
            }) {
                let resurrecting = existing.deleted_at.is_some();
                existing.summary = cal.summary.clone();
                existing.time_zone = cal.time_zone.clone();
                existing.is_primary = cal.is_primary;
                existing.access_role = cal.access_role.clone();
                existing.updated_at = "2026-08-17T00:00:00Z".to_string();
                existing.deleted_at = None;
                if resurrecting {
                    // Returned calendar is a new appearance.
                    existing.sync_enabled = cal.sync_enabled;
                }
                // Living: keep sync_enabled, health, dirty gens, event_labels, id.
                // Empty incoming sync_token / last_synced_at preserve stored
                // (COALESCE), matching CALENDAR_UPSERT_SQL.
                if !cal.sync_token.is_empty() {
                    existing.sync_token = cal.sync_token.clone();
                }
                if cal.last_synced_at.is_some() {
                    existing.last_synced_at = cal.last_synced_at.clone();
                }
            } else {
                let mut next = self.next_id.lock().unwrap();
                let row = GoogleCalendar {
                    id: format!("cal-{next}"),
                    user_id: cal.user_id.clone(),
                    google_calendar_id: cal.google_calendar_id.clone(),
                    summary: cal.summary.clone(),
                    time_zone: cal.time_zone.clone(),
                    is_primary: cal.is_primary,
                    access_role: cal.access_role.clone(),
                    sync_enabled: cal.sync_enabled,
                    sync_token: cal.sync_token.clone(),
                    last_synced_at: cal.last_synced_at.clone(),
                    // Freshly imported rows start with an empty label cache
                    // (cache miss) — `refresh_calendar_list` backfills it.
                    event_labels: String::new(),
                    // Health defaults: calendarList upsert must never write these.
                    sync_query_fingerprint: String::new(),
                    sync_status: String::new(),
                    initial_sync_complete: false,
                    last_attempt_at: None,
                    last_success_at: None,
                    last_error_code: String::new(),
                    failure_streak: 0,
                    next_retry_at: None,
                    dirty_requested_generation: 0,
                    dirty_applied_generation: 0,
                    full_sync_requested: false,
                    lease_owner: String::new(),
                    lease_expires_at: None,
                    cache_revision: 0,
                    projection: "timed_masters_and_exceptions".to_string(),
                    created_at: "2026-08-17T00:00:00Z".to_string(),
                    updated_at: "2026-08-17T00:00:00Z".to_string(),
                    deleted_at: None,
                };
                *next += 1;
                stored.push(row);
            }
        }
        Ok(())
    }

    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError> {
        self.sync_states.lock().unwrap().push((
            id.to_string(),
            sync_token.to_string(),
            last_synced_at_rfc3339.to_string(),
        ));
        // Match D1 `CALENDAR_UPDATE_SYNC_STATE_SQL`: persist token +
        // last_synced_at onto the stored row so a re-read after
        // `sync_calendar` sees compat success.
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.sync_token = sync_token.to_string();
            cal.last_synced_at = Some(last_synced_at_rfc3339.to_string());
        }
        Ok(())
    }

    async fn record_sync_attempt(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.last_attempt_at = Some(now_rfc3339.to_string());
            cal.updated_at = now_rfc3339.to_string();
        }
        Ok(())
    }

    async fn record_sync_success(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Keep `sync_states` meaningful for tests that assert cursor
        // advancement (mirrors legacy `update_sync_state` recording).
        self.sync_states.lock().unwrap().push((
            id.to_string(),
            sync_token.to_string(),
            now_rfc3339.to_string(),
        ));
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            Self::apply_success_fields(cal, sync_token, query_fingerprint, now_rfc3339);
        }
        Ok(())
    }

    async fn record_sync_success_if_owner(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let mut stored = self.stored.lock().unwrap();
        let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
            return Ok(false);
        };
        if !Self::lease_held(cal, lease_owner, now_rfc3339) {
            // Token must remain unchanged.
            return Ok(false);
        }
        Self::apply_success_fields(cal, sync_token, query_fingerprint, now_rfc3339);
        drop(stored);
        self.sync_states.lock().unwrap().push((
            id.to_string(),
            sync_token.to_string(),
            now_rfc3339.to_string(),
        ));
        Ok(true)
    }

    async fn record_sync_failure(
        &self,
        id: &str,
        error_code: &str,
        sync_status: &str,
        next_retry_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            // Do not touch sync_token / last_success_at / last_synced_at.
            cal.last_error_code = error_code.to_string();
            cal.failure_streak += 1;
            cal.sync_status = sync_status.to_string();
            cal.next_retry_at = Some(next_retry_rfc3339.to_string());
            cal.updated_at = now_rfc3339.to_string();
        }
        Ok(())
    }

    async fn try_acquire_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
        expires_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let mut stored = self.stored.lock().unwrap();
        let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
            return Ok(false);
        };
        let can_take = cal.lease_owner.is_empty()
            || cal.lease_owner == owner
            || cal.lease_expires_at.is_none()
            || cal
                .lease_expires_at
                .as_deref()
                .is_some_and(|exp| exp < now_rfc3339);
        if !can_take {
            return Ok(false);
        }
        cal.lease_owner = owner.to_string();
        cal.lease_expires_at = Some(expires_rfc3339.to_string());
        cal.updated_at = now_rfc3339.to_string();
        Ok(true)
    }

    async fn release_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            if cal.lease_owner == owner {
                cal.lease_owner = String::new();
                cal.lease_expires_at = None;
                cal.updated_at = now_rfc3339.to_string();
            }
        }
        Ok(())
    }

    async fn renew_lease(
        &self,
        id: &str,
        owner: &str,
        expires_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let mut stored = self.stored.lock().unwrap();
        let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) else {
            return Ok(false);
        };
        if cal.lease_owner != owner {
            return Ok(false);
        }
        cal.lease_expires_at = Some(expires_rfc3339.to_string());
        cal.updated_at = now_rfc3339.to_string();
        Ok(true)
    }

    async fn bump_dirty_requested(
        &self,
        id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if *self.fail_bump_dirty.lock().unwrap() {
            return Err(RepoError::Backend("bump dirty failed".into()));
        }
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            if cal.deleted_at.is_none() {
                cal.dirty_requested_generation =
                    cal.dirty_requested_generation.saturating_add(1);
                cal.updated_at = now_rfc3339.to_string();
            }
        }
        Ok(())
    }

    async fn mark_dirty_applied(
        &self,
        id: &str,
        generation: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            if cal.deleted_at.is_none() && cal.dirty_applied_generation < generation {
                cal.dirty_applied_generation = generation;
                cal.updated_at = now_rfc3339.to_string();
            }
        }
        Ok(())
    }

    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        _now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if *self.fail_set_sync_enabled.lock().unwrap() {
            return Err(RepoError::Backend("set_sync_enabled failed".into()));
        }
        self.disabled.lock().unwrap().push((id.to_string(), enabled));
        // Mutate stored so a re-read after 404-disable shows `disabled`
        // in the health envelope.
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.sync_enabled = enabled;
        }
        Ok(())
    }

    async fn set_event_labels(
        &self,
        id: &str,
        event_labels_json: &str,
        _now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        self.label_updates
            .lock()
            .unwrap()
            .push((id.to_string(), event_labels_json.to_string()));
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.event_labels = event_labels_json.to_string();
        }
        Ok(())
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let mut stored = self.stored.lock().unwrap();
        if let Some(cal) = stored.iter_mut().find(|cal| cal.id == id) {
            cal.deleted_at = Some(now_rfc3339.to_string());
            cal.updated_at = now_rfc3339.to_string();
        }
        Ok(())
    }

    async fn get_calendar_list_sync_token(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, RepoError> {
        Ok(self
            .calendar_list_tokens
            .lock()
            .unwrap()
            .get(user_id)
            .cloned())
    }

    async fn set_calendar_list_sync_token(
        &self,
        user_id: &str,
        token: &str,
        _now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        self.calendar_list_tokens
            .lock()
            .unwrap()
            .insert(user_id.to_string(), token.to_string());
        Ok(())
    }

    async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError> {
        let mut ids: Vec<String> = self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|cal| cal.deleted_at.is_none())
            .map(|cal| cal.user_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

/// In-memory event repo: upserts materialize rows so the follow-up
/// time-range query returns them, and every call is recorded.
pub(crate) struct FakeEventRepo {
    pub(crate) stored: Mutex<Vec<CalendarEvent>>,
    /// Optional parent-calendar catalog for list queries. Empty = no join
    /// filter (existing fixtures that only seed events keep working).
    /// When populated, mirrors the SQL INNER JOIN + living + sync_enabled.
    pub(crate) parent_calendars: Mutex<Vec<GoogleCalendar>>,
    pub(crate) upserted_batch: Mutex<Vec<NewCalendarEvent>>,
    pub(crate) upserted_single: Mutex<Option<(String, NewCalendarEvent)>>,
    pub(crate) ranged: Mutex<Vec<(String, String, String)>>,
    pub(crate) deleted: Mutex<Vec<(String, String)>>,
    pub(crate) deleted_by_google_event_id: Mutex<Vec<(String, String)>>,
    /// `(calendar_id, older_than, now)` — replica walk must never push.
    pub(crate) deleted_stale: Mutex<Vec<(String, String, String)>>,
    pub(crate) fail_upsert: Mutex<bool>,
    pub(crate) fail_delete: Mutex<bool>,
    pub(crate) next_id: Mutex<u64>,
}

impl FakeEventRepo {
    pub(crate) fn new() -> Self {
        Self {
            stored: Mutex::new(Vec::new()),
            parent_calendars: Mutex::new(Vec::new()),
            upserted_batch: Mutex::new(Vec::new()),
            upserted_single: Mutex::new(None),
            ranged: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
            deleted_by_google_event_id: Mutex::new(Vec::new()),
            deleted_stale: Mutex::new(Vec::new()),
            fail_upsert: Mutex::new(false),
            fail_delete: Mutex::new(false),
            next_id: Mutex::new(1),
        }
    }

    /// Natural-key upsert including soft-deleted rows: on hit, update
    /// fields, clear `deleted_at`, return the existing id; on miss, insert
    /// and return a new id. Mirrors D1 + `EVENT_UPSERT_ON_CONFLICT`.
    pub(crate) fn apply_upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> String {
        let mut stored = self.stored.lock().unwrap();
        if let Some(existing) = stored.iter_mut().find(|row| {
            row.calendar_id == event.calendar_id && row.google_event_id == event.google_event_id
        }) {
            let id = existing.id.clone();
            // Preserve task_id when incoming is empty (SQL COALESCE).
            let task_id = if event.task_id.is_empty() {
                existing.task_id.clone()
            } else {
                event.task_id.clone()
            };
            existing.google_etag = event.google_etag;
            existing.google_updated_at = event.google_updated_at;
            existing.last_synced_at = event.last_synced_at;
            existing.title = event.title;
            existing.description = event.description;
            existing.start_time = event.start_time;
            existing.end_time = event.end_time;
            existing.recurrence = event.recurrence;
            existing.task_id = task_id;
            existing.ical_uid = event.ical_uid;
            existing.sequence = event.sequence;
            existing.status = event.status;
            existing.recurring_event_id = event.recurring_event_id;
            existing.original_start = event.original_start;
            existing.start_time_zone = event.start_time_zone;
            existing.end_time_zone = event.end_time_zone;
            existing.is_all_day = event.is_all_day;
            existing.raw_json = event.raw_json;
            existing.updated_at = now_rfc3339.to_string();
            existing.deleted_at = None;
            return id;
        }
        let mut next = self.next_id.lock().unwrap();
        let id = format!("evt-{next}");
        *next += 1;
        stored.push(row_from_new_event(event, id.clone(), now_rfc3339));
        id
    }

    /// Mirrors GET projection filters (`timed_masters_and_exceptions`),
    /// including the master/instance dedupe rule on
    /// [`crate::repo::EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL`].
    ///
    /// `all` is the full stored set so sibling masters can be found.
    pub(crate) fn in_projection(event: &CalendarEvent, all: &[CalendarEvent]) -> bool {
        if event.deleted_at.is_some() || event.is_all_day {
            return false;
        }
        if !event.status.is_empty() && event.status == "cancelled" {
            return false;
        }
        // Hide unmodified window instance when a living master exists.
        if !event.recurring_event_id.is_empty() {
            let unmodified = event.original_start.is_empty()
                || event.original_start == event.start_time;
            if unmodified {
                let has_master = all.iter().any(|m| {
                    m.deleted_at.is_none()
                        && m.calendar_id == event.calendar_id
                        && m.google_event_id == event.recurring_event_id
                        && !m.recurrence.is_empty()
                        && m.title == event.title
                });
                if has_master {
                    return false;
                }
            }
        }
        true
    }

    /// Mirrors the SQL INNER JOIN on `google_calendars` plus living +
    /// sync-enabled parent predicates. Empty catalog = no filter so fixtures
    /// that only seed events keep working.
    fn living_enabled_parent(
        event: &CalendarEvent,
        user_id: &str,
        calendars: &[GoogleCalendar],
    ) -> bool {
        if calendars.is_empty() {
            return true;
        }
        calendars.iter().any(|c| {
            c.id == event.calendar_id
                && c.user_id == user_id
                && c.deleted_at.is_none()
                && c.sync_enabled
        })
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventRepo for FakeEventRepo {
    async fn upsert(
        &self,
        event: NewCalendarEvent,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        if *self.fail_upsert.lock().unwrap() {
            return Err(RepoError::Backend("cache write failed".into()));
        }
        *self.upserted_single.lock().unwrap() =
            Some((event.google_event_id.clone(), event.clone()));
        Ok(self.apply_upsert(event, now_rfc3339))
    }

    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        if *self.fail_upsert.lock().unwrap() {
            return Err(RepoError::Backend("cache write failed".into()));
        }
        self.upserted_batch.lock().unwrap().extend(events.clone());
        for event in events {
            self.apply_upsert(event, now_rfc3339);
        }
        Ok(())
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|event| event.deleted_at.is_none() && event.id == id)
            .cloned())
    }

    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|event| {
                event.deleted_at.is_none()
                    && event.calendar_id == calendar_id
                    && event.google_event_id == google_event_id
            })
            .cloned())
    }

    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        self.ranged.lock().unwrap().push((
            user_id.to_string(),
            start_rfc3339.to_string(),
            end_rfc3339.to_string(),
        ));
        // Mirrors EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL: living+enabled
        // parent join + projection + overlap (start < window_end AND end >
        // window_start).
        let stored = self.stored.lock().unwrap();
        let parents = self.parent_calendars.lock().unwrap();
        Ok(stored
            .iter()
            .filter(|event| {
                Self::living_enabled_parent(event, user_id, &parents)
                    && Self::in_projection(event, &stored)
                    && event.start_time.as_str() < end_rfc3339
                    && event.end_time.as_str() > start_rfc3339
            })
            .cloned()
            .collect())
    }

    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        // Mirrors EVENT_LIST_RUNNING_BY_USER_ID_SQL: living+enabled parent
        // join + projection + task-tagged + `start_time <= now < end_time`.
        let stored = self.stored.lock().unwrap();
        let parents = self.parent_calendars.lock().unwrap();
        Ok(stored
            .iter()
            .filter(|event| {
                Self::living_enabled_parent(event, user_id, &parents)
                    && Self::in_projection(event, &stored)
                    && !event.task_id.is_empty()
                    && event.start_time.as_str() <= now_rfc3339
                    && event.end_time.as_str() > now_rfc3339
            })
            .cloned()
            .collect())
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        self.deleted
            .lock()
            .unwrap()
            .push((id.to_string(), now_rfc3339.to_string()));
        if *self.fail_delete.lock().unwrap() {
            return Err(RepoError::Backend("cache delete failed".into()));
        }
        let mut stored = self.stored.lock().unwrap();
        if let Some(event) = stored.iter_mut().find(|event| event.id == id) {
            event.deleted_at = Some(now_rfc3339.to_string());
        }
        Ok(())
    }

    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        self.deleted_by_google_event_id
            .lock()
            .unwrap()
            .push((calendar_id.to_string(), google_event_id.to_string()));
        if *self.fail_delete.lock().unwrap() {
            return Err(RepoError::Backend("cache delete failed".into()));
        }
        let mut stored = self.stored.lock().unwrap();
        if let Some(event) = stored.iter_mut().find(|event| {
            event.calendar_id == calendar_id && event.google_event_id == google_event_id
        }) {
            event.deleted_at = Some(now_rfc3339.to_string());
        }
        Ok(())
    }

    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        self.deleted_stale.lock().unwrap().push((
            calendar_id.to_string(),
            older_than_rfc3339.to_string(),
            now_rfc3339.to_string(),
        ));
        Ok(())
    }
}

/// In-memory watch-channel repo: stores rows and records every
/// insert/delete/list call so tests can assert watch behavior.
pub(crate) struct FakeWatchChannelRepo {
    pub(crate) stored: Mutex<Vec<WatchChannel>>,
    pub(crate) inserted: Mutex<Vec<NewWatchChannel>>,
    pub(crate) deleted_by_id: Mutex<Vec<String>>,
    pub(crate) deleted_by_calendar_id: Mutex<Vec<String>>,
}

impl FakeWatchChannelRepo {
    pub(crate) fn new() -> Self {
        Self {
            stored: Mutex::new(Vec::new()),
            inserted: Mutex::new(Vec::new()),
            deleted_by_id: Mutex::new(Vec::new()),
            deleted_by_calendar_id: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn with(channels: Vec<WatchChannel>) -> Self {
        Self {
            stored: Mutex::new(channels),
            inserted: Mutex::new(Vec::new()),
            deleted_by_id: Mutex::new(Vec::new()),
            deleted_by_calendar_id: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl WatchChannelRepo for FakeWatchChannelRepo {
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        self.inserted.lock().unwrap().push(channel.clone());
        // Ids must not collide with preloaded fixture rows (the real D1
        // impl mints UUIDv4s).
        let id = format!("wc-{}", self.stored.lock().unwrap().len() + 1);
        self.stored.lock().unwrap().push(WatchChannel {
            id: id.clone(),
            calendar_id: channel.calendar_id.clone(),
            channel_id: channel.channel_id.clone(),
            resource_id: channel.resource_id.clone(),
            token: channel.token.clone(),
            expiration: channel.expiration.clone(),
            created_at: now_rfc3339.to_string(),
            updated_at: now_rfc3339.to_string(),
        });
        Ok(id)
    }

    async fn get_by_channel_id(
        &self,
        _channel_id: &str,
    ) -> Result<Option<WatchChannel>, RepoError> {
        Ok(None)
    }

    async fn list_by_calendar_id(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|channel| channel.calendar_id == calendar_id)
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> Result<Vec<WatchChannel>, RepoError> {
        Ok(self.stored.lock().unwrap().clone())
    }

    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        // RFC 3339 UTC strings compare lexicographically (fixed width).
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|channel| channel.calendar_id == calendar_id)
            .filter(|channel| channel.expiration.as_str() > now_rfc3339)
            .cloned()
            .collect())
    }

    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError> {
        self.deleted_by_id.lock().unwrap().push(id.to_string());
        self.stored
            .lock()
            .unwrap()
            .retain(|channel| channel.id != id);
        Ok(())
    }

    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError> {
        self.deleted_by_calendar_id
            .lock()
            .unwrap()
            .push(calendar_id.to_string());
        self.stored
            .lock()
            .unwrap()
            .retain(|channel| channel.calendar_id != calendar_id);
        Ok(())
    }
}

/// In-memory outbound operation journal (issue #50 / Vertical 4).
/// Records inserts so slice 2 write-path tests can assert journal-first
/// behavior; `fail_insert` forces a backend error before any row is stored.
pub(crate) struct FakeOperationRepo {
    pub(crate) stored: Mutex<Vec<CalendarEventOperation>>,
    pub(crate) inserted: Mutex<Vec<NewCalendarEventOperation>>,
    pub(crate) fail_insert: Mutex<bool>,
    pub(crate) next_id: Mutex<u64>,
}

impl FakeOperationRepo {
    pub(crate) fn new() -> Self {
        Self {
            stored: Mutex::new(Vec::new()),
            inserted: Mutex::new(Vec::new()),
            fail_insert: Mutex::new(false),
            next_id: Mutex::new(1),
        }
    }

    pub(crate) fn with(ops: Vec<CalendarEventOperation>) -> Self {
        let max_numeric = ops
            .iter()
            .filter_map(|op| op.id.strip_prefix("op-")?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        Self {
            stored: Mutex::new(ops),
            inserted: Mutex::new(Vec::new()),
            fail_insert: Mutex::new(false),
            next_id: Mutex::new(max_numeric.saturating_add(1).max(1)),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventOperationRepo for FakeOperationRepo {
    async fn insert(
        &self,
        op: NewCalendarEventOperation,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        self.inserted.lock().unwrap().push(op.clone());
        if *self.fail_insert.lock().unwrap() {
            return Err(RepoError::Backend("journal insert failed".into()));
        }
        let mut next = self.next_id.lock().unwrap();
        let id = format!("op-{next}");
        *next += 1;
        self.stored.lock().unwrap().push(CalendarEventOperation {
            id: id.clone(),
            user_id: op.user_id,
            calendar_id: op.calendar_id,
            local_event_id: op.local_event_id,
            google_event_id: op.google_event_id,
            verb: op.verb,
            payload_fingerprint: op.payload_fingerprint,
            payload_json: op.payload_json,
            status: op.status,
            google_etag: op.google_etag,
            attempt_count: 0,
            last_error: String::new(),
            created_at: now_rfc3339.to_string(),
            updated_at: now_rfc3339.to_string(),
        });
        Ok(id)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEventOperation>, RepoError> {
        Ok(self
            .stored
            .lock()
            .unwrap()
            .iter()
            .find(|op| op.id == id)
            .cloned())
    }

    async fn list_by_statuses(
        &self,
        statuses: &[&str],
    ) -> Result<Vec<CalendarEventOperation>, RepoError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows: Vec<CalendarEventOperation> = self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|op| statuses.iter().any(|s| *s == op.status.as_str()))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        Ok(rows)
    }

    async fn list_inflight_google_ids(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<String>, RepoError> {
        let mut rows: Vec<(String, String)> = self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|op| {
                op.calendar_id == calendar_id
                    && (op.status == OP_STATUS_PENDING
                        || op.status == OP_STATUS_GOOGLE_COMMITTED)
            })
            .map(|op| (op.updated_at.clone(), op.google_event_id.clone()))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows.into_iter().map(|(_, id)| id).collect())
    }

    async fn update_status(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Unknown id is a successful no-op (D1 UPDATE 0 rows).
        if let Some(op) = self
            .stored
            .lock()
            .unwrap()
            .iter_mut()
            .find(|op| op.id == id)
        {
            op.status = status.to_string();
            op.last_error = last_error.to_string();
            op.updated_at = now_rfc3339.to_string();
        }
        Ok(())
    }

    async fn update_progress(
        &self,
        id: &str,
        status: &str,
        google_event_id: &str,
        local_event_id: &str,
        google_etag: &str,
        last_error: &str,
        bump_attempt: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Unknown id is a successful no-op (D1 UPDATE 0 rows).
        if let Some(op) = self
            .stored
            .lock()
            .unwrap()
            .iter_mut()
            .find(|op| op.id == id)
        {
            op.status = status.to_string();
            op.google_event_id = google_event_id.to_string();
            op.local_event_id = local_event_id.to_string();
            op.google_etag = google_etag.to_string();
            op.last_error = last_error.to_string();
            if bump_attempt {
                op.attempt_count += 1;
            }
            op.updated_at = now_rfc3339.to_string();
        }
        Ok(())
    }
}

// ──────────────────────────────────────────
// Fixtures
// ──────────────────────────────────────────

pub(crate) const NOW_UNIX: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

pub(crate) fn access() -> GoogleAccess {
    GoogleAccess {
        access_token: "at-1".to_string(),
        token_type: "Bearer".to_string(),
    }
}

pub(crate) fn calendar(id: &str, google_cal_id: &str, sync_enabled: bool) -> GoogleCalendar {
    calendar_for_user("u-1", id, google_cal_id, sync_enabled)
}

pub(crate) fn calendar_for_user(
    user_id: &str,
    id: &str,
    google_cal_id: &str,
    sync_enabled: bool,
) -> GoogleCalendar {
    GoogleCalendar {
        id: id.to_string(),
        user_id: user_id.to_string(),
        google_calendar_id: google_cal_id.to_string(),
        summary: "Work".to_string(),
        time_zone: "UTC".to_string(),
        is_primary: true,
        access_role: "owner".to_string(),
        sync_enabled,
        sync_token: String::new(),
        last_synced_at: None,
        // `"[]"` = label cache already fetched (no labels) — existing sync
        // tests skip the `calendars.get` backfill. Tests exercising the
        // cache-miss path construct rows with an empty string explicitly.
        event_labels: "[]".to_string(),
        sync_query_fingerprint: String::new(),
        // Empty string matches serde default for missing columns; production
        // backfill uses `never_initialized` / `ready` / `disabled`.
        sync_status: String::new(),
        initial_sync_complete: false,
        last_attempt_at: None,
        last_success_at: None,
        last_error_code: String::new(),
        failure_streak: 0,
        next_retry_at: None,
        dirty_requested_generation: 0,
        dirty_applied_generation: 0,
        full_sync_requested: false,
        lease_owner: String::new(),
        lease_expires_at: None,
        cache_revision: 0,
        projection: "timed_masters_and_exceptions".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        deleted_at: None,
    }
}

/// Token repo for cron tests: returns a stored token per user (expiring
/// far in the future, so `refresh_if_needed` never POSTs) — or `None`
/// for users without one, which fails that user's refresh without
/// touching anyone else. Records nothing.
pub(crate) struct FakeTokenRepo {
    pub(crate) stored: std::sync::Mutex<std::collections::HashMap<String, GoogleOAuthToken>>,
}

impl FakeTokenRepo {
    pub(crate) fn with(tokens: Vec<GoogleOAuthToken>) -> Self {
        let stored = tokens
            .into_iter()
            .map(|token| (token.user_id.clone(), token))
            .collect();
        Self {
            stored: std::sync::Mutex::new(stored),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl TokenRepo for FakeTokenRepo {
    async fn get_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<Option<GoogleOAuthToken>, RepoError> {
        Ok(self.stored.lock().unwrap().get(user_id).cloned())
    }

    async fn upsert(&self, _token: NewToken) -> Result<(), RepoError> {
        Ok(())
    }

    async fn delete(&self, _user_id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
        Ok(())
    }
}

/// A stored OAuth token for `user_id` whose expiry is centuries out, so
/// `refresh_if_needed` returns it as-is (no refresh POST).
pub(crate) fn fresh_token(user_id: &str, access_token: &str) -> GoogleOAuthToken {
    GoogleOAuthToken {
        id: format!("tok-{user_id}"),
        user_id: user_id.to_string(),
        access_token: access_token.to_string(),
        refresh_token: Some("rt-1".to_string()),
        expiry: "2099-01-01T00:00:00Z".to_string(),
        token_type: "Bearer".to_string(),
        scope: Some("calendar".to_string()),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        deleted_at: None,
    }
}

/// OAuth client credentials for `run_fallback_cron` tests; the fresh
/// tokens above mean `refresh_if_needed` never uses them.
pub(crate) fn oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: "client-id.apps.googleusercontent.com".to_string(),
        client_secret: "client-secret".to_string(),
        redirect_url: "http://localhost:5173/auth/google/callback".to_string(),
    }
}

/// A stored watch channel for `calendar_id` with the given RFC 3339
/// `expiration` (future expirations must be > NOW_UNIX's instant,
/// 2023-11-14T22:13:20Z, to count as unexpired).
pub(crate) fn watch_channel(calendar_id: &str, expiration: &str) -> WatchChannel {
    WatchChannel {
        id: "wc-1".to_string(),
        calendar_id: calendar_id.to_string(),
        channel_id: "minted-id".to_string(),
        resource_id: "resource-1".to_string(),
        token: "tok-1".to_string(),
        expiration: expiration.to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

pub(crate) const CALLBACK_URL: &str =
    "https://my-sanctuary.fahimalizain.com/api/calendar/notifications";

pub(crate) const CALENDAR_LIST_JSON: &str = r#"{
    "items": [
        {"id": "primary@example.com", "summary": "Work", "timeZone": "UTC", "primary": true, "accessRole": "owner"},
        {"id": "en.usa#holiday@group.v.calendar.google.com", "summary": "Holidays", "primary": false, "accessRole": "reader"}
    ]
}"#;

pub(crate) const EVENTS_JSON: &str = r#"{
    "items": [
        {"id": "evt-1", "etag": "e1", "updated": "2026-08-17T10:00:00.000Z",
         "summary": "Standup", "description": "Daily",
         "start": {"dateTime": "2026-08-18T09:00:00Z"},
         "end": {"dateTime": "2026-08-18T09:30:00Z"},
         "recurrence": ["RRULE:FREQ=DAILY"]},
        {"id": "evt-2", "summary": "Lunch",
         "start": {"dateTime": "2026-08-18T12:00:00Z"},
         "end": {"dateTime": "2026-08-18T13:00:00Z"}}
    ],
    "nextSyncToken": "st-9"
}"#;






















pub(crate) fn seeded_event(
    id: &str,
    calendar_id: &str,
    google_event_id: &str,
    task_id: &str,
) -> CalendarEvent {
    CalendarEvent {
        id: id.to_string(),
        calendar_id: calendar_id.to_string(),
        google_event_id: google_event_id.to_string(),
        google_etag: String::new(),
        google_updated_at: String::new(),
        last_synced_at: "2023-11-14T21:00:00Z".to_string(),
        title: google_event_id.to_string(),
        description: String::new(),
        start_time: "2026-08-18T09:00:00Z".to_string(),
        end_time: "2026-08-18T09:30:00Z".to_string(),
        recurrence: String::new(),
        task_id: task_id.to_string(),
        ical_uid: String::new(),
        sequence: 0,
        status: "confirmed".to_string(),
        recurring_event_id: String::new(),
        original_start: String::new(),
        start_time_zone: String::new(),
        end_time_zone: String::new(),
        is_all_day: false,
        raw_json: String::new(),
        created_at: "2023-11-14T21:00:00Z".to_string(),
        updated_at: "2023-11-14T21:00:00Z".to_string(),
        deleted_at: None,
    }
}






















// ──────────────────────────────────────────
// list_events watch wiring
// ──────────────────────────────────────────

pub(crate) const WATCH_JSON: &str = r#"{"id":"minted-id","resourceId":"resource-123","expiration":1710000000000}"#;
// Production shape: `Channel.expiration` is discovery type string/int64, so
// `events.watch` returns it as a JSON string of milliseconds. Same millis
// as `WATCH_JSON` — the converted expiration "2024-03-09T16:00:00Z" holds.
pub(crate) const WATCH_JSON_STRING_EXPIRATION: &str =
    r#"{"id":"minted-id","resourceId":"resource-123","expiration":"1710000000000"}"#;






























// ──────────────────────────────────────────
// decide_webhook / tokens_match
// ──────────────────────────────────────────

/// A stored channel whose token is `tok-1` (the `watch_channel` fixture
/// token), so the fixture calendar and channel pair verify cleanly.
pub(crate) fn webhook_channel(calendar_id: &str) -> WatchChannel {
    watch_channel(calendar_id, "2023-11-21T22:13:20Z")
}


























// ──────────────────────────────────────────
// create_event
// ──────────────────────────────────────────

pub(crate) const CREATED_JSON: &str = r#"{
    "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
    "summary": "New meeting", "description": "About things",
    "start": {"dateTime": "2026-08-19T09:00:00Z"},
    "end": {"dateTime": "2026-08-19T10:00:00Z"}
}"#;

pub(crate) fn input() -> NewEventInput {
    NewEventInput {
        calendar_id: "cal-1".to_string(),
        summary: "New meeting".to_string(),
        description: Some("About things".to_string()),
        start: "2026-08-19T09:00:00Z".to_string(),
        end: "2026-08-19T10:00:00Z".to_string(),
        task_id: None,
        routine_id: None,
        occurrence_id: None,
        color_hex: None,
        sanctuary_focus: false,
        priority: None,
        difficulty: None,
    }
}

/// A created event that carries the task carrier, exactly as Google
/// echoes it back after `events.insert` with the property.
pub(crate) fn created_with_task_json(task_id: &str) -> String {
    format!(
        r#"{{
            "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
            "summary": "New meeting", "description": "About things",
            "start": {{"dateTime": "2026-08-19T09:00:00Z"}},
            "end": {{"dateTime": "2026-08-19T10:00:00Z"}},
            "extendedProperties": {{"shared": {{"sanctuary_task_id": "{task_id}"}}}}
        }}"#
    )
}











/// A created event that carries both the task carrier and the focus flag,
/// exactly as Google echoes a focused segment back after `events.insert`.
pub(crate) fn created_with_focus_json(task_id: &str) -> String {
    format!(
        r#"{{
            "id": "google-evt-created", "etag": "e1", "updated": "2026-08-17T12:00:00.000Z",
            "summary": "New meeting", "description": "About things",
            "start": {{"dateTime": "2026-08-19T09:00:00Z"}},
            "end": {{"dateTime": "2026-08-19T10:00:00Z"}},
            "extendedProperties": {{"shared": {{"sanctuary_task_id": "{task_id}", "sanctuary_focus": "1"}}}}
        }}"#
    )
}





// ──────────────────────────────────────────
// patch_event
// ──────────────────────────────────────────

pub(crate) const PATCHED_JSON: &str = r#"{
    "id": "google-evt-created", "etag": "e2", "updated": "2026-08-17T12:30:00.000Z",
    "summary": "New meeting",
    "start": {"dateTime": "2026-08-19T09:00:00Z"},
    "end": {"dateTime": "2026-08-19T11:00:00Z"}
}"#;








// ──────────────────────────────────────────
// delete_event / update_event_for_user
// ──────────────────────────────────────────

pub(crate) fn living_event(id: &str, calendar_id: &str, google_event_id: &str) -> CalendarEvent {
    CalendarEvent {
        id: id.to_string(),
        calendar_id: calendar_id.to_string(),
        google_event_id: google_event_id.to_string(),
        google_etag: "e1".to_string(),
        google_updated_at: "2026-08-17T12:00:00Z".to_string(),
        last_synced_at: "2026-08-17T12:00:00Z".to_string(),
        title: "Meeting".to_string(),
        description: String::new(),
        start_time: "2026-08-19T09:00:00Z".to_string(),
        end_time: "2026-08-19T10:00:00Z".to_string(),
        recurrence: String::new(),
        task_id: String::new(),
        ical_uid: String::new(),
        sequence: 0,
        status: String::new(),
        recurring_event_id: String::new(),
        original_start: String::new(),
        start_time_zone: String::new(),
        end_time_zone: String::new(),
        is_all_day: false,
        raw_json: String::new(),
        created_at: "2026-08-17T12:00:00Z".to_string(),
        updated_at: "2026-08-17T12:00:00Z".to_string(),
        deleted_at: None,
    }
}






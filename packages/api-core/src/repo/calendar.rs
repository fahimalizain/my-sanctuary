//! Calendar repository traits and SQL.

use async_trait::async_trait;

use super::RepoError;
use crate::models::{
    CalendarEvent, CalendarEventOperation, GoogleCalendar, NewCalendar, NewCalendarEvent,
    NewCalendarEventOperation, NewWatchChannel, WatchChannel,
};

/// Google Calendar persistence (`google_calendars` rows).
///
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed.
#[async_trait(?Send)]
pub trait CalendarRepo: Send + Sync {
    /// The user's calendars, primary first then by summary.
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError>;
    /// Every sync-enabled, non-deleted calendar across all users — the
    /// fallback cron's work list (ADR 0001 § Fallback cron).
    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError>;
    /// Returns the calendar with local `id`, or `None` when absent/soft-deleted.
    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError>;
    /// Like [`CalendarRepo::get_by_id`] but does **not** filter `deleted_at`.
    /// Used to retry leftover watch stops after a calendar is soft-deleted
    /// (channel rows survive; living-only reads cannot recover `user_id`).
    async fn get_by_id_unfiltered(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError>;
    /// Returns the calendar with `google_calendar_id`, or `None`.
    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError>;
    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError>;
    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError>;
    /// Stores the incremental sync cursor and the sync timestamp.
    ///
    /// Compat path kept for existing call sites. Prefer
    /// [`CalendarRepo::record_sync_success`] which also writes health columns.
    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Records that a sync attempt started (`last_attempt_at = now`). Does not
    /// touch token, success, or failure columns.
    async fn record_sync_attempt(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Records a successful apply: writes token + `last_synced_at` /
    /// `last_success_at`, clears error/streak, marks ready, bumps
    /// `cache_revision`.
    ///
    /// Unfenced compat path. Prefer [`CalendarRepo::record_sync_success_if_owner`]
    /// for the replica walk so a lost lease cannot publish a cursor.
    async fn record_sync_success(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Fenced success: same writes as [`CalendarRepo::record_sync_success`] but
    /// only when `lease_owner` still matches and the lease is unexpired.
    /// Returns `false` when zero rows updated (token / health unchanged).
    async fn record_sync_success_if_owner(
        &self,
        id: &str,
        sync_token: &str,
        query_fingerprint: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    /// Records a failed attempt: sets error code, increments streak, updates
    /// status and next retry. Does **not** touch token, `last_success_at`,
    /// `last_synced_at`, or `last_attempt_at` (attempt is recorded at start).
    async fn record_sync_failure(
        &self,
        id: &str,
        error_code: &str,
        sync_status: &str,
        next_retry_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Contention (lost/stolen lease): set error code + retrying + next_retry.
    /// Does **not** increment `failure_streak`, does **not** touch token / success.
    async fn record_sync_contention(
        &self,
        id: &str,
        error_code: &str,
        sync_status: &str,
        next_retry_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Persist that a merge-full / reseed is required and now in-flight.
    /// Sets `full_sync_requested = 1` and `sync_status = 'rebuilding'`.
    /// Does **not** touch `sync_token`, success stamps, error/streak, dirty gens, or lease.
    async fn begin_replica_reseed(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Try to become the sole replica owner for `id`. Succeeds when the lease
    /// is empty, already ours, or expired. Returns `true` only when this
    /// `owner` holds the lease after the update.
    async fn try_acquire_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
        expires_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    /// Clear the lease only if `owner` still holds it.
    async fn release_lease(
        &self,
        id: &str,
        owner: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Extend `lease_expires_at` only if `owner` still holds the lease.
    /// Returns `true` when the row was updated.
    async fn renew_lease(
        &self,
        id: &str,
        owner: &str,
        expires_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    /// Bump `dirty_requested_generation` so cron/webhook replica work can catch
    /// up (V3 dirty channel). Does **not** set `full_sync_requested`.
    async fn bump_dirty_requested(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Set `dirty_applied_generation` to `generation` when that is strictly
    /// greater than the stored applied value. Never writes `dirty_requested_generation`
    /// or `sync_token`. Used after a successful replica publication with the
    /// generation snapshotted at the start of the run.
    async fn mark_dirty_applied(
        &self,
        id: &str,
        generation: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Stores the cached `calendars.get` `labelProperties.eventLabels` JSON
    /// (empty string = never fetched; `"[]"`/JSON array = fetched).
    async fn set_event_labels(
        &self,
        id: &str,
        event_labels_json: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Persist sanitized watch coverage (`missing` | `expiring` |
    /// `no_successor` | `covered`). Never stores channel tokens / resource ids.
    async fn set_watch_coverage(
        &self,
        id: &str,
        coverage: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Persist sanitized event coverage (`complete` | `degraded`). Never stores
    /// replay payloads, sync tokens, or channel ids.
    async fn set_event_coverage(
        &self,
        id: &str,
        coverage: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339`.
    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Stored calendarList.list `nextSyncToken` for `user_id`, if any.
    ///
    /// `None` or `Some("")` both mean "no cursor → full list".
    async fn get_calendar_list_sync_token(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, RepoError>;
    /// UPSERT the calendarList sync cursor. Persist `""` to clear it (410 restart).
    async fn set_calendar_list_sync_token(
        &self,
        user_id: &str,
        token: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Distinct living-calendar owners, ordered. Cron uses this so users with
    /// only disabled calendars still get list refresh (they may add a calendar).
    async fn list_user_ids_with_calendars(&self) -> Result<Vec<String>, RepoError>;
    /// Clear `authorization_required` on living sync-enabled calendars for
    /// `user_id` after a successful token refresh or OAuth reconnect.
    ///
    /// Sets status to `retrying` when the calendar has ever succeeded
    /// (`initial_sync_complete` or non-empty `last_success_at`), else
    /// `never_initialized`. Clears error/streak. Binds `next_retry_at` to
    /// `next_retry_rfc3339` (callers pass **now**) so [`replica_due`]'s
    /// retry gate opens this tick — `NULL` would leave a recently-ready
    /// misclassified row not due until stale. Does **not** touch
    /// `sync_token`, lease, dirty gens, or `full_sync_requested`.
    async fn clear_authorization_required_for_user(
        &self,
        user_id: &str,
        next_retry_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
}

/// Cached Google Calendar event persistence (`calendar_events` rows).
///
/// All deletes are SOFT: `deleted_at` is stamped, rows are never removed
/// (fixing the old Go D1 implementation, which hard-deleted).
#[async_trait(?Send)]
pub trait CalendarEventRepo: Send + Sync {
    /// Inserts or updates one event and returns the generated `id`.
    async fn upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> Result<String, RepoError>;
    /// Inserts or updates many events, chunked to respect D1's 100-bound-
    /// parameter limit (see `EVENT_UPSERT_CHUNK_SIZE`).
    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Fenced batch upsert: same as [`Self::upsert_batch`] but only when
    /// `lease_owner` still holds an unexpired lease on the events' calendar
    /// (`lease_owner = ? AND (lease_expires_at IS NULL OR lease_expires_at >= ?)`
    /// plus living calendar `deleted_at IS NULL` — same predicate as
    /// [`CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL`]). Returns `false` when
    /// the lease is missing/stolen/expired (no rows written). Empty `events`
    /// returns `true` without touching storage.
    async fn upsert_batch_if_owner(
        &self,
        events: Vec<NewCalendarEvent>,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError>;
    /// Returns the *living* cached event by `(calendar_id, google_event_id)` —
    /// the exit path (`stop_running_event`) resolves the event a `started` log
    /// points at, then reads its `start_time` before PATCHing the end.
    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError>;
    /// Events that *overlap* the half-open `[start, end)` window:
    /// `start_time < end AND end_time > start` (multi-day events are not
    /// clipped at window edges).
    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError>;
    /// The user's *living* timed events (task-tagged, joined to their
    /// calendars) with `task_id` set AND `start_time <= now < end_time` —
    /// the derived "running" set (RFC 3339 UTC strings of this shape compare
    /// lexicographically). At most one such event per user is expected.
    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError>;
    /// SOFT delete by local id.
    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Reassign a living event's `calendar_id` in place (same local `id`).
    /// Used after Google `events.move` so the natural key can flip without
    /// inserting a second row.
    async fn reassign_calendar(
        &self,
        id: &str,
        new_calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// SOFT delete by `(calendar_id, google_event_id)` — used when incremental
    /// sync reports a cancelled event.
    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Fenced soft-delete by `(calendar_id, google_event_id)`. Returns `false`
    /// when the lease is missing/stolen/expired (row unchanged). Returns `true`
    /// when the owner still holds the lease, even if no living row existed
    /// (idempotent delete). Ownership predicate matches
    /// [`CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL`]:
    /// `lease_owner = ? AND (lease_expires_at IS NULL OR lease_expires_at >= ?)`
    /// plus living calendar (`deleted_at IS NULL`).
    async fn delete_by_google_event_id_if_owner(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;
    /// SOFT delete of rows whose `last_synced_at` is older than
    /// `older_than_rfc3339` (stale-event cleanup).
    ///
    /// **Do not call from the replica walk.** Generation membership uses
    /// [`Self::sweep_absent_if_owner`] after a completed merge-full; an
    /// unfenced timestamp sweep is not generation membership (issue #60).
    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;

    /// Record Google event ids seen on one page of a merge-full walk.
    /// `INSERT OR IGNORE` into `calendar_replica_seen` for
    /// `(calendar_id, run_id, google_event_id)`. Empty `google_event_ids` is a
    /// no-op. Chunked for D1's 100-bind limit (4 binds/row → ≤ 25 rows).
    async fn record_replica_seen(
        &self,
        calendar_id: &str,
        run_id: &str,
        google_event_ids: Vec<String>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;

    /// Drop all `calendar_replica_seen` rows for `calendar_id` (abandon an
    /// incomplete snapshot or clear after successful publish).
    async fn clear_replica_seen_for_calendar(&self, calendar_id: &str) -> Result<(), RepoError>;

    /// Fenced generation-membership sweep: soft-delete living rows on
    /// `calendar_id` whose `google_event_id` is **not** in
    /// `calendar_replica_seen` for `(calendar_id, run_id)`, excluding cancelled
    /// exceptions (`status = 'cancelled'` + non-empty trimmed
    /// `recurring_event_id`), non-empty trimmed `task_id`, and already-deleted
    /// rows. Lease predicate matches
    /// [`CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL`]. Returns `false` when the
    /// lease is missing/stolen/expired (no further mutation). Zero updated
    /// rows is ambiguous — probe with [`CALENDAR_LEASE_HELD_SQL`] like
    /// [`Self::delete_by_google_event_id_if_owner`].
    async fn sweep_absent_if_owner(
        &self,
        calendar_id: &str,
        run_id: &str,
        lease_owner: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError>;

    /// Upsert a deterministic poison quarantine row (issue #60).
    ///
    /// Page-level poison uses `google_event_id = ""` and `phase = "replica_page"`.
    /// `replay_payload` is a capped lossy-UTF-8 body retained for operator
    /// repair only — never expose on the GET envelope, diagnostics, or logs.
    /// ON CONFLICT bumps `attempt_count` and replaces payload / error_class /
    /// `last_attempt_at`; leaves `first_seen_at` unchanged.
    async fn upsert_quarantine(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        phase: &str,
        error_class: &str,
        replay_payload: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;

    /// Drop all quarantine rows for `calendar_id` after a successful publish.
    async fn clear_quarantine_for_calendar(&self, calendar_id: &str) -> Result<(), RepoError>;
}

/// Google Calendar watch channel persistence (`google_calendars_watch_channels`
/// rows).
///
/// Unlike every other table, deletes are HARD: rows are physically removed,
/// never soft-deleted. This table is a subscription, not a domain entity (see
/// ADR 0001).
#[async_trait(?Send)]
pub trait WatchChannelRepo: Send + Sync {
    /// Inserts a new watch channel and returns the generated `id`.
    /// `now_rfc3339` is stamped into `created_at`/`updated_at`.
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError>;
    /// Returns the channel with `channel_id` (the UUID we minted and that
    /// Google echoes back as `X-Goog-Channel-ID`), or `None`.
    async fn get_by_channel_id(&self, channel_id: &str) -> Result<Option<WatchChannel>, RepoError>;
    /// All channels for `calendar_id`. Many rows per calendar are expected:
    /// renewal overlaps two channels briefly (ADR 0001).
    async fn list_by_calendar_id(&self, calendar_id: &str) -> Result<Vec<WatchChannel>, RepoError>;
    /// Every watch channel row (all calendars). Small table; personal app.
    /// Cron uses this to find leftover channels on disabled/soft-deleted
    /// calendars after a best-effort `channels.stop` failed.
    async fn list_all(&self) -> Result<Vec<WatchChannel>, RepoError>;
    /// Channels for `calendar_id` whose `expiration` is still in the future.
    /// RFC 3339 UTC strings compare lexicographically, so `expiration > ?`
    /// is correct.
    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError>;
    /// HARD delete by local row id.
    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError>;
    /// HARD delete of every channel row for `calendar_id` (used when a
    /// calendar is disabled or soft-deleted — see ADR 0001).
    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError>;
}

/// Outbound calendar write journal (`calendar_event_operations` rows).
///
/// Issue #50 / Vertical 4. Separate table = separate trait (same pattern as
/// [`WatchChannelRepo`]). No soft-delete — journal rows stay forever for a
/// personal app. Status machine: `pending` → `google_committed` →
/// `cache_applied`; or `pending` → `failed`; or
/// `pending`/`google_committed` → `conflict` after a 412 retry cap.
#[async_trait(?Send)]
pub trait CalendarEventOperationRepo: Send + Sync {
    /// Inserts a new journal row and returns the generated `id`.
    /// `now_rfc3339` is stamped into `created_at`/`updated_at`. Callers pass
    /// `status = "pending"`.
    async fn insert(
        &self,
        op: NewCalendarEventOperation,
        now_rfc3339: &str,
    ) -> Result<String, RepoError>;
    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEventOperation>, RepoError>;
    /// Living journal rows in `statuses` (e.g. pending + google_committed)
    /// for repair. Empty `statuses` returns `Ok(vec![])` without a query.
    async fn list_by_statuses(
        &self,
        statuses: &[&str],
    ) -> Result<Vec<CalendarEventOperation>, RepoError>;
    /// In-flight (`pending` or `google_committed`) google event ids for one
    /// calendar — replica skip of writes still in flight.
    async fn list_inflight_google_ids(
        &self,
        calendar_id: &str,
    ) -> Result<Vec<String>, RepoError>;
    async fn update_status(
        &self,
        id: &str,
        status: &str,
        last_error: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Patch mutable fields after Google/cache steps: status, etag,
    /// local_event_id, google_event_id (if discovered), last_error;
    /// increments `attempt_count` when `bump_attempt` is true.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<(), RepoError>;
}


// ──────────────────────────────────────────
// Calendar SQL
// ──────────────────────────────────────────

pub const CALENDAR_LIST_BY_USER_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE user_id = ? AND deleted_at IS NULL ORDER BY is_primary DESC, summary ASC";

/// The fallback cron's work list: every sync-enabled calendar that is not
/// soft-deleted, ordered by user, then primary first, then summary.
pub const CALENDAR_LIST_SYNC_ENABLED_SQL: &str =
    "SELECT * FROM google_calendars WHERE sync_enabled = 1 AND deleted_at IS NULL ORDER BY user_id ASC, is_primary DESC, summary ASC";

pub const CALENDAR_GET_BY_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE id = ? AND deleted_at IS NULL";

/// Unfiltered by `deleted_at` — leftover watch-stop retry after soft-delete.
pub const CALENDAR_GET_BY_ID_UNFILTERED_SQL: &str =
    "SELECT * FROM google_calendars WHERE id = ?";

pub const CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL: &str =
    "SELECT * FROM google_calendars WHERE user_id = ? AND google_calendar_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `(user_id, google_calendar_id)`. An empty incoming
/// `sync_token`/`last_synced_at` preserves the stored value (`COALESCE`), and
/// `deleted_at = NULL` on conflict resurrects a soft-deleted row so a
/// re-import of the calendar list brings it back.
///
/// `sync_enabled` is written on INSERT (new calendars default true). On
/// conflict for a **living** row the stored value is kept (user disable is
/// sticky). On conflict when resurrecting (`deleted_at IS NOT NULL`) the
/// incoming value is written — a returned calendar is a new appearance.
pub const CALENDAR_UPSERT_SQL: &str = "
    INSERT INTO google_calendars
        (id, user_id, google_calendar_id, summary, time_zone, is_primary, access_role, sync_enabled, sync_token, last_synced_at, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(user_id, google_calendar_id) DO UPDATE SET
        summary = excluded.summary,
        time_zone = excluded.time_zone,
        is_primary = excluded.is_primary,
        access_role = excluded.access_role,
        sync_enabled = CASE
          WHEN google_calendars.deleted_at IS NOT NULL THEN excluded.sync_enabled
          ELSE google_calendars.sync_enabled
        END,
        sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token),
        last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at),
        updated_at = excluded.updated_at,
        deleted_at = NULL
";

/// Per-user calendarList.list cursor. Binds: user_id.
pub const CALENDAR_LIST_STATE_GET_SQL: &str =
    "SELECT sync_token FROM google_calendar_list_state WHERE user_id = ?";

/// UPSERT calendarList cursor. Binds: user_id, sync_token, updated_at.
/// Empty `sync_token` is allowed (clears the cursor after a 410).
pub const CALENDAR_LIST_STATE_UPSERT_SQL: &str = "
    INSERT INTO google_calendar_list_state (user_id, sync_token, updated_at)
    VALUES (?, ?, ?)
    ON CONFLICT(user_id) DO UPDATE SET
        sync_token = excluded.sync_token,
        updated_at = excluded.updated_at
";

/// Distinct owners of living calendars (including sync-disabled). Ordered.
pub const CALENDAR_LIST_USER_IDS_SQL: &str =
    "SELECT DISTINCT user_id FROM google_calendars WHERE deleted_at IS NULL ORDER BY user_id ASC";

pub const CALENDAR_UPDATE_SYNC_STATE_SQL: &str =
    "UPDATE google_calendars SET sync_token = ?, last_synced_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Marks a sync attempt start. Does not touch token, success, or failure fields.
pub const CALENDAR_RECORD_SYNC_ATTEMPT_SQL: &str = "
    UPDATE google_calendars
    SET last_attempt_at = ?, updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Successful apply: token + both success timestamps, clear error/streak,
/// ready status, bump cache_revision, restore event_coverage. Binds: token,
/// now, now, now, fingerprint, now, id (`last_synced_at` and `last_success_at`
/// share the same instant).
pub const CALENDAR_RECORD_SYNC_SUCCESS_SQL: &str = "
    UPDATE google_calendars SET
      sync_token = ?,
      last_synced_at = ?,
      last_success_at = ?,
      last_attempt_at = ?,
      last_error_code = '',
      failure_streak = 0,
      next_retry_at = NULL,
      initial_sync_complete = 1,
      sync_status = 'ready',
      sync_query_fingerprint = ?,
      cache_revision = cache_revision + 1,
      full_sync_requested = 0,
      event_coverage = 'complete',
      updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Fenced success: same SET as [`CALENDAR_RECORD_SYNC_SUCCESS_SQL`] plus a
/// lease-owner guard so a stolen/expired owner cannot publish the cursor.
/// Binds: token, now, now, now, fingerprint, now, id, lease_owner, now.
pub const CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL: &str = "
    UPDATE google_calendars SET
      sync_token = ?,
      last_synced_at = ?,
      last_success_at = ?,
      last_attempt_at = ?,
      last_error_code = '',
      failure_streak = 0,
      next_retry_at = NULL,
      initial_sync_complete = 1,
      sync_status = 'ready',
      sync_query_fingerprint = ?,
      cache_revision = cache_revision + 1,
      full_sync_requested = 0,
      event_coverage = 'complete',
      updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
      AND lease_owner = ?
      AND (lease_expires_at IS NULL OR lease_expires_at >= ?)
";

/// Failed attempt: error code, streak++, status, next retry. Does not write
/// `sync_token`, `last_success_at`, `last_synced_at`, or `last_attempt_at`.
pub const CALENDAR_RECORD_SYNC_FAILURE_SQL: &str = "
    UPDATE google_calendars SET
      last_error_code = ?,
      failure_streak = failure_streak + 1,
      sync_status = ?,
      next_retry_at = ?,
      updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Contention (lost/stolen lease): error code, status, next retry. Does **not**
/// increment `failure_streak`. Does not write `sync_token`, success stamps,
/// lease, or dirty gens. Binds: error_code, sync_status, next_retry, now, id.
pub const CALENDAR_RECORD_SYNC_CONTENTION_SQL: &str = "
    UPDATE google_calendars SET
      last_error_code = ?,
      sync_status = ?,
      next_retry_at = ?,
      updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Mark merge-full / reseed in-flight. Sets `full_sync_requested` and
/// `sync_status = 'rebuilding'`. Does **not** touch `sync_token`, success
/// stamps, error/streak, dirty gens, or lease. Binds: now, id.
pub const CALENDAR_BEGIN_REPLICA_RESEED_SQL: &str = "
    UPDATE google_calendars
    SET full_sync_requested = 1,
        sync_status = 'rebuilding',
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Steal or re-acquire the replica lease when empty, same owner, or expired.
/// Binds: owner, expires, now, id, owner, now.
pub const CALENDAR_TRY_ACQUIRE_LEASE_SQL: &str = "
    UPDATE google_calendars
    SET lease_owner = ?, lease_expires_at = ?, updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
      AND (
        lease_owner = ''
        OR lease_owner = ?
        OR lease_expires_at IS NULL
        OR lease_expires_at < ?
      )
";

/// Release only if still owned by `owner`. Does not touch `sync_token`.
/// Binds: now, id, owner.
pub const CALENDAR_RELEASE_LEASE_SQL: &str = "
    UPDATE google_calendars
    SET lease_owner = '', lease_expires_at = NULL, updated_at = ?
    WHERE id = ? AND lease_owner = ? AND deleted_at IS NULL
";

/// Renew expiry only if still owned by `owner`. Binds: expires, now, id, owner.
///
/// `expires` must be a **fresh** wall-clock expiry (`now + REPLICA_LEASE_TTL_SECS`
/// as RFC 3339 `Z`), computed by the caller at renew time — not the walk-start
/// stamp. Do not use SQLite `datetime()` (it is not RFC 3339 `Z`).
pub const CALENDAR_RENEW_LEASE_SQL: &str = "
    UPDATE google_calendars
    SET lease_expires_at = ?, updated_at = ?
    WHERE id = ? AND lease_owner = ? AND deleted_at IS NULL
";

/// Bump dirty generation so Path B (replica) can catch up after a window
/// first-paint. Binds: now, id. Does not touch `full_sync_requested`.
pub const CALENDAR_BUMP_DIRTY_REQUESTED_SQL: &str = "
    UPDATE google_calendars
    SET dirty_requested_generation = dirty_requested_generation + 1,
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Advance `dirty_applied_generation` to the generation snapshotted at the
/// start of a successful replica publish. Binds: generation, now, id, generation.
/// The `<` guard never moves applied backwards. Does not touch
/// `dirty_requested_generation`, `sync_token`, or `full_sync_requested`.
pub const CALENDAR_MARK_DIRTY_APPLIED_SQL: &str = "
    UPDATE google_calendars
    SET dirty_applied_generation = ?,
        updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
      AND dirty_applied_generation < ?
";

pub const CALENDAR_SET_SYNC_ENABLED_SQL: &str =
    "UPDATE google_calendars SET sync_enabled = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Writes the cached `calendars.get` `labelProperties.eventLabels` JSON onto
/// the living row (see `GoogleCalendar::event_labels` for the empty-string /
/// `"[]"` / JSON-array convention) and stamps `event_labels_updated_at`.
/// Deliberately a dedicated UPDATE — the calendarList upsert must never wipe
/// the cache or the stamp.
pub const CALENDAR_SET_EVENT_LABELS_SQL: &str =
    "UPDATE google_calendars SET event_labels = ?, event_labels_updated_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Persist sanitized watch coverage. Binds: coverage, now, id.
/// Values: `missing` | `expiring` | `no_successor` | `covered`. Never stores
/// channel tokens or resource ids (those live only on watch channel rows).
pub const CALENDAR_SET_WATCH_COVERAGE_SQL: &str = "
    UPDATE google_calendars
    SET watch_coverage = ?, updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// Persist sanitized event coverage. Binds: coverage, now, id.
/// Values: `complete` | `degraded`. Never stores replay payloads, tokens, or
/// channel ids (those live only on quarantine rows / other tables).
pub const CALENDAR_SET_EVENT_COVERAGE_SQL: &str = "
    UPDATE google_calendars
    SET event_coverage = ?, updated_at = ?
    WHERE id = ? AND deleted_at IS NULL
";

/// SOFT delete: stamps `deleted_at`, keeping the row's UNIQUE
/// `(user_id, google_calendar_id)` slot.
pub const CALENDAR_DELETE_SQL: &str =
    "UPDATE google_calendars SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Clear `authorization_required` after a successful refresh / OAuth reconnect.
///
/// Binds: next_retry, now, user_id.
///
/// `next_retry_at = ?` (callers bind **now**) rather than NULL: a recently-ready
/// calendar misclassified as auth-revoked would not be `replica_due` with a
/// fresh `last_success_at` and NULL backoff. Binding now makes `retry_due`
/// true so the next tick (and the same tick after re-list) attempts. Does
/// **not** touch `sync_token`, lease, dirty gens, or `full_sync_requested`.
pub const CALENDAR_CLEAR_AUTHORIZATION_REQUIRED_SQL: &str = "
    UPDATE google_calendars
    SET sync_status = CASE
          WHEN initial_sync_complete = 1
            OR (last_success_at IS NOT NULL AND last_success_at != '')
          THEN 'retrying'
          ELSE 'never_initialized'
        END,
        last_error_code = '',
        failure_streak = 0,
        next_retry_at = ?,
        updated_at = ?
    WHERE user_id = ?
      AND deleted_at IS NULL
      AND sync_enabled = 1
      AND sync_status = 'authorization_required'
";

// ──────────────────────────────────────────
// Calendar event SQL
// ──────────────────────────────────────────

pub const EVENT_GET_BY_ID_SQL: &str =
    "SELECT * FROM calendar_events WHERE id = ? AND deleted_at IS NULL";

/// The exit path's event lookup: the living cached row a `started` log points
/// at (its `start_time` decides the PATCH end, on the minute grid).
pub const EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL: &str =
    "SELECT * FROM calendar_events WHERE calendar_id = ? AND google_event_id = ? AND deleted_at IS NULL";

/// Overlap semantics: an event intersects `[start, end)` when it begins before
/// the window ends AND ends after it begins — multi-day and overnight events
/// are not clipped at window edges.
///
/// Parent calendar must be living (`c.deleted_at IS NULL`) and sync-enabled
/// (`c.sync_enabled = 1`). Soft-deleted or user-disabled calendars keep their
/// cached events but must not paint on GET.
///
/// Projection name remains `timed_masters_and_exceptions` (health string), but
/// GET range list **includes all-day rows** so the week grid can paint them.
/// Cancelled exceptions (stored living for series correctness) stay excluded.
///
/// Also hide an unmodified window instance when a living **master** exists in
/// the same calendar (`m.google_event_id = e.recurring_event_id`,
/// `m.deleted_at IS NULL`, `m.recurrence != ''`) and the instance looks
/// unmodified (`original_start` empty or equal to `start_time`, same title).
/// Modified exceptions (moved start or different title) stay visible. When no
/// master exists (window-only never-init), instances stay visible.
pub const EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL: &str = "
    SELECT e.* FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND c.sync_enabled = 1
      AND e.deleted_at IS NULL
      AND (e.status IS NULL OR e.status = '' OR e.status != 'cancelled')
      AND e.start_time < ? AND e.end_time > ?
      AND NOT (
        e.recurring_event_id IS NOT NULL AND e.recurring_event_id != ''
        AND (e.original_start IS NULL OR e.original_start = '' OR e.original_start = e.start_time)
        AND EXISTS (
          SELECT 1 FROM calendar_events m
          WHERE m.calendar_id = e.calendar_id
            AND m.google_event_id = e.recurring_event_id
            AND m.deleted_at IS NULL
            AND m.recurrence IS NOT NULL AND m.recurrence != ''
            AND e.title = m.title
        )
      )
    ORDER BY e.start_time ASC
";

/// The derived "running" set: task-tagged events joined to the user's living,
/// sync-enabled calendars where `task_id` is set AND `start_time <= now < end_time`.
/// Soft-deleted or user-disabled parents are excluded (same as the range query).
/// SQLite evaluates `NULL != ''` to NULL (falsy), so the NULL guard before the
/// empty-string test is required, not cosmetic. RFC 3339 UTC strings of this
/// shape (`…Z`, zero-padded, no fractions) compare lexicographically, so the
/// range test needs no timestamp function.
///
/// Running task chips stay **timed only** (`is_all_day = 0`); cancelled rows
/// and unmodified window instances are hidden when a living master exists
/// (same instance dedupe as [`EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL`]).
pub const EVENT_LIST_RUNNING_BY_USER_ID_SQL: &str = "
    SELECT e.* FROM calendar_events e
    JOIN google_calendars c ON c.id = e.calendar_id
    WHERE c.user_id = ?
      AND c.deleted_at IS NULL
      AND c.sync_enabled = 1
      AND e.deleted_at IS NULL
      AND e.is_all_day = 0
      AND (e.status IS NULL OR e.status = '' OR e.status != 'cancelled')
      AND e.task_id IS NOT NULL AND e.task_id != ''
      AND e.start_time <= ? AND e.end_time > ?
      AND NOT (
        e.recurring_event_id IS NOT NULL AND e.recurring_event_id != ''
        AND (e.original_start IS NULL OR e.original_start = '' OR e.original_start = e.start_time)
        AND EXISTS (
          SELECT 1 FROM calendar_events m
          WHERE m.calendar_id = e.calendar_id
            AND m.google_event_id = e.recurring_event_id
            AND m.deleted_at IS NULL
            AND m.recurrence IS NOT NULL AND m.recurrence != ''
            AND e.title = m.title
        )
      )
    ORDER BY e.start_time ASC
";

/// Natural-key id lookup **including soft-deleted rows**. Used before upsert so
/// ON CONFLICT updates the living/deleted row and the caller returns the
/// persisted id (never a discarded candidate UUID).
pub const EVENT_GET_ID_BY_NATURAL_KEY_SQL: &str =
    "SELECT id FROM calendar_events WHERE calendar_id = ? AND google_event_id = ?";

/// SOFT delete by local id.
pub const EVENT_DELETE_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// Reassign living event to another calendar (keeps local id).
pub const EVENT_REASSIGN_CALENDAR_SQL: &str =
    "UPDATE calendar_events SET calendar_id = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL";

/// SOFT delete by `(calendar_id, google_event_id)`.
pub const EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE calendar_id = ? AND google_event_id = ? AND deleted_at IS NULL";

/// Fenced soft-delete by `(calendar_id, google_event_id)`: same SET as
/// [`EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL`] plus an EXISTS gate on
/// `google_calendars` so a stolen/expired owner cannot tombstone rows.
/// Binds: now, now, calendar_id, google_event_id, lease_owner, now.
pub const EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL: &str = "
    UPDATE calendar_events
    SET deleted_at = ?, updated_at = ?
    WHERE calendar_id = ? AND google_event_id = ? AND deleted_at IS NULL
      AND EXISTS (
        SELECT 1 FROM google_calendars
        WHERE id = calendar_events.calendar_id
          AND deleted_at IS NULL
          AND lease_owner = ?
          AND (lease_expires_at IS NULL OR lease_expires_at >= ?)
      )
";

/// Probe whether `lease_owner` still holds an unexpired lease on a living
/// calendar. Used after a fenced delete (which may change 0 rows when no
/// living event existed) to distinguish lease loss from idempotent no-op.
/// Binds: calendar_id, lease_owner, now.
pub const CALENDAR_LEASE_HELD_SQL: &str = "
    SELECT 1 AS ok FROM google_calendars
    WHERE id = ?
      AND deleted_at IS NULL
      AND lease_owner = ?
      AND (lease_expires_at IS NULL OR lease_expires_at >= ?)
";

/// SOFT delete of stale rows (older than a cutoff).
/// **Unused by the replica walk** — see [`CalendarEventRepo::sweep_absent_if_owner`].
pub const EVENT_DELETE_STALE_SQL: &str =
    "UPDATE calendar_events SET deleted_at = ?, updated_at = ? WHERE calendar_id = ? AND last_synced_at < ? AND deleted_at IS NULL";

/// Clear all generation-membership seen rows for one calendar.
/// Binds: calendar_id.
pub const EVENT_CLEAR_REPLICA_SEEN_FOR_CALENDAR_SQL: &str =
    "DELETE FROM calendar_replica_seen WHERE calendar_id = ?";

/// Upsert a deterministic poison quarantine row (issue #60).
/// ON CONFLICT replaces payload + error_class, bumps attempt_count, stamps
/// last_attempt_at; first_seen_at is unchanged.
/// Binds: calendar_id, google_event_id, phase, error_class, replay_payload,
/// first_seen_at (= now on insert), last_attempt_at (= now).
/// Never stores OAuth tokens, lease owners, or channel ids.
pub const EVENT_UPSERT_QUARANTINE_SQL: &str = "
    INSERT INTO calendar_event_quarantine (
      calendar_id, google_event_id, phase, error_class, replay_payload,
      first_seen_at, last_attempt_at, attempt_count
    ) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
    ON CONFLICT(calendar_id, google_event_id, phase) DO UPDATE SET
      error_class = excluded.error_class,
      replay_payload = excluded.replay_payload,
      last_attempt_at = excluded.last_attempt_at,
      attempt_count = calendar_event_quarantine.attempt_count + 1
";

/// Drop all quarantine rows for one calendar (after successful publish).
/// Binds: calendar_id.
pub const EVENT_CLEAR_QUARANTINE_FOR_CALENDAR_SQL: &str =
    "DELETE FROM calendar_event_quarantine WHERE calendar_id = ?";

/// Fenced generation-membership sweep (issue #60). Soft-deletes living rows
/// whose Google id is absent from this walk's completed seen snapshot.
/// Does **not** use `last_synced_at`. Excludes cancelled exceptions and
/// non-empty `task_id`. Lease EXISTS matches
/// [`EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL`].
/// Binds: now, now, calendar_id, calendar_id, run_id, lease_owner, now.
pub const EVENT_SWEEP_ABSENT_IF_OWNER_SQL: &str = "
    UPDATE calendar_events
    SET deleted_at = ?, updated_at = ?
    WHERE calendar_id = ?
      AND deleted_at IS NULL
      AND NOT (
        status = 'cancelled'
        AND recurring_event_id IS NOT NULL
        AND TRIM(recurring_event_id) != ''
      )
      AND NOT (
        task_id IS NOT NULL
        AND TRIM(task_id) != ''
      )
      AND google_event_id NOT IN (
        SELECT google_event_id FROM calendar_replica_seen
        WHERE calendar_id = ? AND run_id = ?
      )
      AND EXISTS (
        SELECT 1 FROM google_calendars
        WHERE id = calendar_events.calendar_id
          AND deleted_at IS NULL
          AND lease_owner = ?
          AND (lease_expires_at IS NULL OR lease_expires_at >= ?)
      )
";

/// D1 allows at most 100 bound parameters per SQL statement.
/// `calendar_events` upsert binds 23 columns per row → max 4 rows per statement.
pub const EVENT_UPSERT_COL_COUNT: usize = 23;
pub const EVENT_UPSERT_CHUNK_SIZE: usize = 100 / EVENT_UPSERT_COL_COUNT; // 4

/// `calendar_replica_seen` insert binds 4 columns per row → max 25 rows.
pub const REPLICA_SEEN_INSERT_COL_COUNT: usize = 4;
pub const REPLICA_SEEN_INSERT_CHUNK_SIZE: usize = 100 / REPLICA_SEEN_INSERT_COL_COUNT; // 25

/// Builds a multi-row `INSERT OR IGNORE` into `calendar_replica_seen` for one
/// chunk (non-empty and ≤ [`REPLICA_SEEN_INSERT_CHUNK_SIZE`]).
/// Binds per row: calendar_id, run_id, google_event_id, created_at.
pub fn build_replica_seen_insert_sql(
    calendar_id: &str,
    run_id: &str,
    google_event_ids: &[String],
    now_rfc3339: &str,
) -> (String, Vec<String>) {
    assert!(
        !google_event_ids.is_empty(),
        "replica_seen insert chunk must not be empty"
    );
    assert!(
        google_event_ids.len() <= REPLICA_SEEN_INSERT_CHUNK_SIZE,
        "replica_seen insert chunk exceeds {REPLICA_SEEN_INSERT_CHUNK_SIZE} rows"
    );

    let mut sql = String::from(
        "INSERT OR IGNORE INTO calendar_replica_seen \
         (calendar_id, run_id, google_event_id, created_at) VALUES ",
    );
    let mut args: Vec<String> =
        Vec::with_capacity(google_event_ids.len() * REPLICA_SEEN_INSERT_COL_COUNT);
    for (index, gid) in google_event_ids.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?)");
        args.push(calendar_id.to_string());
        args.push(run_id.to_string());
        args.push(gid.clone());
        args.push(now_rfc3339.to_string());
    }
    (sql, args)
}

const EVENT_UPSERT_ON_CONFLICT: &str = "
    ON CONFLICT(calendar_id, google_event_id) DO UPDATE SET
        google_etag = excluded.google_etag,
        google_updated_at = excluded.google_updated_at,
        last_synced_at = excluded.last_synced_at,
        title = excluded.title,
        description = excluded.description,
        start_time = excluded.start_time,
        end_time = excluded.end_time,
        recurrence = excluded.recurrence,
        task_id = COALESCE(NULLIF(excluded.task_id, ''), calendar_events.task_id),
        ical_uid = excluded.ical_uid,
        sequence = excluded.sequence,
        status = excluded.status,
        recurring_event_id = excluded.recurring_event_id,
        original_start = excluded.original_start,
        start_time_zone = excluded.start_time_zone,
        end_time_zone = excluded.end_time_zone,
        is_all_day = excluded.is_all_day,
        raw_json = excluded.raw_json,
        updated_at = excluded.updated_at,
        deleted_at = NULL
";

/// Builds a multi-row `INSERT … ON CONFLICT` statement for one chunk of
/// events (non-empty and ≤ `EVENT_UPSERT_CHUNK_SIZE`). `ids` supplies the new
/// UUID for each row and must match `events.len()` — the D1 implementation
/// generates them (api-core stays free of a UUID dependency).
///
/// Returns `(sql, args)` where every arg is a string; the D1 implementation
/// binds them as `D1Type::Text`. 23 columns; COALESCE-free apart from the
/// `task_id` guard — a Google event without the `sanctuary_task_id` property
/// must not wipe a stored link. New identity columns are Google-owned and
/// overwrite from `excluded.*` (no COALESCE).
pub fn build_event_upsert_sql(
    events: &[NewCalendarEvent],
    now_rfc3339: &str,
    ids: Vec<String>,
) -> (String, Vec<String>) {
    assert!(!events.is_empty(), "event upsert chunk must not be empty");
    assert!(
        events.len() <= EVENT_UPSERT_CHUNK_SIZE,
        "event upsert chunk exceeds {EVENT_UPSERT_CHUNK_SIZE} rows"
    );
    assert_eq!(events.len(), ids.len(), "one id per event required");

    let mut sql = String::from(
        "INSERT INTO calendar_events
        (id, calendar_id, google_event_id, google_etag, google_updated_at, last_synced_at, title, description, start_time, end_time, recurrence, task_id, ical_uid, sequence, status, recurring_event_id, original_start, start_time_zone, end_time_zone, is_all_day, raw_json, created_at, updated_at)
        VALUES ",
    );
    let mut args: Vec<String> = Vec::with_capacity(events.len() * EVENT_UPSERT_COL_COUNT);
    for (index, (event, id)) in events.iter().zip(ids).enumerate() {
        if index > 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)");
        args.extend([
            id,
            event.calendar_id.clone(),
            event.google_event_id.clone(),
            event.google_etag.clone(),
            event.google_updated_at.clone(),
            event.last_synced_at.clone(),
            event.title.clone(),
            event.description.clone(),
            event.start_time.clone(),
            event.end_time.clone(),
            event.recurrence.clone(),
            event.task_id.clone(),
            event.ical_uid.clone(),
            event.sequence.to_string(),
            event.status.clone(),
            event.recurring_event_id.clone(),
            event.original_start.clone(),
            event.start_time_zone.clone(),
            event.end_time_zone.clone(),
            if event.is_all_day {
                "1".to_string()
            } else {
                "0".to_string()
            },
            event.raw_json.clone(),
            now_rfc3339.to_string(),
            now_rfc3339.to_string(),
        ]);
    }
    sql.push(' ');
    sql.push_str(EVENT_UPSERT_ON_CONFLICT);
    (sql, args)
}

/// Builds a lease-fenced multi-row `INSERT … SELECT … WHERE EXISTS … ON
/// CONFLICT` for one chunk of events (non-empty and ≤
/// `EVENT_UPSERT_CHUNK_SIZE`). Same 23 columns and
/// [`EVENT_UPSERT_ON_CONFLICT`] as [`build_event_upsert_sql`], but the INSERT
/// is gated on `google_calendars` still holding `lease_owner` with an
/// unexpired lease (same predicate as
/// [`CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL`]). When the EXISTS fails the
/// SELECT returns zero rows and nothing is written.
///
/// Extra binds (once per statement, not per row): `calendar_id`,
/// `lease_owner`, `now_rfc3339` — so `23 * chunk + 3 ≤ 100` keeps
/// `EVENT_UPSERT_CHUNK_SIZE` at 4 (`92 + 3 = 95`).
///
/// `calendar_id` is the replica calendar id (all events in a chunk share it).
/// `ids` supplies the new UUID for each row and must match `events.len()`.
pub fn build_event_upsert_if_owner_sql(
    events: &[NewCalendarEvent],
    now_rfc3339: &str,
    ids: Vec<String>,
    calendar_id: &str,
    lease_owner: &str,
) -> (String, Vec<String>) {
    assert!(!events.is_empty(), "event upsert-if-owner chunk must not be empty");
    assert!(
        events.len() <= EVENT_UPSERT_CHUNK_SIZE,
        "event upsert-if-owner chunk exceeds {EVENT_UPSERT_CHUNK_SIZE} rows"
    );
    assert_eq!(events.len(), ids.len(), "one id per event required");

    let mut sql = String::from(
        "INSERT INTO calendar_events
        (id, calendar_id, google_event_id, google_etag, google_updated_at, last_synced_at, title, description, start_time, end_time, recurrence, task_id, ical_uid, sequence, status, recurring_event_id, original_start, start_time_zone, end_time_zone, is_all_day, raw_json, created_at, updated_at)
        SELECT * FROM (",
    );
    let mut args: Vec<String> =
        Vec::with_capacity(events.len() * EVENT_UPSERT_COL_COUNT + 3);
    for (index, (event, id)) in events.iter().zip(ids).enumerate() {
        if index > 0 {
            sql.push_str(" UNION ALL ");
        }
        sql.push_str("SELECT ?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?");
        args.extend([
            id,
            event.calendar_id.clone(),
            event.google_event_id.clone(),
            event.google_etag.clone(),
            event.google_updated_at.clone(),
            event.last_synced_at.clone(),
            event.title.clone(),
            event.description.clone(),
            event.start_time.clone(),
            event.end_time.clone(),
            event.recurrence.clone(),
            event.task_id.clone(),
            event.ical_uid.clone(),
            event.sequence.to_string(),
            event.status.clone(),
            event.recurring_event_id.clone(),
            event.original_start.clone(),
            event.start_time_zone.clone(),
            event.end_time_zone.clone(),
            if event.is_all_day {
                "1".to_string()
            } else {
                "0".to_string()
            },
            event.raw_json.clone(),
            now_rfc3339.to_string(),
            now_rfc3339.to_string(),
        ]);
    }
    sql.push_str(
        ")
        WHERE EXISTS (
          SELECT 1 FROM google_calendars
          WHERE id = ?
            AND deleted_at IS NULL
            AND lease_owner = ?
            AND (lease_expires_at IS NULL OR lease_expires_at >= ?)
        ) ",
    );
    args.push(calendar_id.to_string());
    args.push(lease_owner.to_string());
    args.push(now_rfc3339.to_string());
    sql.push_str(EVENT_UPSERT_ON_CONFLICT);
    (sql, args)
}

// ──────────────────────────────────────────
// Watch channel SQL
// ──────────────────────────────────────────

/// Plain INSERT, not an upsert: `channel_id` is UNIQUE and renewal mints a new
/// row rather than replacing an old one — overlap of two rows per calendar is
/// expected (ADR 0001). The D1 implementation supplies `id` (UUIDv4) and
/// `created_at`/`updated_at` from the passed `now_rfc3339`.
pub const WATCH_CHANNEL_INSERT_SQL: &str = "
    INSERT INTO google_calendars_watch_channels
        (id, calendar_id, channel_id, resource_id, token, expiration, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
";

pub const WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL: &str =
    "SELECT * FROM google_calendars_watch_channels WHERE channel_id = ?";

pub const WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL: &str =
    "SELECT * FROM google_calendars_watch_channels WHERE calendar_id = ? ORDER BY created_at ASC";

/// Every channel row. Small personal-app table; cron leftover-stop pass.
pub const WATCH_CHANNEL_LIST_ALL_SQL: &str =
    "SELECT * FROM google_calendars_watch_channels";

/// RFC 3339 UTC strings compare correctly as text, so `expiration > ?` finds
/// channels that are still valid.
pub const WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL: &str = "
    SELECT * FROM google_calendars_watch_channels
    WHERE calendar_id = ? AND expiration > ?
    ORDER BY created_at ASC
";

/// HARD delete: this table has no `deleted_at` (ADR 0001), so rows are
/// physically removed.
pub const WATCH_CHANNEL_DELETE_BY_ID_SQL: &str =
    "DELETE FROM google_calendars_watch_channels WHERE id = ?";

/// HARD delete of every row for a calendar. `channels.stop` runs per row
/// before this; overlap rows are removed together (ADR 0001).
pub const WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL: &str =
    "DELETE FROM google_calendars_watch_channels WHERE calendar_id = ?";

// ──────────────────────────────────────────
// Calendar event operation journal SQL (issue #50 / Vertical 4)
// ──────────────────────────────────────────

/// Cap on statuses passed to [`build_operation_list_by_statuses_sql`] —
/// well under D1's 100-parameter limit; the full status set is five values.
pub const OPERATION_LIST_BY_STATUSES_MAX: usize = 8;

/// Plain INSERT, not an upsert. The D1 implementation supplies `id` (UUIDv4)
/// and `created_at`/`updated_at` from the passed `now_rfc3339`.
/// `attempt_count` uses the column default (0); callers pass status
/// `"pending"`. Binds: id, user_id, calendar_id, local_event_id,
/// google_event_id, verb, payload_fingerprint, payload_json, status,
/// google_etag, created_at, updated_at.
pub const OPERATION_INSERT_SQL: &str = "
    INSERT INTO calendar_event_operations
        (id, user_id, calendar_id, local_event_id, google_event_id, verb,
         payload_fingerprint, payload_json, status, google_etag,
         created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
";

pub const OPERATION_GET_BY_ID_SQL: &str =
    "SELECT * FROM calendar_event_operations WHERE id = ?";

/// In-flight repair set: `pending` and `google_committed` only, oldest first.
/// Cron repair walks these; terminal statuses (`cache_applied`, `failed`,
/// `conflict`) are excluded.
pub const OPERATION_LIST_INFLIGHT_SQL: &str = "
    SELECT * FROM calendar_event_operations
    WHERE status IN ('pending', 'google_committed')
    ORDER BY updated_at ASC
";

/// In-flight google event ids for one calendar (replica skip). Same status
/// filter as [`OPERATION_LIST_INFLIGHT_SQL`]. Binds: calendar_id.
pub const OPERATION_LIST_INFLIGHT_GOOGLE_IDS_SQL: &str = "
    SELECT google_event_id FROM calendar_event_operations
    WHERE calendar_id = ?
      AND status IN ('pending', 'google_committed')
    ORDER BY updated_at ASC
";

/// Status-only update. Does not touch payload, etag, event ids, or
/// attempt_count. Binds: status, last_error, updated_at, id.
pub const OPERATION_UPDATE_STATUS_SQL: &str = "
    UPDATE calendar_event_operations
    SET status = ?, last_error = ?, updated_at = ?
    WHERE id = ?
";

/// Progress update after Google/cache steps. Binds: status, google_event_id,
/// local_event_id, google_etag, last_error, bump (0|1), updated_at, id.
/// `attempt_count = attempt_count + ?` so one statement covers both bump and
/// no-bump paths.
pub const OPERATION_UPDATE_PROGRESS_SQL: &str = "
    UPDATE calendar_event_operations
    SET status = ?,
        google_event_id = ?,
        local_event_id = ?,
        google_etag = ?,
        last_error = ?,
        attempt_count = attempt_count + ?,
        updated_at = ?
    WHERE id = ?
";

/// Builds `SELECT * FROM calendar_event_operations WHERE status IN (…)` for a
/// non-empty status list (≤ [`OPERATION_LIST_BY_STATUSES_MAX`]). Returns
/// `(sql, args)` with every arg a string (bound as `D1Type::Text`). Empty
/// input is handled by the trait impl (returns `Ok(vec![])` without a query).
pub fn build_operation_list_by_statuses_sql(statuses: &[&str]) -> (String, Vec<String>) {
    assert!(!statuses.is_empty(), "operation status list must not be empty");
    assert!(
        statuses.len() <= OPERATION_LIST_BY_STATUSES_MAX,
        "operation status list exceeds {OPERATION_LIST_BY_STATUSES_MAX} statuses"
    );
    let placeholders: Vec<&str> = statuses.iter().map(|_| "?").collect();
    let sql = format!(
        "SELECT * FROM calendar_event_operations WHERE status IN ({}) ORDER BY updated_at ASC",
        placeholders.join(", ")
    );
    let args = statuses.iter().map(|s| (*s).to_string()).collect();
    (sql, args)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_get_by_id_unfiltered_has_no_deleted_at_filter() {
        let sql = CALENDAR_GET_BY_ID_UNFILTERED_SQL;
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        assert!(
            !sql.contains("deleted_at"),
            "unfiltered read must see soft-deleted rows: {sql}"
        );
    }

    #[test]
    fn watch_channel_list_all_has_no_filter() {
        let sql = WATCH_CHANNEL_LIST_ALL_SQL;
        assert!(
            sql.contains("FROM google_calendars_watch_channels"),
            "{sql}"
        );
        assert!(!sql.contains("WHERE"), "{sql}");
        assert!(!sql.contains("deleted_at"), "{sql}");
    }

    #[test]
    fn calendar_and_event_deletes_are_soft_not_hard() {
        for sql in [
            CALENDAR_DELETE_SQL,
            EVENT_DELETE_SQL,
            EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL,
            EVENT_DELETE_STALE_SQL,
        ] {
            assert!(sql.starts_with("UPDATE"), "{sql}");
            assert!(sql.contains("SET deleted_at = ?"), "{sql}");
            assert!(!sql.contains("DELETE FROM"), "{sql}");
        }
    }

    #[test]
    fn event_reassign_calendar_sql_updates_living_row() {
        assert_eq!(
            EVENT_REASSIGN_CALENDAR_SQL,
            "UPDATE calendar_events SET calendar_id = ?, updated_at = ? WHERE id = ? AND deleted_at IS NULL"
        );
        assert!(EVENT_REASSIGN_CALENDAR_SQL.starts_with("UPDATE"), "{EVENT_REASSIGN_CALENDAR_SQL}");
        assert!(!EVENT_REASSIGN_CALENDAR_SQL.contains("DELETE FROM"), "{EVENT_REASSIGN_CALENDAR_SQL}");
    }

    #[test]
    fn calendar_upsert_preserves_sync_token_and_last_synced_at() {
        assert!(
            CALENDAR_UPSERT_SQL.contains(
                "sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token)"
            ),
            "{CALENDAR_UPSERT_SQL}"
        );
        assert!(
            CALENDAR_UPSERT_SQL.contains(
                "last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at)"
            ),
            "{CALENDAR_UPSERT_SQL}"
        );
    }

    #[test]
    fn calendar_upsert_never_mentions_event_labels() {
        // The event-label cache is a dedicated UPDATE
        // (`CALENDAR_SET_EVENT_LABELS_SQL`); a calendarList re-import must not
        // wipe an existing cache, so the upsert must not write the column.
        assert!(
            !CALENDAR_UPSERT_SQL.contains("event_labels"),
            "upsert must not touch event_labels: {CALENDAR_UPSERT_SQL}"
        );
        assert!(
            !CALENDAR_UPSERT_SQL.contains("event_labels_updated_at"),
            "upsert must not touch event_labels_updated_at: {CALENDAR_UPSERT_SQL}"
        );
        assert!(
            !CALENDAR_UPSERT_SQL.contains("eventLabels"),
            "upsert must not touch event labels: {CALENDAR_UPSERT_SQL}"
        );
    }

    #[test]
    fn calendar_set_event_labels_writes_column_and_stamps_updated_at() {
        let sql = CALENDAR_SET_EVENT_LABELS_SQL;
        assert!(sql.contains("event_labels = ?"), "{sql}");
        assert!(sql.contains("event_labels_updated_at = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn calendar_set_watch_coverage_writes_column_and_stamps_updated_at() {
        let sql = CALENDAR_SET_WATCH_COVERAGE_SQL;
        assert!(sql.contains("watch_coverage = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        // Never writes channel secrets.
        assert!(!sql.contains("token"), "{sql}");
        assert!(!sql.contains("resource_id"), "{sql}");
        assert!(!sql.contains("channel_id"), "{sql}");
    }

    #[test]
    fn calendar_set_event_coverage_writes_column_and_stamps_updated_at() {
        let sql = CALENDAR_SET_EVENT_COVERAGE_SQL;
        assert!(sql.contains("event_coverage = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("channel_id"), "{sql}");
        assert!(!sql.contains("resource_id"), "{sql}");
        assert!(!sql.contains("replay_payload"), "{sql}");
    }

    #[test]
    fn migration_0014_adds_watch_coverage_without_secrets() {
        let migration =
            include_str!("../../../../apps/worker/migrations/0014_watch_coverage.sql");
        assert!(
            migration.contains("ALTER TABLE google_calendars ADD COLUMN watch_coverage"),
            "migration must add watch_coverage column: {migration}"
        );
        assert!(
            migration.contains("DEFAULT 'missing'"),
            "default must be missing: {migration}"
        );
        assert!(
            migration.contains("never stores channel tokens"),
            "comment must note secrets are not stored: {migration}"
        );
        // Must not introduce secret-bearing columns.
        assert!(
            !migration.contains("resource_id"),
            "must not add resource_id: {migration}"
        );
        assert!(
            !migration.contains("channel_id"),
            "must not add channel_id: {migration}"
        );
        assert!(
            !migration.contains(" ADD COLUMN token"),
            "must not add token column: {migration}"
        );
    }

    #[test]
    fn migration_0015_adds_replica_seen_without_tokens_or_secrets() {
        let migration =
            include_str!("../../../../apps/worker/migrations/0015_replica_generation.sql");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS calendar_replica_seen"),
            "migration must create calendar_replica_seen: {migration}"
        );
        assert!(
            migration.contains("PRIMARY KEY (calendar_id, run_id, google_event_id)"),
            "PK must be (calendar_id, run_id, google_event_id): {migration}"
        );
        assert!(
            migration.contains("idx_replica_seen_calendar_run"),
            "must index (calendar_id, run_id): {migration}"
        );
        assert!(
            migration.contains("created_at TEXT"),
            "timestamps must be TEXT RFC 3339: {migration}"
        );
        // No resume-cursor or secret-bearing columns (comments may mention
        // pageToken/nextSyncToken as deliberately omitted).
        assert!(
            !migration.contains("page_token") && !migration.contains("pageToken TEXT"),
            "must not add pageToken column: {migration}"
        );
        assert!(
            !migration.contains("sync_token") && !migration.contains("next_sync_token"),
            "must not add syncToken column: {migration}"
        );
        assert!(
            !migration.contains("channel_id"),
            "must not store channel_id: {migration}"
        );
        assert!(
            !migration.contains("resource_id"),
            "must not store resource_id: {migration}"
        );
        assert!(
            migration.contains("issue #60") || migration.contains("Issue #60"),
            "comment must reference issue #60: {migration}"
        );
    }

    #[test]
    fn migration_0016_adds_quarantine_and_event_coverage_without_secrets() {
        let migration =
            include_str!("../../../../apps/worker/migrations/0016_replica_quarantine.sql");
        assert!(
            migration.contains("ALTER TABLE google_calendars ADD COLUMN event_coverage"),
            "migration must add event_coverage column: {migration}"
        );
        assert!(
            migration.contains("DEFAULT 'complete'"),
            "default must be complete: {migration}"
        );
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS calendar_event_quarantine"),
            "migration must create calendar_event_quarantine: {migration}"
        );
        assert!(
            migration.contains("replay_payload"),
            "table must have replay_payload: {migration}"
        );
        assert!(
            migration.contains("PRIMARY KEY (calendar_id, google_event_id, phase)"),
            "PK must be (calendar_id, google_event_id, phase): {migration}"
        );
        assert!(
            migration.contains("issue #60") || migration.contains("Issue #60"),
            "comment must reference issue #60: {migration}"
        );
        // Must not introduce secret-bearing / cursor columns on the table.
        assert!(
            !migration.contains("sync_token"),
            "must not add sync_token: {migration}"
        );
        assert!(
            !migration.contains("channel_id"),
            "must not add channel_id: {migration}"
        );
        assert!(
            !migration.contains("resource_id"),
            "must not add resource_id: {migration}"
        );
        assert!(
            !migration.contains("lease_owner"),
            "must not add lease_owner: {migration}"
        );
    }

    #[test]
    fn calendar_list_orders_primary_first_then_summary() {
        let sql = CALENDAR_LIST_BY_USER_ID_SQL;
        let order_start = sql.find("ORDER BY").expect("has ORDER BY");
        assert_eq!(
            &sql[order_start..],
            "ORDER BY is_primary DESC, summary ASC"
        );
    }

    #[test]
    fn calendar_list_sync_enabled_filters_and_orders() {
        // The fallback cron's work list: only sync-enabled, non-deleted rows,
        // ordered by user then primary first then summary.
        let sql = CALENDAR_LIST_SYNC_ENABLED_SQL;
        assert!(sql.contains("sync_enabled = 1"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        let order_start = sql.find("ORDER BY").expect("has ORDER BY");
        assert_eq!(
            &sql[order_start..],
            "ORDER BY user_id ASC, is_primary DESC, summary ASC"
        );
    }

    #[test]
    fn event_range_query_uses_overlap_semantics() {
        let sql = EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL;
        assert!(sql.contains("e.start_time < ?"), "{sql}");
        assert!(sql.contains("e.end_time > ?"), "{sql}");
        assert!(sql.contains("c.user_id = ?"), "{sql}");
        assert!(sql.contains("ORDER BY e.start_time ASC"), "{sql}");
        // Range GET includes all-day rows (week grid); do not filter is_all_day.
        assert!(
            !sql.contains("e.is_all_day = 0"),
            "range list must include all-day: {sql}"
        );
        assert!(
            sql.contains("(e.status IS NULL OR e.status = '' OR e.status != 'cancelled')"),
            "{sql}"
        );
    }

    #[test]
    fn event_list_queries_require_living_enabled_parent() {
        for sql in [
            EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL,
            EVENT_LIST_RUNNING_BY_USER_ID_SQL,
        ] {
            assert!(sql.contains("c.user_id = ?"), "{sql}");
            assert!(sql.contains("c.deleted_at IS NULL"), "{sql}");
            assert!(sql.contains("c.sync_enabled = 1"), "{sql}");
        }
    }

    #[test]
    fn event_range_and_running_queries_share_cancelled_and_master_filters() {
        for sql in [
            EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL,
            EVENT_LIST_RUNNING_BY_USER_ID_SQL,
        ] {
            assert!(
                sql.contains("(e.status IS NULL OR e.status = '' OR e.status != 'cancelled')"),
                "{sql}"
            );
            assert!(sql.contains("e.deleted_at IS NULL"), "{sql}");
            // Unmodified window instances hidden when a living master exists.
            assert!(
                sql.contains("m.google_event_id = e.recurring_event_id"),
                "master join by recurring_event_id: {sql}"
            );
            assert!(
                sql.contains("m.recurrence IS NOT NULL AND m.recurrence != ''"),
                "master must be a series: {sql}"
            );
            assert!(
                sql.contains("e.original_start = e.start_time"),
                "unmodified expansion predicate: {sql}"
            );
            assert!(sql.contains("e.title = m.title"), "title match: {sql}");
        }
    }

    #[test]
    fn event_upsert_on_conflict_clears_deleted_at() {
        assert!(
            EVENT_UPSERT_ON_CONFLICT.contains("deleted_at = NULL"),
            "{EVENT_UPSERT_ON_CONFLICT}"
        );
        // DO UPDATE must not assign id — a fresh UUID on conflict is discarded
        // and the existing row id is kept (batch upsert mints without SELECT).
        // Match a bare column assignment (line-start / after comma), not
        // suffixes like google_event_id / recurring_event_id.
        let assigns_id = EVENT_UPSERT_ON_CONFLICT.lines().any(|line| {
            let trimmed = line.trim().trim_start_matches(',').trim();
            trimmed.starts_with("id =") || trimmed.starts_with("id=")
        });
        assert!(
            !assigns_id,
            "ON CONFLICT must not overwrite id: {EVENT_UPSERT_ON_CONFLICT}"
        );
    }

    #[test]
    fn event_get_id_by_natural_key_includes_soft_deleted() {
        let sql = EVENT_GET_ID_BY_NATURAL_KEY_SQL;
        assert!(sql.contains("SELECT id FROM calendar_events"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
        assert!(sql.contains("google_event_id = ?"), "{sql}");
        assert!(
            !sql.contains("deleted_at"),
            "must include soft-deleted rows: {sql}"
        );
    }

    #[test]
    fn event_upsert_chunk_size_respects_d1_100_param_limit() {
        assert_eq!(EVENT_UPSERT_COL_COUNT, 23);
        assert_eq!(EVENT_UPSERT_CHUNK_SIZE, 4);
        assert!(EVENT_UPSERT_CHUNK_SIZE * EVENT_UPSERT_COL_COUNT <= 100);
    }

    fn sample_new_event() -> NewCalendarEvent {
        NewCalendarEvent {
            calendar_id: "cal-1".to_string(),
            google_event_id: "g-1".to_string(),
            google_etag: "etag".to_string(),
            google_updated_at: "2026-08-17T10:00:00Z".to_string(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: "Standup".to_string(),
            description: String::new(),
            start_time: "2026-08-18T09:00:00Z".to_string(),
            end_time: "2026-08-18T09:30:00Z".to_string(),
            recurrence: String::new(),
            task_id: "task-1".to_string(),
            ical_uid: "uid-1".to_string(),
            sequence: 3,
            status: "confirmed".to_string(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: "UTC".to_string(),
            end_time_zone: "UTC".to_string(),
            is_all_day: false,
            raw_json: r#"{"id":"g-1"}"#.to_string(),
        }
    }

    #[test]
    fn event_upsert_sql_has_23_placeholders_per_row_and_on_conflict() {
        let event = sample_new_event();
        let (sql, args) = build_event_upsert_sql(
            &[event.clone()],
            "2026-08-17T12:00:00Z",
            vec!["evt-1".to_string()],
        );

        assert!(sql.starts_with("INSERT INTO calendar_events"), "{sql}");
        assert!(sql.contains("ON CONFLICT(calendar_id, google_event_id)"), "{sql}");
        assert!(
            !sql.contains("ON CONFLICT(google_event_id)"),
            "must not use a global unique on google_event_id: {sql}"
        );
        assert!(sql.contains("google_etag = excluded.google_etag"), "{sql}");
        assert!(sql.contains("updated_at = excluded.updated_at"), "{sql}");
        assert!(sql.contains("ical_uid = excluded.ical_uid"), "{sql}");
        assert!(sql.contains("sequence = excluded.sequence"), "{sql}");
        assert!(sql.contains("status = excluded.status"), "{sql}");
        assert!(sql.contains("recurring_event_id = excluded.recurring_event_id"), "{sql}");
        assert!(sql.contains("original_start = excluded.original_start"), "{sql}");
        assert!(sql.contains("start_time_zone = excluded.start_time_zone"), "{sql}");
        assert!(sql.contains("end_time_zone = excluded.end_time_zone"), "{sql}");
        assert!(sql.contains("is_all_day = excluded.is_all_day"), "{sql}");
        assert!(sql.contains("raw_json = excluded.raw_json"), "{sql}");

        assert_eq!(args.len(), 23);
        assert_eq!(args[0], "evt-1");
        assert_eq!(args[1], "cal-1");
        assert_eq!(args[5], "2026-08-17T12:00:00Z", "last_synced_at bound");
        assert_eq!(args[11], "task-1", "task_id bound");
        assert_eq!(args[12], "uid-1", "ical_uid bound");
        assert_eq!(args[13], "3", "sequence bound as decimal string");
        assert_eq!(args[14], "confirmed", "status bound");
        assert_eq!(args[19], "0", "is_all_day bound as 0/1");
        assert_eq!(args[20], r#"{"id":"g-1"}"#, "raw_json bound");
        assert_eq!(args[21], "2026-08-17T12:00:00Z", "created_at bound");
        assert_eq!(args[22], "2026-08-17T12:00:00Z", "updated_at bound");

        // Exactly 23 placeholders for the single row (no trailing/extra commas).
        assert_eq!(
            sql.matches("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .count(),
            1
        );
        assert!(
            !sql.contains("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?"),
            "no 24th placeholder"
        );
    }

    #[test]
    fn event_upsert_sql_chunks_4_rows_with_92_placeholders() {
        let event = sample_new_event();
        let events: Vec<NewCalendarEvent> = (0..4).map(|_| event.clone()).collect();
        let ids: Vec<String> = (0..4).map(|i| format!("evt-{i}")).collect();
        let (sql, args) = build_event_upsert_sql(&events, "2026-08-17T12:00:00Z", ids);

        assert_eq!(args.len(), 4 * 23);
        assert_eq!(sql.matches('?').count(), 4 * 23);
        assert_eq!(
            sql.matches("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
                .count(),
            4
        );
        assert_eq!(args[0], "evt-0");
        assert_eq!(args[23], "evt-1");
        assert_eq!(args[23 * 3], "evt-3");
    }

    #[test]
    fn event_upsert_preserves_existing_task_id_when_incoming_is_empty() {
        // A sync of an untagged Google event must never wipe a stored task
        // link: the COALESCE keeps the existing value.
        assert!(
            EVENT_UPSERT_ON_CONFLICT.contains(
                "task_id = COALESCE(NULLIF(excluded.task_id, ''), calendar_events.task_id)"
            ),
            "{EVENT_UPSERT_ON_CONFLICT}"
        );
    }

    #[test]
    fn event_upsert_overwrites_google_owned_identity_columns() {
        // Identity columns are Google-owned: merge assigns from excluded.*,
        // no COALESCE protection (unlike task_id).
        for col in [
            "ical_uid = excluded.ical_uid",
            "sequence = excluded.sequence",
            "status = excluded.status",
            "recurring_event_id = excluded.recurring_event_id",
            "original_start = excluded.original_start",
            "start_time_zone = excluded.start_time_zone",
            "end_time_zone = excluded.end_time_zone",
            "is_all_day = excluded.is_all_day",
            "raw_json = excluded.raw_json",
        ] {
            assert!(
                EVENT_UPSERT_ON_CONFLICT.contains(col),
                "missing overwrite for {col}: {EVENT_UPSERT_ON_CONFLICT}"
            );
        }
        assert!(
            !EVENT_UPSERT_ON_CONFLICT.contains("COALESCE(NULLIF(excluded.ical_uid"),
            "ical_uid must not be COALESCE-protected: {EVENT_UPSERT_ON_CONFLICT}"
        );
    }

    #[test]
    fn calendar_upsert_does_not_clobber_sync_health() {
        // Health columns live on google_calendars but calendarList upsert must
        // never write them (same invariant as event_labels).
        for col in [
            "sync_status",
            "last_success_at",
            "last_attempt_at",
            "last_error_code",
            "failure_streak",
            "next_retry_at",
            "sync_query_fingerprint",
            "initial_sync_complete",
            "cache_revision",
            "dirty_requested_generation",
            "dirty_applied_generation",
            "full_sync_requested",
            "lease_owner",
            "lease_expires_at",
            "projection",
            "watch_coverage",
            "event_coverage",
        ] {
            assert!(
                !CALENDAR_UPSERT_SQL.contains(col),
                "upsert must not touch health column {col}: {CALENDAR_UPSERT_SQL}"
            );
        }
    }

    #[test]
    fn calendar_upsert_does_not_unconditionally_clobber_sync_enabled() {
        // Living rows keep the user's disable; only resurrect writes excluded.
        assert!(
            !CALENDAR_UPSERT_SQL.contains("sync_enabled = excluded.sync_enabled"),
            "unconditional assign would re-enable a user-disabled calendar: {CALENDAR_UPSERT_SQL}"
        );
        assert!(
            CALENDAR_UPSERT_SQL.contains(
                "sync_enabled = CASE\n          WHEN google_calendars.deleted_at IS NOT NULL THEN excluded.sync_enabled\n          ELSE google_calendars.sync_enabled\n        END"
            ) || CALENDAR_UPSERT_SQL.contains("WHEN google_calendars.deleted_at IS NOT NULL THEN excluded.sync_enabled"),
            "expected CASE preserve/resurrect for sync_enabled: {CALENDAR_UPSERT_SQL}"
        );
    }

    #[test]
    fn calendar_list_state_upsert_is_keyed_on_user_id_and_allows_empty_token() {
        let sql = CALENDAR_LIST_STATE_UPSERT_SQL;
        assert!(sql.contains("INSERT INTO google_calendar_list_state"), "{sql}");
        assert!(sql.contains("ON CONFLICT(user_id) DO UPDATE SET"), "{sql}");
        assert!(sql.contains("sync_token = excluded.sync_token"), "{sql}");
        assert!(sql.contains("updated_at = excluded.updated_at"), "{sql}");
        // Empty token is a normal bind value — no NOT NULL guard beyond the
        // column default; the SQL must not reject ''.
        assert!(!sql.contains("NULLIF(excluded.sync_token"), "{sql}");
    }

    #[test]
    fn calendar_list_state_get_selects_token_by_user() {
        let sql = CALENDAR_LIST_STATE_GET_SQL;
        assert!(sql.contains("FROM google_calendar_list_state"), "{sql}");
        assert!(sql.contains("WHERE user_id = ?"), "{sql}");
        assert!(sql.contains("sync_token"), "{sql}");
    }

    #[test]
    fn calendar_list_user_ids_distinct_living_ordered() {
        let sql = CALENDAR_LIST_USER_IDS_SQL;
        assert!(sql.contains("DISTINCT user_id"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("ORDER BY user_id ASC"), "{sql}");
    }

    #[test]
    fn record_sync_success_sql_writes_token_and_success_not_just_attempt() {
        let sql = CALENDAR_RECORD_SYNC_SUCCESS_SQL;
        assert!(sql.contains("sync_token = ?"), "{sql}");
        assert!(sql.contains("last_success_at = ?"), "{sql}");
        assert!(sql.contains("last_synced_at = ?"), "{sql}");
        assert!(sql.contains("failure_streak = 0"), "{sql}");
        assert!(sql.contains("cache_revision = cache_revision + 1"), "{sql}");
        assert!(sql.contains("sync_status = 'ready'"), "{sql}");
        assert!(sql.contains("initial_sync_complete = 1"), "{sql}");
        assert!(sql.contains("sync_query_fingerprint = ?"), "{sql}");
        assert!(sql.contains("full_sync_requested = 0"), "{sql}");
        assert!(sql.contains("event_coverage = 'complete'"), "{sql}");
        assert!(sql.contains("next_retry_at = NULL"), "{sql}");
        assert!(sql.contains("last_error_code = ''"), "{sql}");
    }

    #[test]
    fn record_sync_success_if_owner_sql_fences_on_lease_and_writes_token() {
        let sql = CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL;
        assert!(sql.contains("sync_token = ?"), "{sql}");
        assert!(sql.contains("last_success_at = ?"), "{sql}");
        assert!(sql.contains("last_synced_at = ?"), "{sql}");
        assert!(sql.contains("sync_query_fingerprint = ?"), "{sql}");
        assert!(sql.contains("cache_revision = cache_revision + 1"), "{sql}");
        assert!(sql.contains("full_sync_requested = 0"), "{sql}");
        assert!(sql.contains("event_coverage = 'complete'"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(
            sql.contains("lease_expires_at IS NULL OR lease_expires_at >= ?"),
            "{sql}"
        );
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn event_upsert_quarantine_sql_retains_payload_without_secrets() {
        let sql = EVENT_UPSERT_QUARANTINE_SQL;
        assert!(sql.contains("INSERT INTO calendar_event_quarantine"), "{sql}");
        assert!(sql.contains("replay_payload"), "{sql}");
        assert!(sql.contains("ON CONFLICT(calendar_id, google_event_id, phase)"), "{sql}");
        assert!(sql.contains("attempt_count = calendar_event_quarantine.attempt_count + 1"), "{sql}");
        assert!(sql.contains("first_seen_at"), "{sql}");
        // first_seen_at must not be overwritten on conflict.
        assert!(
            !sql.contains("first_seen_at = excluded"),
            "must leave first_seen_at unchanged on conflict: {sql}"
        );
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("channel_id"), "{sql}");
        assert!(!sql.contains("resource_id"), "{sql}");
        assert!(!sql.contains("lease_owner"), "{sql}");
    }

    #[test]
    fn event_clear_quarantine_for_calendar_sql_is_scoped() {
        let sql = EVENT_CLEAR_QUARANTINE_FOR_CALENDAR_SQL;
        assert!(sql.contains("DELETE FROM calendar_event_quarantine"), "{sql}");
        assert!(sql.contains("WHERE calendar_id = ?"), "{sql}");
    }

    #[test]
    fn record_sync_failure_sql_does_not_touch_event_coverage() {
        let sql = CALENDAR_RECORD_SYNC_FAILURE_SQL;
        assert!(!sql.contains("event_coverage"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
    }

    #[test]
    fn clear_authorization_required_sql_scopes_and_preserves_cursors() {
        let sql = CALENDAR_CLEAR_AUTHORIZATION_REQUIRED_SQL;
        assert!(sql.contains("sync_status = CASE"), "{sql}");
        assert!(sql.contains("'retrying'"), "{sql}");
        assert!(sql.contains("'never_initialized'"), "{sql}");
        assert!(sql.contains("initial_sync_complete = 1"), "{sql}");
        assert!(sql.contains("last_success_at IS NOT NULL"), "{sql}");
        assert!(sql.contains("last_error_code = ''"), "{sql}");
        assert!(sql.contains("failure_streak = 0"), "{sql}");
        assert!(sql.contains("next_retry_at = ?"), "{sql}");
        assert!(sql.contains("WHERE user_id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("sync_enabled = 1"), "{sql}");
        assert!(sql.contains("sync_status = 'authorization_required'"), "{sql}");
        // Must not clobber replica cursor / lease / dirty / reseed flag.
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("lease_owner"), "{sql}");
        assert!(!sql.contains("lease_expires_at"), "{sql}");
        assert!(!sql.contains("dirty_requested"), "{sql}");
        assert!(!sql.contains("dirty_applied"), "{sql}");
        assert!(!sql.contains("full_sync_requested"), "{sql}");
    }

    #[test]
    fn event_delete_by_google_event_id_if_owner_sql_fences_on_lease() {
        let sql = EVENT_DELETE_BY_GOOGLE_EVENT_ID_IF_OWNER_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("SET deleted_at = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("EXISTS"), "{sql}");
        assert!(sql.contains("google_calendars"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(
            sql.contains("lease_expires_at IS NULL OR lease_expires_at >= ?"),
            "{sql}"
        );
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("DELETE FROM"), "{sql}");
    }

    #[test]
    fn event_sweep_absent_if_owner_sql_fences_and_excludes_exceptions() {
        let sql = EVENT_SWEEP_ABSENT_IF_OWNER_SQL;
        assert!(sql.trim_start().starts_with("UPDATE"), "{sql}");
        assert!(sql.contains("SET deleted_at = ?"), "{sql}");
        assert!(sql.contains("calendar_replica_seen"), "{sql}");
        assert!(sql.contains("EXISTS"), "{sql}");
        assert!(sql.contains("google_calendars"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(
            sql.contains("lease_expires_at IS NULL OR lease_expires_at >= ?"),
            "{sql}"
        );
        // Generation membership — never a timestamp sweep.
        assert!(!sql.contains("last_synced_at"), "{sql}");
        // Cancelled exceptions + task-linked rows must survive.
        assert!(sql.contains("status = 'cancelled'"), "{sql}");
        assert!(sql.contains("recurring_event_id"), "{sql}");
        assert!(sql.contains("task_id"), "{sql}");
        assert!(sql.contains("TRIM(recurring_event_id)"), "{sql}");
        assert!(sql.contains("TRIM(task_id)"), "{sql}");
        assert!(!sql.contains("DELETE FROM"), "{sql}");
    }

    #[test]
    fn replica_seen_insert_sql_chunks_and_uses_or_ignore() {
        let ids: Vec<String> = (0..3).map(|i| format!("g-{i}")).collect();
        let (sql, args) =
            build_replica_seen_insert_sql("cal-1", "run-a", &ids, "2023-11-14T22:13:20Z");
        assert!(sql.starts_with("INSERT OR IGNORE INTO calendar_replica_seen"), "{sql}");
        assert_eq!(args.len(), 3 * REPLICA_SEEN_INSERT_COL_COUNT);
        assert_eq!(sql.matches('?').count(), 3 * REPLICA_SEEN_INSERT_COL_COUNT);
        assert_eq!(args[0], "cal-1");
        assert_eq!(args[1], "run-a");
        assert_eq!(args[2], "g-0");
        assert_eq!(args[3], "2023-11-14T22:13:20Z");
        assert_eq!(REPLICA_SEEN_INSERT_CHUNK_SIZE, 25);
        assert!(REPLICA_SEEN_INSERT_CHUNK_SIZE * REPLICA_SEEN_INSERT_COL_COUNT <= 100);
    }

    #[test]
    fn event_clear_replica_seen_sql_is_calendar_scoped_delete() {
        let sql = EVENT_CLEAR_REPLICA_SEEN_FOR_CALENDAR_SQL;
        assert!(sql.contains("DELETE FROM calendar_replica_seen"), "{sql}");
        assert!(sql.contains("WHERE calendar_id = ?"), "{sql}");
        assert!(!sql.contains("google_event_id"), "{sql}");
    }

    #[test]
    fn event_upsert_if_owner_sql_fences_on_lease_and_preserves_task_id() {
        let event = sample_new_event();
        let (sql, args) = build_event_upsert_if_owner_sql(
            &[event.clone()],
            "2026-08-17T12:00:00Z",
            vec!["evt-1".to_string()],
            "cal-1",
            "owner-a",
        );

        assert!(sql.starts_with("INSERT INTO calendar_events"), "{sql}");
        assert!(sql.contains("EXISTS"), "{sql}");
        assert!(sql.contains("google_calendars"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(
            sql.contains("lease_expires_at IS NULL OR lease_expires_at >= ?"),
            "{sql}"
        );
        assert!(sql.contains("ON CONFLICT(calendar_id, google_event_id)"), "{sql}");
        assert!(
            sql.contains(
                "task_id = COALESCE(NULLIF(excluded.task_id, ''), calendar_events.task_id)"
            ),
            "{sql}"
        );
        // 23 row binds + calendar_id + lease_owner + now.
        assert_eq!(args.len(), 23 + 3);
        assert_eq!(args[0], "evt-1");
        assert_eq!(args[23], "cal-1");
        assert_eq!(args[24], "owner-a");
        assert_eq!(args[25], "2026-08-17T12:00:00Z");
        assert_eq!(sql.matches('?').count(), 23 + 3);
    }

    #[test]
    fn event_upsert_if_owner_sql_chunks_4_rows_with_95_placeholders() {
        let event = sample_new_event();
        let events: Vec<NewCalendarEvent> = (0..4).map(|_| event.clone()).collect();
        let ids: Vec<String> = (0..4).map(|i| format!("evt-{i}")).collect();
        let (sql, args) = build_event_upsert_if_owner_sql(
            &events,
            "2026-08-17T12:00:00Z",
            ids,
            "cal-1",
            "owner-a",
        );

        // 4 * 23 row binds + 3 fence binds.
        assert_eq!(args.len(), 4 * 23 + 3);
        assert_eq!(sql.matches('?').count(), 4 * 23 + 3);
        assert!(4 * 23 + 3 <= 100);
        assert_eq!(args[0], "evt-0");
        assert_eq!(args[23], "evt-1");
        assert_eq!(args[23 * 3], "evt-3");
        assert_eq!(args[4 * 23], "cal-1");
        assert_eq!(args[4 * 23 + 1], "owner-a");
        assert_eq!(args[4 * 23 + 2], "2026-08-17T12:00:00Z");
    }

    #[test]
    fn try_acquire_lease_sql_allows_empty_same_or_expired() {
        let sql = CALENDAR_TRY_ACQUIRE_LEASE_SQL;
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(sql.contains("lease_expires_at = ?"), "{sql}");
        assert!(sql.contains("lease_owner = ''"), "{sql}");
        assert!(sql.contains("OR lease_owner = ?"), "{sql}");
        assert!(sql.contains("OR lease_expires_at IS NULL"), "{sql}");
        assert!(sql.contains("OR lease_expires_at < ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn release_lease_sql_does_not_touch_sync_token() {
        let sql = CALENDAR_RELEASE_LEASE_SQL;
        assert!(sql.contains("lease_owner = ''"), "{sql}");
        assert!(sql.contains("lease_expires_at = NULL"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("last_success_at"), "{sql}");
    }

    #[test]
    fn renew_lease_sql_only_owner() {
        let sql = CALENDAR_RENEW_LEASE_SQL;
        assert!(sql.contains("lease_expires_at = ?"), "{sql}");
        assert!(sql.contains("lease_owner = ?"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
    }

    #[test]
    fn bump_dirty_requested_sql_increments_generation_only() {
        let sql = CALENDAR_BUMP_DIRTY_REQUESTED_SQL;
        assert!(
            sql.contains("dirty_requested_generation = dirty_requested_generation + 1"),
            "{sql}"
        );
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("full_sync_requested"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("last_synced_at"), "{sql}");
    }

    #[test]
    fn mark_dirty_applied_sql_advances_applied_with_guard_only() {
        let sql = CALENDAR_MARK_DIRTY_APPLIED_SQL;
        assert!(sql.contains("dirty_applied_generation = ?"), "{sql}");
        assert!(sql.contains("dirty_applied_generation < ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("full_sync_requested"), "{sql}");
        assert!(
            !sql.contains("dirty_requested_generation ="),
            "must not write dirty_requested_generation: {sql}"
        );
    }

    #[test]
    fn record_sync_failure_sql_does_not_touch_token_or_success() {
        let sql = CALENDAR_RECORD_SYNC_FAILURE_SQL;
        assert!(sql.contains("last_error_code = ?"), "{sql}");
        assert!(sql.contains("failure_streak = failure_streak + 1"), "{sql}");
        assert!(sql.contains("sync_status = ?"), "{sql}");
        assert!(sql.contains("next_retry_at = ?"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("last_success_at"), "{sql}");
        assert!(!sql.contains("last_synced_at"), "{sql}");
        assert!(!sql.contains("last_attempt_at"), "{sql}");
        assert!(!sql.contains("full_sync_requested"), "{sql}");
    }

    #[test]
    fn record_sync_contention_sql_does_not_touch_streak_token_or_success() {
        let sql = CALENDAR_RECORD_SYNC_CONTENTION_SQL;
        assert!(sql.contains("last_error_code = ?"), "{sql}");
        assert!(sql.contains("sync_status = ?"), "{sql}");
        assert!(sql.contains("next_retry_at = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("failure_streak"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("last_success_at"), "{sql}");
        assert!(!sql.contains("last_synced_at"), "{sql}");
        assert!(!sql.contains("last_attempt_at"), "{sql}");
        assert!(!sql.contains("lease_owner"), "{sql}");
        assert!(!sql.contains("dirty_"), "{sql}");
        assert!(!sql.contains("full_sync_requested"), "{sql}");
    }

    #[test]
    fn begin_replica_reseed_sql_sets_flag_and_rebuilding_without_touching_token() {
        let sql = CALENDAR_BEGIN_REPLICA_RESEED_SQL;
        assert!(sql.contains("full_sync_requested = 1"), "{sql}");
        assert!(sql.contains("sync_status = 'rebuilding'"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ? AND deleted_at IS NULL"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("last_success_at"), "{sql}");
        assert!(!sql.contains("last_synced_at"), "{sql}");
        assert!(!sql.contains("last_attempt_at"), "{sql}");
        assert!(!sql.contains("last_error_code"), "{sql}");
        assert!(!sql.contains("failure_streak"), "{sql}");
        assert!(!sql.contains("lease_owner"), "{sql}");
        assert!(!sql.contains("dirty_"), "{sql}");
    }

    #[test]
    fn migration_0010_adds_sync_health_and_event_identity_columns() {
        // Path from packages/api-core → apps/worker/migrations.
        let migration = include_str!("../../../../apps/worker/migrations/0010_calendar_sync_health.sql");
        for col in [
            "sync_query_fingerprint",
            "sync_status",
            "initial_sync_complete",
            "last_attempt_at",
            "last_success_at",
            "last_error_code",
            "failure_streak",
            "next_retry_at",
            "dirty_requested_generation",
            "dirty_applied_generation",
            "full_sync_requested",
            "lease_owner",
            "lease_expires_at",
            "cache_revision",
            "projection",
            "ical_uid",
            "sequence",
            "status",
            "recurring_event_id",
            "original_start",
            "start_time_zone",
            "end_time_zone",
            "is_all_day",
            "raw_json",
        ] {
            assert!(
                migration.contains(col),
                "migration missing column {col}"
            );
        }
        assert!(
            migration.contains("UPDATE google_calendars"),
            "migration must backfill living rows from last_synced_at"
        );
        assert!(
            migration.contains("last_success_at = last_synced_at"),
            "backfill must copy last_synced_at → last_success_at"
        );
        assert!(
            migration.contains("never add a global")
                || migration.contains("UNIQUE (calendar_id, google_event_id)"),
            "migration must document UNIQUE (calendar_id, google_event_id) invariant"
        );
        assert!(
            migration.contains("calendarList")
                && migration.contains("must never touch these health columns"),
            "migration must document calendarList upsert health invariant"
        );
        assert!(
            migration.contains("task_id"),
            "migration must document task_id COALESCE protection"
        );
    }

    #[test]
    fn watch_channel_insert_is_insert_not_upsert_and_lists_adr_columns() {
        let sql = WATCH_CHANNEL_INSERT_SQL;
        assert!(sql.contains("INSERT INTO google_calendars_watch_channels"), "{sql}");
        assert!(!sql.contains("ON CONFLICT"), "{sql}");
        // Columns from the ADR DDL: every NOT NULL business column plus the
        // D1-generated `id` and timestamps.
        for column in [
            "id",
            "calendar_id",
            "channel_id",
            "resource_id",
            "token",
            "expiration",
            "created_at",
            "updated_at",
        ] {
            assert!(sql.contains(column), "missing {column} in {sql}");
        }
        assert_eq!(sql.matches('?').count(), 8, "one placeholder per column: {sql}");
        assert!(!sql.contains("deleted_at"), "{sql}");
    }

    #[test]
    fn watch_channel_deletes_are_hard_not_soft() {
        for sql in [WATCH_CHANNEL_DELETE_BY_ID_SQL, WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL] {
            assert!(sql.starts_with("DELETE FROM"), "{sql}");
            assert!(!sql.contains("UPDATE"), "{sql}");
            assert!(!sql.contains("deleted_at"), "{sql}");
        }
        assert!(WATCH_CHANNEL_DELETE_BY_ID_SQL.contains("WHERE id = ?"), "{}", WATCH_CHANNEL_DELETE_BY_ID_SQL);
        assert!(
            WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL.contains("WHERE calendar_id = ?"),
            "{}",
            WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL
        );
    }

    #[test]
    fn watch_channel_reads_have_no_deleted_at_filter() {
        // Hard-delete table: reads must not reference `deleted_at`.
        for sql in [
            WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL,
            WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL,
            WATCH_CHANNEL_LIST_ALL_SQL,
            WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
        ] {
            assert!(!sql.contains("deleted_at"), "{sql}");
        }
        assert!(WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL.contains("WHERE channel_id = ?"), "{}", WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL);
        assert!(WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL.contains("WHERE calendar_id = ?"), "{}", WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL);
    }

    #[test]
    fn watch_channel_unexpired_query_filters_on_expiration() {
        let sql = WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL;
        assert!(sql.contains("expiration > ?"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
    }

    #[test]
    fn running_events_query_filters_task_tagged_living_events_in_now_window() {
        let sql = EVENT_LIST_RUNNING_BY_USER_ID_SQL;
        assert!(sql.contains("c.user_id = ?"), "{sql}");
        assert!(sql.contains("e.deleted_at IS NULL"), "{sql}");
        assert!(sql.contains("e.is_all_day = 0"), "{sql}");
        assert!(
            sql.contains("(e.status IS NULL OR e.status = '' OR e.status != 'cancelled')"),
            "{sql}"
        );
        assert!(sql.contains("e.task_id IS NOT NULL AND e.task_id != ''"), "{sql}");
        assert!(sql.contains("e.start_time <= ? AND e.end_time > ?"), "{sql}");
        assert!(sql.contains("JOIN google_calendars c ON c.id = e.calendar_id"), "{sql}");
    }

    #[test]
    fn event_get_by_calendar_and_google_id_filters_living_rows() {
        let sql = EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL;
        assert!(sql.starts_with("SELECT * FROM calendar_events"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
        assert!(sql.contains("google_event_id = ?"), "{sql}");
        assert!(sql.contains("deleted_at IS NULL"), "{sql}");
    }

    // ── calendar_event_operations journal (issue #50 / Vertical 4) ──

    #[test]
    fn migration_0012_creates_calendar_event_operations_journal() {
        // Path from packages/api-core → apps/worker/migrations.
        let migration =
            include_str!("../../../../apps/worker/migrations/0012_calendar_event_operations.sql");
        assert!(
            migration.contains("CREATE TABLE IF NOT EXISTS calendar_event_operations"),
            "migration must create calendar_event_operations"
        );
        for col in [
            "id",
            "user_id",
            "calendar_id",
            "local_event_id",
            "google_event_id",
            "verb",
            "payload_fingerprint",
            "payload_json",
            "status",
            "google_etag",
            "attempt_count",
            "last_error",
            "created_at",
            "updated_at",
        ] {
            assert!(migration.contains(col), "migration missing column {col}");
        }
        // Status / verb machine documented in the migration comment.
        for token in [
            "pending",
            "google_committed",
            "cache_applied",
            "failed",
            "conflict",
            "insert",
            "patch",
            "delete",
        ] {
            assert!(
                migration.contains(token),
                "migration must document status/verb {token}"
            );
        }
        assert!(
            migration.contains("issue #50") || migration.contains("Vertical 4"),
            "migration must reference issue #50 / Vertical 4"
        );
        assert!(
            !migration.contains("deleted_at"),
            "journal has no soft-delete: {migration}"
        );
        assert!(
            migration.contains("idx_calendar_event_operations_status_updated")
                || migration.contains("(status, updated_at)"),
            "migration must index (status, updated_at) for cron repair"
        );
        assert!(
            migration.contains("(calendar_id, google_event_id)"),
            "migration must index (calendar_id, google_event_id)"
        );
        assert!(
            migration.contains("(user_id, calendar_id)"),
            "migration must index (user_id, calendar_id)"
        );
    }

    #[test]
    fn operation_insert_is_insert_not_upsert_and_lists_columns() {
        let sql = OPERATION_INSERT_SQL;
        assert!(
            sql.contains("INSERT INTO calendar_event_operations"),
            "{sql}"
        );
        assert!(!sql.contains("ON CONFLICT"), "{sql}");
        for column in [
            "id",
            "user_id",
            "calendar_id",
            "local_event_id",
            "google_event_id",
            "verb",
            "payload_fingerprint",
            "payload_json",
            "status",
            "google_etag",
            "created_at",
            "updated_at",
        ] {
            assert!(sql.contains(column), "missing {column} in {sql}");
        }
        // attempt_count uses the column default — not bound on insert.
        assert!(
            !sql.contains("attempt_count"),
            "insert leaves attempt_count to DEFAULT 0: {sql}"
        );
        assert_eq!(
            sql.matches('?').count(),
            12,
            "one placeholder per bound column: {sql}"
        );
        assert!(!sql.contains("deleted_at"), "{sql}");
    }

    #[test]
    fn operation_list_inflight_filters_pending_and_google_committed_only() {
        let sql = OPERATION_LIST_INFLIGHT_SQL;
        assert!(sql.contains("FROM calendar_event_operations"), "{sql}");
        assert!(sql.contains("'pending'"), "{sql}");
        assert!(sql.contains("'google_committed'"), "{sql}");
        assert!(!sql.contains("'failed'"), "{sql}");
        assert!(!sql.contains("'conflict'"), "{sql}");
        assert!(!sql.contains("'cache_applied'"), "{sql}");
        assert!(sql.contains("ORDER BY updated_at ASC"), "{sql}");
    }

    #[test]
    fn operation_list_inflight_google_ids_scoped_by_calendar() {
        let sql = OPERATION_LIST_INFLIGHT_GOOGLE_IDS_SQL;
        assert!(sql.contains("SELECT google_event_id"), "{sql}");
        assert!(sql.contains("FROM calendar_event_operations"), "{sql}");
        assert!(sql.contains("calendar_id = ?"), "{sql}");
        assert!(sql.contains("'pending'"), "{sql}");
        assert!(sql.contains("'google_committed'"), "{sql}");
        assert!(!sql.contains("'failed'"), "{sql}");
        assert!(!sql.contains("'conflict'"), "{sql}");
        assert!(!sql.contains("'cache_applied'"), "{sql}");
    }

    #[test]
    fn operation_update_progress_can_bump_attempt_and_write_fields() {
        let sql = OPERATION_UPDATE_PROGRESS_SQL;
        assert!(sql.contains("status = ?"), "{sql}");
        assert!(sql.contains("google_event_id = ?"), "{sql}");
        assert!(sql.contains("local_event_id = ?"), "{sql}");
        assert!(sql.contains("google_etag = ?"), "{sql}");
        assert!(sql.contains("last_error = ?"), "{sql}");
        assert!(
            sql.contains("attempt_count = attempt_count + ?"),
            "must support bump via bound 0/1: {sql}"
        );
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        assert!(!sql.contains("payload_json"), "{sql}");
        assert!(!sql.contains("payload_fingerprint"), "{sql}");
        assert!(!sql.contains("sync_token"), "{sql}");
    }

    #[test]
    fn operation_update_status_only_touches_status_error_and_updated_at() {
        let sql = OPERATION_UPDATE_STATUS_SQL;
        assert!(sql.contains("status = ?"), "{sql}");
        assert!(sql.contains("last_error = ?"), "{sql}");
        assert!(sql.contains("updated_at = ?"), "{sql}");
        assert!(sql.contains("WHERE id = ?"), "{sql}");
        // Must not touch payload bodies, etag, event ids, attempt_count, or
        // unrelated calendar sync columns.
        assert!(!sql.contains("sync_token"), "{sql}");
        assert!(!sql.contains("payload_json"), "{sql}");
        assert!(!sql.contains("payload_fingerprint"), "{sql}");
        assert!(!sql.contains("google_etag"), "{sql}");
        assert!(!sql.contains("google_event_id"), "{sql}");
        assert!(!sql.contains("local_event_id"), "{sql}");
        assert!(!sql.contains("attempt_count"), "{sql}");
    }

    #[test]
    fn operation_list_by_statuses_builder_orders_and_binds() {
        let (sql, args) =
            build_operation_list_by_statuses_sql(&["pending", "google_committed"]);
        assert!(sql.contains("FROM calendar_event_operations"), "{sql}");
        assert!(sql.contains("status IN (?, ?)"), "{sql}");
        assert!(sql.contains("ORDER BY updated_at ASC"), "{sql}");
        assert_eq!(args, vec!["pending".to_string(), "google_committed".to_string()]);
    }

    #[test]
    fn calendar_event_operation_deserializes_d1_shaped_json() {
        use crate::models::CalendarEventOperation;

        let op: CalendarEventOperation = serde_json::from_str(
            r#"{
                "id": "op-1",
                "user_id": "u-1",
                "calendar_id": "cal-1",
                "local_event_id": null,
                "google_event_id": null,
                "verb": "insert",
                "payload_fingerprint": "abc",
                "payload_json": "{}",
                "status": "pending",
                "google_etag": null,
                "attempt_count": 0,
                "last_error": null,
                "created_at": "2026-09-10T00:00:00Z",
                "updated_at": "2026-09-10T00:00:00Z"
            }"#,
        )
        .unwrap();
        assert_eq!(op.id, "op-1");
        assert_eq!(op.local_event_id, "", "NULL TEXT maps to empty string");
        assert_eq!(op.google_event_id, "");
        assert_eq!(op.google_etag, "");
        assert_eq!(op.last_error, "");
        assert_eq!(op.attempt_count, 0);
        assert_eq!(op.verb, "insert");
        assert_eq!(op.status, "pending");
    }

}

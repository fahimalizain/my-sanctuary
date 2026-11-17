//! Idempotent local-calendar seed for a standalone user (ADR 0012).
//!
//! A standalone user owns exactly one `google_calendars` row. It is written
//! with `sync_enabled = 1` (the event-range and running-task SQL filter on it)
//! and `sync_status = 'disabled'` (no sync, no stale banner, no repair), so the
//! row behaves like a real calendar for reads and writes while never entering
//! any Google path.
//!
//! [`ensure_standalone_calendar`] is safe to call on register, on login, and
//! from the standalone list self-heal: the repo's
//! `INSERT … ON CONFLICT(user_id, google_calendar_id) DO UPDATE SET deleted_at
//! = NULL` keeps an existing row (and its stored time zone/labels) untouched
//! and resurrects a soft-deleted local calendar.

use crate::models::StandaloneCalendarSeed;
use crate::repo::{standalone_event_labels_json, CalendarRepo, RepoError};
use crate::time::{parse_iana_tz, unix_secs_to_rfc3339};

use super::FALLBACK_TIME_ZONE;

/// Summary shown for the one local calendar.
pub const STANDALONE_CALENDAR_SUMMARY: &str = "Personal";

/// Deterministic local `google_calendars.id` for `user_id`.
pub fn standalone_calendar_local_id(user_id: &str) -> String {
    format!("standalone-cal-{user_id}")
}

/// Synthetic `google_calendar_id` — the value the write journal puts in URLs
/// and the stand-in resolves back through `(user_id, google_calendar_id)`.
pub fn standalone_google_calendar_id(user_id: &str) -> String {
    format!("standalone:{user_id}")
}

/// Seeds the user's local calendar. The conflict clause only clears
/// `deleted_at`, so this is idempotent, an existing row is never overwritten,
/// and a soft-deleted local calendar is resurrected.
///
/// `time_zone` is trimmed; empty/unknown IANA names store `UTC` (the same
/// fallback rule as [`crate::time::parse_iana_tz`] — never an error, since this
/// is a seed and the user must always end up with a usable calendar).
pub async fn ensure_standalone_calendar(
    calendars: &dyn CalendarRepo,
    user_id: &str,
    time_zone: &str,
    now_unix: i64,
) -> Result<(), RepoError> {
    let trimmed = time_zone.trim();
    let zone = if parse_iana_tz(trimmed).is_some() {
        trimmed.to_string()
    } else {
        FALLBACK_TIME_ZONE.to_string()
    };

    calendars
        .insert_standalone(&StandaloneCalendarSeed {
            id: standalone_calendar_local_id(user_id),
            user_id: user_id.to_string(),
            google_calendar_id: standalone_google_calendar_id(user_id),
            summary: STANDALONE_CALENDAR_SUMMARY.to_string(),
            time_zone: zone,
            event_labels: standalone_event_labels_json(),
            now_rfc3339: unix_secs_to_rfc3339(now_unix),
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::FakeCalendarRepo;
    use crate::google_color::GOOGLE_EVENT_LABEL_COLORS;

    const NOW: i64 = 1_700_000_000;

    fn seeded(calendars: &FakeCalendarRepo) -> Vec<crate::models::GoogleCalendar> {
        calendars.stored.lock().unwrap().clone()
    }

    #[test]
    fn seeds_one_disabled_sync_local_calendar() {
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(ensure_standalone_calendar(
            &calendars,
            "u-1",
            "Asia/Kolkata",
            NOW,
        ))
        .expect("seed");

        let rows = seeded(&calendars);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, "standalone-cal-u-1");
        assert_eq!(row.user_id, "u-1");
        assert_eq!(row.google_calendar_id, "standalone:u-1");
        assert_eq!(row.summary, "Personal");
        assert_eq!(row.time_zone, "Asia/Kolkata");
        assert!(row.is_primary);
        assert_eq!(row.access_role, "owner");
        assert!(row.sync_enabled, "the event-range/running SQL require it");
        assert_eq!(row.sync_status, "disabled");
        assert!(row.initial_sync_complete);
        assert_eq!(row.sync_token, "");
        assert_eq!(row.last_synced_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(row.last_success_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(row.last_error_code, "");
        assert_eq!(row.failure_streak, 0);
        assert_eq!(row.next_retry_at, None);
        assert!(!row.full_sync_requested);
        assert_eq!(row.watch_coverage, "covered");
        assert_eq!(row.event_coverage, "complete");
        assert_eq!(row.created_at, "2023-11-14T22:13:20Z");
        assert_eq!(row.updated_at, "2023-11-14T22:13:20Z");
        assert_eq!(
            row.event_labels_updated_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(row.event_labels, standalone_event_labels_json());

        let labels: Vec<serde_json::Value> =
            serde_json::from_str(&row.event_labels).expect("labels json");
        assert_eq!(labels.len(), GOOGLE_EVENT_LABEL_COLORS.len());
        assert_eq!(labels.len(), 24);
        assert_eq!(labels[0]["id"], "1");
        assert_eq!(labels[0]["backgroundColor"], GOOGLE_EVENT_LABEL_COLORS[0]);
        assert_eq!(labels[23]["id"], "24");
        assert_eq!(labels[23]["backgroundColor"], GOOGLE_EVENT_LABEL_COLORS[23]);
    }

    #[test]
    fn seeding_twice_keeps_the_original_row() {
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(ensure_standalone_calendar(
            &calendars,
            "u-1",
            "America/New_York",
            NOW,
        ))
        .expect("first seed");
        pollster::block_on(ensure_standalone_calendar(
            &calendars,
            "u-1",
            "Europe/Berlin",
            NOW + 3_600,
        ))
        .expect("second seed");

        let rows = seeded(&calendars);
        assert_eq!(rows.len(), 1, "the seed must not insert a second row");
        assert_eq!(rows[0].time_zone, "America/New_York");
        assert_eq!(rows[0].created_at, "2023-11-14T22:13:20Z");
        assert_eq!(rows[0].updated_at, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn seeding_resurrects_a_soft_deleted_calendar_and_keeps_its_zone() {
        let calendars = FakeCalendarRepo::with(Vec::new());
        pollster::block_on(ensure_standalone_calendar(
            &calendars,
            "u-1",
            "America/New_York",
            NOW,
        ))
        .expect("first seed");

        {
            let mut stored = calendars.stored.lock().unwrap();
            let row = stored.iter_mut().find(|row| row.user_id == "u-1").unwrap();
            row.deleted_at = Some("2023-11-15T00:00:00Z".to_string());
        }

        pollster::block_on(ensure_standalone_calendar(
            &calendars,
            "u-1",
            "UTC",
            NOW + 3_600,
        ))
        .expect("resurrecting seed");

        let rows = seeded(&calendars);
        assert_eq!(rows.len(), 1, "the same row comes back, never a duplicate");
        assert_eq!(rows[0].deleted_at, None, "the local calendar lives again");
        assert_eq!(
            rows[0].time_zone, "America/New_York",
            "the stored zone survives the resurrection"
        );
    }

    #[test]
    fn empty_and_unknown_zones_seed_utc() {
        for zone in ["", "   ", "Mars/Olympus_Mons"] {
            let calendars = FakeCalendarRepo::with(Vec::new());
            pollster::block_on(ensure_standalone_calendar(&calendars, "u-1", zone, NOW))
                .expect("seed");
            assert_eq!(seeded(&calendars)[0].time_zone, "UTC", "{zone:?}");
        }
    }

    #[test]
    fn repo_errors_propagate() {
        let calendars = FakeCalendarRepo::with(Vec::new());
        *calendars.fail_insert_standalone.lock().unwrap() = true;

        let err = pollster::block_on(ensure_standalone_calendar(&calendars, "u-1", "UTC", NOW))
            .unwrap_err();
        assert!(matches!(err, RepoError::Backend(_)), "{err:?}");
    }
}

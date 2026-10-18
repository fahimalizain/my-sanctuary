//! Rules against the real Worker migrations (local SQLite = D1 schema).
//!
//! Applies `apps/worker/migrations/*.sql`, seeds a user, a category, and a few
//! days of `daily_category_minutes`, then drives the api-core rules service
//! through [`SqliteRuleRepo`] and checks the scoreboard the Worker would
//! return. The pure scorer has its own (DB-free) tests in `api-core`.

use api_core::rules::repo::RuleRepo;
use api_core::{create_rule, scoreboard, NewRuleInput, RuleKind};
use d1_sync::{
    open_harness, seed_category, seed_category_seconds, seed_meta, seed_user_token_calendar,
    SeedOpts,
};

/// 2026-10-18T06:00:00Z — a UTC civil date of 2026-10-18.
const NOW_UNIX: i64 = 1_792_312_800;
const AS_OF: &str = "2026-10-18";
const NOW_RFC: &str = "2026-10-18T06:00:00Z";

fn params(json: &str) -> serde_json::Value {
    serde_json::from_str(json).expect("valid json")
}

fn learning_rule() -> NewRuleInput {
    NewRuleInput {
        name: "Learning".to_string(),
        icon: Some("learning".to_string()),
        kind: RuleKind::CategoryTotal,
        starts_on: Some("2026-09-01".to_string()),
        ends_on: None,
        params: params(
            r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                "window_days":7,"comparator":"gte","target_hours":4.0}"#,
        ),
    }
}

/// Aggregates `[from, to]` in `daily_meta` — the digest rows the scorer needs
/// to call a window full.
fn seed_meta_range(conn: &rusqlite::Connection, from: &str, to: &str, zone: &str) {
    let mut cursor = Some(from.to_string());
    while let Some(date) = cursor {
        if date.as_str() > to {
            break;
        }
        seed_meta(conn, "u-1", &date, zone, 0, 0).expect("meta");
        cursor = api_core::shift_date(&date, 1);
    }
}

#[test]
fn d1_rules_round_trip_and_score_through_the_service() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        seed_category(&conn, "u-1", "cat-learning", "learning", "Learning", false).expect("category");
        // 12h inside the 7-day window ending 2026-10-18, plus a day outside it.
        for date in ["2026-10-15", "2026-10-16", "2026-10-17"] {
            seed_category_seconds(&conn, "u-1", date, "cat-learning", 4 * 3600).expect("seconds");
        }
        seed_category_seconds(&conn, "u-1", "2026-09-20", "cat-learning", 4 * 3600)
            .expect("seconds");
        seed_category_seconds(&conn, "u-1", "2026-10-18", "", 1800).expect("unclassified");
        // Every day the rule's windows can reach is aggregated (the backfill
        // has caught up), so no window is a hole.
        seed_meta_range(&conn, "2026-08-01", AS_OF, "UTC");
    }

    let rules = h.rules();
    let created = pollster::block_on(create_rule(
        &rules,
        "u-1",
        &learning_rule(),
        AS_OF,
        NOW_RFC,
    ))
    .expect("creates");
    assert_eq!(created.starts_on, "2026-09-01");
    assert_eq!(created.condition, "≥4h / 7d");
    assert!(created.window_full);
    assert!(created.on_track, "12h in the window clears the 4h target");
    assert_eq!(created.streak, 3);
    // The 2026-09-20 day sits in seven consecutive windows (09-20 .. 09-26).
    assert_eq!(created.best_streak, 7);

    // One rule exists, so the board does not seed the eight defaults.
    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(board.as_of, AS_OF);
    assert_eq!(board.time_zone, "UTC");
    assert_eq!(board.active_rules, 1);
    assert_eq!(board.on_track, 1);
    assert_eq!(board.paused_rules, 0);
    assert!(board.warnings.is_empty(), "{:?}", board.warnings);
    assert_eq!(board.rules.len(), 1);
    assert_eq!(board.rules[0].id, created.id);
    assert_eq!(board.rules[0].kind, "category_total");
    assert_eq!(board.rules[0].phase, "active");
    assert_eq!(board.rules[0].actual_label, "12.0h");
    assert_eq!(board.rules[0].target_label, "≥4h / 7d");
    assert_eq!(board.rules[0].owed, "on track");
    match &board.rules[0].evidence {
        api_core::rules::Evidence::CategoryTotal { points } => {
            assert_eq!(points.len(), 14, "one point per strip day");
            assert_eq!(points.last().expect("today").date, AS_OF);
            assert_eq!(points.last().expect("today").hours, Some(12.0));
            // 2026-10-04 is outside the 7-day window ending 2026-10-18.
            assert_eq!(points[0].hours, Some(0.0));
        }
        other => panic!("expected category_total evidence, got {other:?}"),
    }
    assert_eq!(board.rules[0].strip.len(), 14);
    assert_eq!(
        board.rules[0].strip.last().expect("today").state,
        "pending",
        "today never gets a verdict"
    );

    // A per-rule pause round-trips through `pause_periods` and greys the strip.
    pollster::block_on(api_core::pause_rule(
        &rules,
        "u-1",
        &created.id,
        &api_core::PauseRuleInput {
            from: Some("2026-10-16".to_string()),
            to: Some("2026-10-16".to_string()),
        },
        AS_OF,
        NOW_RFC,
    ))
    .expect("schedules a pause");
    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    let rule = &board.rules[0];
    assert_eq!(
        rule.strip
            .iter()
            .find(|cell| cell.date == "2026-10-16")
            .expect("cell")
            .state,
        "paused"
    );
    // 8h left in the window, still over the 4h target.
    assert_eq!(rule.actual_label, "8.0h");
    assert!(rule.on_track);
    assert_eq!(rule.streak, 2, "the paused day neither breaks nor extends the streak");

    // A params edit forks a revision, and the older window keeps the old params.
    pollster::block_on(api_core::update_rule(
        &rules,
        "u-1",
        &created.id,
        &api_core::UpdateRuleInput {
            params: Some(params(
                r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                    "window_days":7,"comparator":"gte","target_hours":20.0}"#,
            )),
            ..api_core::UpdateRuleInput::default()
        },
        AS_OF,
        NOW_RFC,
    ))
    .expect("updates");
    let revisions = pollster::block_on(rules.list_revisions("u-1")).expect("revisions");
    let revisions: Vec<_> = revisions
        .into_iter()
        .filter(|revision| revision.rule_id == created.id)
        .collect();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].effective_from, "2026-09-01");
    assert_eq!(revisions[1].effective_from, AS_OF);
    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(board.rules[0].target_label, "≥20h / 7d");
    assert!(!board.rules[0].on_track);
    assert_eq!(board.on_track, 0);

    // Archive is a status, not a delete: the rule leaves the board.
    pollster::block_on(api_core::set_rule_archived(
        &rules, "u-1", &created.id, true, AS_OF, NOW_RFC,
    ))
    .expect("archives");
    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert!(board.rules.is_empty());
    assert_eq!(board.active_rules, 0);
    let archive =
        pollster::block_on(api_core::archive_rules(&rules, &h.digest(), "u-1", NOW_UNIX))
            .expect("archive");
    assert_eq!(archive.rules.len(), 1);
    assert_eq!(archive.rules[0].phase, "archived");
}

#[test]
fn d1_rules_seed_the_eight_defaults_when_the_user_has_none() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        for (slug, title) in [
            ("fitness", "Fitness"),
            ("learning", "Learning"),
            ("trip", "Trip"),
            ("travel", "Travel"),
        ] {
            seed_category(&conn, "u-1", &format!("cat-{slug}"), slug, title, false)
                .expect("category");
        }
        // The seeds start 120 days before as_of and ask for 60-day windows.
        seed_meta_range(&conn, "2026-04-20", AS_OF, "UTC");
    }

    let board =
        pollster::block_on(scoreboard(&h.rules(), &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(board.rules.len(), 8);
    assert_eq!(board.active_rules, 8);
    assert_eq!(board.paused_rules, 0);
    assert!(board.warnings.is_empty(), "{:?}", board.warnings);
    assert_eq!(board.rules[0].id, "seed:u-1:fitness");
    assert_eq!(board.rules[0].name, "Fitness");
    assert_eq!(board.rules[0].starts_on, "2026-06-20", "120 days before as_of");
    assert_eq!(board.rules[0].condition, "≥12 days of ≥1h / 60d");
    assert_eq!(board.rules[0].owed, "need 12 more day(s) with ≥1h fitness");
    assert_eq!(board.rules[1].condition, "≥7h weekday mean / 60d");
    assert_eq!(board.rules[2].condition, "≥4h / 7d");
    assert_eq!(board.rules[3].condition, "median ≥6.5h / 60d");
    assert_eq!(board.rules[4].condition, "median bedtime ≤22:30");
    assert_eq!(board.rules[5].condition, "bedtime spread ≤45m");
    assert_eq!(board.rules[6].condition, "median wake ≤05:00");
    assert_eq!(board.rules[7].condition, "wake spread ≤60m");
    // Nothing is logged yet, so every sleep rule asks for the markers.
    assert_eq!(board.rules[3].owed, api_core::rules::NOT_ENOUGH_NIGHTS);

    // A second read does not seed again.
    let again =
        pollster::block_on(scoreboard(&h.rules(), &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(again.rules.len(), 8);
    assert_eq!(
        pollster::block_on(h.rules().count_rules("u-1")).expect("count"),
        8
    );
}

/// The seed race guard (B1): the deterministic id plus `INSERT OR IGNORE`
/// leaves one row when the same seed is written twice.
#[test]
fn d1_a_duplicate_seed_id_writes_one_row() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
    }

    let rules = h.rules();
    let row = api_core::NewRuleRow {
        id: Some("seed:u-1:fitness".to_string()),
        user_id: "u-1".to_string(),
        name: "Fitness".to_string(),
        icon: "fitness".to_string(),
        kind: "category_days".to_string(),
        status: "active".to_string(),
        starts_on: "2026-06-20".to_string(),
        ends_on: None,
        paused_on: None,
        sort_order: 0,
    };
    pollster::block_on(rules.insert_rule(&row, NOW_RFC)).expect("inserts");
    pollster::block_on(rules.insert_rule(&row, NOW_RFC)).expect("ignores the duplicate");
    assert_eq!(
        pollster::block_on(rules.count_rules("u-1")).expect("count"),
        1,
        "the second insert is ignored, not a second row"
    );
}

/// m9: a revision can only be written under a rule the user owns.
#[test]
fn d1_a_revision_cannot_be_written_under_a_foreign_rule() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        // A rule that belongs to somebody else.
        conn.execute(
            "INSERT INTO users (id, google_id, email, name, created_at, updated_at)
             VALUES ('u-2', 'g-u-2', 'other@example.com', 'Other', ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("other user");
        conn.execute(
            "INSERT INTO rules (id, user_id, name, icon, kind, status, starts_on, sort_order, created_at, updated_at)
             VALUES ('foreign-rule', 'u-2', 'Theirs', '', 'category_total', 'active', '2026-09-01', 0, ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("foreign rule");
    }

    let rules = h.rules();
    pollster::block_on(rules.insert_revision(
        "u-1",
        "foreign-rule",
        None,
        "2026-09-01",
        r#"{"window_days":7,"comparator":"gte","target_hours":4.0,"categories":"all_productive"}"#,
        NOW_RFC,
    ))
    .expect("the statement runs");
    assert!(
        pollster::block_on(rules.list_revisions("u-1"))
            .expect("revisions")
            .is_empty(),
        "a foreign rule id writes nothing"
    );
}

/// m10: before a primary calendar exists, "today" comes from the newest
/// persisted digest day rather than jumping to UTC.
#[test]
fn d1_as_of_falls_back_to_the_latest_daily_meta_zone() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        // No primary calendar (the calendar list never synced)…
        conn.execute("UPDATE google_calendars SET is_primary = 0", [])
            .expect("unprimary");
        // …but the digest knows the user's zone.
        seed_meta(&conn, "u-1", "2026-10-17", "Asia/Kolkata", 0, 0).expect("meta");
    }

    // 2026-10-18T20:00:00Z is 01:30 on the 19th in Kolkata, and the 18th in UTC.
    let now_unix = NOW_UNIX + 14 * 3600;
    let board = pollster::block_on(scoreboard(&h.rules(), &h.digest(), "u-1", now_unix))
        .expect("board");
    assert_eq!(board.time_zone, "Asia/Kolkata");
    assert_eq!(board.as_of, "2026-10-19");

    // With no persisted day either, UTC is the last resort.
    {
        let conn = h.db.lock().expect("db");
        conn.execute("DELETE FROM daily_meta", []).expect("clear meta");
    }
    let board = pollster::block_on(scoreboard(&h.rules(), &h.digest(), "u-1", now_unix))
        .expect("board");
    assert_eq!(board.time_zone, "UTC");
    assert_eq!(board.as_of, AS_OF);
}

/// m3: one corrupt `rules` row must not take the whole board down.
#[test]
fn d1_a_corrupt_rule_row_is_skipped_not_fatal() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        seed_category(&conn, "u-1", "cat-learning", "learning", "Learning", false).expect("category");
        seed_meta_range(&conn, "2026-08-01", AS_OF, "UTC");
    }

    let rules = h.rules();
    let created = pollster::block_on(create_rule(&rules, "u-1", &learning_rule(), AS_OF, NOW_RFC))
        .expect("creates");
    {
        let conn = h.db.lock().expect("db");
        // An unknown kind…
        conn.execute(
            "INSERT INTO rules (id, user_id, name, icon, kind, status, starts_on, sort_order, created_at, updated_at)
             VALUES ('bad-kind', 'u-1', 'Broken', '', 'nonsense', 'active', '2026-09-01', 1, ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("bad kind row");
        // …and a real kind with unreadable params.
        conn.execute(
            "INSERT INTO rules (id, user_id, name, icon, kind, status, starts_on, sort_order, created_at, updated_at)
             VALUES ('bad-params', 'u-1', 'Broken params', '', 'category_total', 'active', '2026-09-01', 2, ?1, ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("bad params row");
        conn.execute(
            "INSERT INTO rule_revisions (id, rule_id, effective_from, params_json, created_at)
             VALUES ('bad-rev', 'bad-params', '2026-09-01', '{not json', ?1)",
            rusqlite::params![NOW_RFC],
        )
        .expect("bad revision row");
    }

    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(board.rules.len(), 1, "the readable rule still scores");
    assert_eq!(board.rules[0].id, created.id);
    assert_eq!(board.warnings.len(), 2, "{:?}", board.warnings);
    assert!(
        board.warnings.iter().any(|warning| warning.contains("bad-kind")),
        "{:?}",
        board.warnings
    );
    assert!(
        board.warnings.iter().any(|warning| warning.contains("bad-params")),
        "{:?}",
        board.warnings
    );
    // The notes are for the operator's log, never for the client.
    let value = serde_json::to_value(&board).expect("serializes");
    assert!(value.get("warnings").is_none());
}

/// M4: a paused challenge whose end has passed is finished, so the archive
/// carries it and the board does not.
#[test]
fn d1_a_paused_challenge_past_its_end_is_archived() {
    let h = open_harness().expect("harness");
    {
        let conn = h.db.lock().expect("db");
        seed_user_token_calendar(&conn, SeedOpts::default()).expect("seed user");
        seed_category(&conn, "u-1", "cat-learning", "learning", "Learning", false).expect("category");
        seed_meta_range(&conn, "2026-08-01", AS_OF, "UTC");
    }

    let rules = h.rules();
    let created = pollster::block_on(create_rule(
        &rules,
        "u-1",
        &learning_rule(),
        AS_OF,
        NOW_RFC,
    ))
    .expect("creates");
    // Pause it open-ended, then move its end behind today.
    pollster::block_on(api_core::pause_rule(
        &rules,
        "u-1",
        &created.id,
        &api_core::PauseRuleInput::default(),
        AS_OF,
        NOW_RFC,
    ))
    .expect("pauses");
    pollster::block_on(api_core::update_rule(
        &rules,
        "u-1",
        &created.id,
        &api_core::UpdateRuleInput {
            ends_on: Some(Some("2026-10-10".to_string())),
            ..api_core::UpdateRuleInput::default()
        },
        AS_OF,
        NOW_RFC,
    ))
    .expect("ends the challenge");

    let board = pollster::block_on(scoreboard(&rules, &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert!(board.rules.is_empty(), "a finished challenge leaves the board");
    assert_eq!(board.active_rules, 0);
    assert_eq!(board.paused_rules, 0);

    let archive =
        pollster::block_on(api_core::archive_rules(&rules, &h.digest(), "u-1", NOW_UNIX))
            .expect("archive");
    assert_eq!(archive.rules.len(), 1);
    assert_eq!(archive.rules[0].phase, "finished");
    assert_eq!(archive.rules[0].ends_on.as_deref(), Some("2026-10-10"));
}

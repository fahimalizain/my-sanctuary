//! Rules against the real Worker migrations (local SQLite = D1 schema).
//!
//! Applies `apps/worker/migrations/*.sql`, seeds a user, a category, and a few
//! days of `daily_category_minutes`, then drives the api-core rules service
//! through [`SqliteRuleRepo`] and checks the scoreboard the Worker would
//! return. The pure scorer has its own (DB-free) tests in `api-core`.

use api_core::rules::repo::RuleRepo;
use api_core::{create_rule, scoreboard, NewRuleInput, RuleKind};
use d1_sync::{open_harness, seed_category, seed_category_seconds, seed_user_token_calendar, SeedOpts};

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
    let revisions = pollster::block_on(rules.list_revisions(&created.id)).expect("revisions");
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
    }

    let board =
        pollster::block_on(scoreboard(&h.rules(), &h.digest(), "u-1", NOW_UNIX)).expect("board");
    assert_eq!(board.rules.len(), 8);
    assert_eq!(board.active_rules, 8);
    assert_eq!(board.rules[0].name, "Fitness");
    assert_eq!(board.rules[0].starts_on, "2026-06-20", "120 days before as_of");
    assert_eq!(board.rules[0].condition, "≥12 days of ≥1h / 60d");
    assert_eq!(board.rules[0].owed, "need 12 more day(s) with ≥1h Fitness");
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

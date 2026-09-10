use super::support::*;
use crate::calendar::{
    decide_webhook, persist_webhook_decision, tokens_match, WebhookDecision, WebhookPersistResult,
};
use crate::models::{GoogleCalendar, WatchChannel};
use crate::time::unix_secs_to_rfc3339;

#[test]
fn unknown_channel_is_ignored() {
    assert_eq!(
        decide_webhook(
            "exists",
            None,
            Some("tok-1"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn token_mismatch_is_ignored() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "exists",
            Some(&channel),
            Some("tok-2"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn token_length_mismatch_is_ignored() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "exists",
            Some(&channel),
            Some("short"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn missing_token_header_is_ignored() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "exists",
            Some(&channel),
            None,
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn missing_calendar_is_ignored() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook("exists", Some(&channel), Some("tok-1"), None),
        WebhookDecision::Ignore
    );
}

#[test]
fn sync_disabled_calendar_is_ignored() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "exists",
            Some(&channel),
            Some("tok-1"),
            Some(&calendar("cal-1", "primary@example.com", false)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn sync_handshake_state_is_ignored() {
    // Channel + calendar verify, but the `sync` handshake must not sync.
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "sync",
            Some(&channel),
            Some("tok-1"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::Ignore
    );
}

#[test]
fn unknown_state_is_ignored() {
    let channel = webhook_channel("cal-1");
    let calendar = calendar("cal-1", "primary@example.com", true);
    for state in ["", "deleted", "EXISTS", "exists2"] {
        assert_eq!(
            decide_webhook(state, Some(&channel), Some("tok-1"), Some(&calendar)),
            WebhookDecision::Ignore,
            "state {state:?} must not sync"
        );
    }
}

#[test]
fn exists_with_extra_whitespace_is_ignored() {
    // Google sends bare values; a padded `exists` is not one of them.
    let channel = webhook_channel("cal-1");
    let calendar = calendar("cal-1", "primary@example.com", true);
    for state in [" exists", "exists ", " exists ", "\texists"] {
        assert_eq!(
            decide_webhook(state, Some(&channel), Some("tok-1"), Some(&calendar)),
            WebhookDecision::Ignore,
            "state {state:?} must not sync"
        );
    }
}

#[test]
fn exists_state_enqueues_dirty_for_the_channel_calendar() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "exists",
            Some(&channel),
            Some("tok-1"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::EnqueueDirty {
            calendar_id: "cal-1".to_string(),
        }
    );
}

#[test]
fn not_exists_state_marks_calendar_gone() {
    let channel = webhook_channel("cal-1");
    assert_eq!(
        decide_webhook(
            "not_exists",
            Some(&channel),
            Some("tok-1"),
            Some(&calendar("cal-1", "primary@example.com", true)),
        ),
        WebhookDecision::CalendarGone {
            calendar_id: "cal-1".to_string(),
        }
    );
}

#[test]
fn exists_persist_bumps_dirty_and_returns_dirty_accepted() {
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let channel = webhook_channel("cal-1");
    let decision = decide_webhook(
        "exists",
        Some(&channel),
        Some("tok-1"),
        Some(&calendars.stored.lock().unwrap()[0].clone()),
    );
    assert_eq!(
        decision,
        WebhookDecision::EnqueueDirty {
            calendar_id: "cal-1".to_string(),
        }
    );

    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let result = pollster::block_on(persist_webhook_decision(
        &calendars,
        &decision,
        &now,
    ));
    assert_eq!(
        result,
        WebhookPersistResult::DirtyAccepted {
            calendar_id: "cal-1".to_string(),
        }
    );
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].dirty_requested_generation, 1);
    assert!(stored[0].sync_enabled);
}

#[test]
fn missing_dirty_write_is_not_accepted_work() {
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    *calendars.fail_bump_dirty.lock().unwrap() = true;
    let decision = WebhookDecision::EnqueueDirty {
        calendar_id: "cal-1".to_string(),
    };
    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let result = pollster::block_on(persist_webhook_decision(
        &calendars,
        &decision,
        &now,
    ));
    assert_eq!(
        result,
        WebhookPersistResult::DirtyPersistFailed {
            calendar_id: "cal-1".to_string(),
        }
    );
    assert!(!matches!(result, WebhookPersistResult::DirtyAccepted { .. }));
    assert_eq!(
        calendars.stored.lock().unwrap()[0].dirty_requested_generation,
        0
    );
}

#[test]
fn non_actionable_webhook_states_do_not_bump_dirty() {
    let channel = webhook_channel("cal-1");
    let enabled = calendar("cal-1", "primary@example.com", true);
    let disabled = calendar("cal-1", "primary@example.com", false);
    let now = unix_secs_to_rfc3339(NOW_UNIX);

    let cases: Vec<(&str, Option<&WatchChannel>, Option<&str>, Option<&GoogleCalendar>)> = vec![
        ("sync", Some(&channel), Some("tok-1"), Some(&enabled)),
        ("exists", Some(&channel), Some("tok-2"), Some(&enabled)),
        ("exists", None, Some("tok-1"), Some(&enabled)),
        ("exists", Some(&channel), Some("tok-1"), Some(&disabled)),
    ];

    for (state, stored_ch, token, cal) in cases {
        let calendars = FakeCalendarRepo::with(vec![enabled.clone()]);
        let decision = decide_webhook(state, stored_ch, token, cal);
        assert_eq!(
            decision,
            WebhookDecision::Ignore,
            "state={state:?} token={token:?} should Ignore"
        );
        let result = pollster::block_on(persist_webhook_decision(
            &calendars,
            &decision,
            &now,
        ));
        assert_eq!(result, WebhookPersistResult::Ignored);
        assert_eq!(
            calendars.stored.lock().unwrap()[0].dirty_requested_generation,
            0,
            "state={state:?} must not bump dirty"
        );
    }
}

#[test]
fn not_exists_persist_disables_without_dirty_bump() {
    let cal = calendar("cal-1", "primary@example.com", true);
    let calendars = FakeCalendarRepo::with(vec![cal]);
    let channel = webhook_channel("cal-1");
    let decision = decide_webhook(
        "not_exists",
        Some(&channel),
        Some("tok-1"),
        Some(&calendars.stored.lock().unwrap()[0].clone()),
    );
    assert_eq!(
        decision,
        WebhookDecision::CalendarGone {
            calendar_id: "cal-1".to_string(),
        }
    );

    let now = unix_secs_to_rfc3339(NOW_UNIX);
    let result = pollster::block_on(persist_webhook_decision(
        &calendars,
        &decision,
        &now,
    ));
    assert_eq!(
        result,
        WebhookPersistResult::GoneDisabled {
            calendar_id: "cal-1".to_string(),
        }
    );
    let stored = calendars.stored.lock().unwrap();
    assert!(!stored[0].sync_enabled);
    assert_eq!(stored[0].dirty_requested_generation, 0);
}

#[test]
fn tokens_match_compares_whole_strings() {
    let stored = "0123456789abcdef0123456789abcdef";
    assert!(tokens_match(stored, "0123456789abcdef0123456789abcdef"));
    assert!(
        !tokens_match(stored, "1123456789abcdef0123456789abcdef"),
        "first byte differs"
    );
    assert!(
        !tokens_match(stored, "0123456789abcdef0123456789abcde0"),
        "last byte differs"
    );
}

#[test]
fn tokens_match_rejects_different_lengths() {
    assert!(!tokens_match("abcdef", "abc"));
    assert!(!tokens_match("", "x"));
    assert!(tokens_match("", ""));
}

#[test]
fn tokens_match_handles_real_64_hex_tokens() {
    let stored = "a".repeat(64);
    let same = "a".repeat(64);
    let different = format!("{}b", "a".repeat(63));
    assert!(tokens_match(&stored, &same));
    assert!(!tokens_match(&stored, &different));
    assert!(!tokens_match(&stored, &"a".repeat(63)));
}

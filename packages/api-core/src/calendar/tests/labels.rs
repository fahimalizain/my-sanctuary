use super::support::*;
use crate::calendar::labels::{
    ensure_event_labels, event_labels_cache_is_fresh, EVENT_LABELS_TTL_SECS,
};
use crate::calendar::CalendarError;
use crate::time::unix_secs_to_rfc3339;

fn now_rfc3339() -> String {
    unix_secs_to_rfc3339(NOW_UNIX)
}

#[test]
fn empty_cache_fetches_and_stamps() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = String::new();
    cal.event_labels_updated_at = None;

    let body = r##"{"id":"primary@example.com","labelProperties":{"eventLabels":[
        {"id":"1","backgroundColor":"#AC725E"},
        {"id":"2","backgroundColor":"#d06b64"}
    ]}}"##;
    let http = FakeHttp::new(vec![("/calendars/", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap();

    let gets = http.gets.lock().unwrap();
    assert_eq!(gets.len(), 1, "{gets:?}");
    assert!(
        gets[0].contains("/calendars/primary%40example.com") && !gets[0].contains("/events"),
        "bare calendars.get: {gets:?}"
    );

    let expected = r##"[{"id":"1","backgroundColor":"#ac725e"},{"id":"2","backgroundColor":"#d06b64"}]"##;
    let updates = calendars.label_updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, "cal-1");
    assert_eq!(updates[0].1, expected);

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].event_labels, expected);
    assert_eq!(stored[0].event_labels_updated_at.as_deref(), Some(now.as_str()));
}

#[test]
fn fresh_non_empty_cache_skips() {
    let cal = calendar("cal-1", "primary@example.com", true);
    let http = FakeHttp::new(vec![]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap();

    assert!(http.gets.lock().unwrap().is_empty());
    assert!(calendars.label_updates.lock().unwrap().is_empty());
}

#[test]
fn stale_non_empty_cache_refreshes() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = r##"[{"id":"old","backgroundColor":"#616161"}]"##.to_string();
    cal.event_labels_updated_at = Some("2020-01-01T00:00:00Z".to_string());

    let body = r##"{"labelProperties":{"eventLabels":[
        {"id":"new","backgroundColor":"#ac725e"}
    ]}}"##;
    let http = FakeHttp::new(vec![("/calendars/", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap();

    assert_eq!(http.gets.lock().unwrap().len(), 1);
    let expected = r##"[{"id":"new","backgroundColor":"#ac725e"}]"##;
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].event_labels, expected);
    assert_eq!(stored[0].event_labels_updated_at.as_deref(), Some(now.as_str()));
}

#[test]
fn stale_empty_array_cache_refreshes() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = "[]".to_string();
    cal.event_labels_updated_at = Some("2020-01-01T00:00:00Z".to_string());

    let body = r##"{"labelProperties":{"eventLabels":[
        {"id":"1","backgroundColor":"#ac725e"}
    ]}}"##;
    let http = FakeHttp::new(vec![("/calendars/", 200, body)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap();

    assert_eq!(http.gets.lock().unwrap().len(), 1);
    let expected = r##"[{"id":"1","backgroundColor":"#ac725e"}]"##;
    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].event_labels, expected);
    assert_eq!(stored[0].event_labels_updated_at.as_deref(), Some(now.as_str()));
}

#[test]
fn failed_fetch_does_not_overwrite_stale_cache() {
    let old_json = r##"[{"id":"old","backgroundColor":"#616161"}]"##;
    let old_stamp = "2020-01-01T00:00:00Z";
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = old_json.to_string();
    cal.event_labels_updated_at = Some(old_stamp.to_string());

    let http = FakeHttp::new(vec![(
        "/calendars/",
        500,
        r#"{"error":"secret-body","access_token":"tok-leak"}"#,
    )]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    let err = pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap_err();
    assert!(
        matches!(&err, CalendarError::GoogleApi(m) if m.contains("500") && m.contains("primary@example.com")),
        "{err:?}"
    );
    let msg = format!("{err}");
    assert!(msg.contains("500"), "{msg}");
    assert!(msg.contains("primary@example.com"), "{msg}");
    assert!(!msg.contains("secret-body"), "{msg}");
    assert!(!msg.contains("tok-leak"), "{msg}");
    assert!(!msg.contains("access_token"), "{msg}");
    assert!(!msg.contains("at-1"), "{msg}");

    let stored = calendars.stored.lock().unwrap();
    assert_eq!(stored[0].event_labels, old_json);
    assert_eq!(stored[0].event_labels_updated_at.as_deref(), Some(old_stamp));
    assert!(calendars.label_updates.lock().unwrap().is_empty());
}

#[test]
fn failed_fetch_on_empty_cache_stays_empty() {
    let mut cal = calendar("cal-1", "primary@example.com", true);
    cal.event_labels = String::new();
    cal.event_labels_updated_at = None;

    let http = FakeHttp::new(vec![("/calendars/", 500, r#"{"error":"secret-body"}"#)]);
    let calendars = FakeCalendarRepo::with(vec![cal.clone()]);
    let now = now_rfc3339();

    let err = pollster::block_on(ensure_event_labels(
        &http, &calendars, &access(), &cal, &now,
    ))
    .unwrap_err();
    assert!(matches!(&err, CalendarError::GoogleApi(_)), "{err:?}");
    assert!(!format!("{err}").contains("secret-body"));

    let stored = calendars.stored.lock().unwrap();
    assert!(stored[0].event_labels.is_empty());
    assert_eq!(stored[0].event_labels_updated_at, None);
    assert!(calendars.label_updates.lock().unwrap().is_empty());
}

#[test]
fn freshness_helper_table() {
    let now = now_rfc3339();
    let now_unix = NOW_UNIX;
    let just_under = unix_secs_to_rfc3339(now_unix - (EVENT_LABELS_TTL_SECS - 1));
    let exactly_ttl = unix_secs_to_rfc3339(now_unix - EVENT_LABELS_TTL_SECS);
    let over_ttl = unix_secs_to_rfc3339(now_unix - (EVENT_LABELS_TTL_SECS + 1));
    let future = unix_secs_to_rfc3339(now_unix + 3600);
    let age_zero = now.clone();

    let cases: &[(&str, Option<&str>, &str, bool)] = &[
        ("[]", None, &now, false),
        ("[]", Some("not-a-date"), &now, false),
        ("[]", Some(&future), &now, true),
        ("[]", Some(&age_zero), &now, true),
        ("[]", Some(&just_under), &now, true),
        ("[]", Some(&exactly_ttl), &now, false),
        ("[]", Some(&over_ttl), &now, false),
        ("", Some(&age_zero), &now, false),
        (
            r##"[{"id":"1","backgroundColor":"#ac725e"}]"##,
            Some(&age_zero),
            &now,
            true,
        ),
        (
            r##"[{"id":"1","backgroundColor":"#ac725e"}]"##,
            None,
            &now,
            false,
        ),
        ("[]", Some(&age_zero), "not-a-date", false),
    ];

    for (labels, stamp, now_s, expected) in cases {
        assert_eq!(
            event_labels_cache_is_fresh(labels, *stamp, now_s),
            *expected,
            "labels={labels:?} stamp={stamp:?} now={now_s:?}"
        );
    }
}

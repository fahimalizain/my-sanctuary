//! Pure helpers shared by the tool slices: turning an upstream REST response
//! into a [`ToolExecution`] and validating tool arguments before a
//! sub-request.
//!
//! No sub-requests here — those are the Worker's allowlisted in-process calls
//! (ADR 0009 §4).

use crate::time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};

use super::protocol::ToolExecution;

/// The exact text a Google 401 becomes. A hyphen, not an em dash; the upstream
/// body is never copied in. It tells a human what to do, and exists so a client
/// does not start an OAuth flow it cannot complete (ADR 0009 §3).
pub const GOOGLE_ACCESS_EXPIRED: &str = "Google access expired - sign in to the web app";

/// Longest `list_events` window: 31 days, inclusive.
pub const MAX_EVENT_SPAN_SECS: i64 = 31 * 24 * 60 * 60;

/// Longest upstream `error` string kept; longer ones are truncated.
const MAX_UPSTREAM_ERROR_CHARS: usize = 200;

/// Maps an upstream REST response onto a tool outcome.
///
/// - `401` is always [`GOOGLE_ACCESS_EXPIRED`], whatever the body says.
/// - A 2xx with a whitespace-only body is `Ok(Null)`; a 2xx with non-JSON is a
///   failure (a tool result may never be invalid JSON).
/// - Any other status prefers the body's string `error` field and otherwise
///   reports the status only — the raw body is never copied in.
pub fn tool_outcome_from_upstream(status: u16, body: &str) -> ToolExecution {
    match status {
        401 => ToolExecution::Failed(GOOGLE_ACCESS_EXPIRED.to_string()),
        200..=299 => {
            if body.trim().is_empty() {
                return ToolExecution::Ok(serde_json::Value::Null);
            }
            match serde_json::from_str(body) {
                Ok(value) => ToolExecution::Ok(value),
                Err(_) => ToolExecution::Failed("upstream returned non-json".to_string()),
            }
        }
        _ => match serde_json::from_str::<serde_json::Value>(body) {
            Ok(serde_json::Value::Object(object)) => match object.get("error") {
                Some(serde_json::Value::String(message)) => {
                    ToolExecution::Failed(truncate_chars(message, MAX_UPSTREAM_ERROR_CHARS))
                }
                _ => ToolExecution::Failed(format!("upstream returned {status}")),
            },
            _ => ToolExecution::Failed(format!("upstream returned {status}")),
        },
    }
}

/// Validates a `list_events` window before any sub-request is made.
///
/// Returns the two instants normalized to RFC 3339 UTC (so `+05:30` input
/// becomes `…Z`), ready to put on the query string. The span must be at most
/// [`MAX_EVENT_SPAN_SECS`]; exactly 31 days is allowed.
pub fn validate_event_window(time_min: &str, time_max: &str) -> Result<(String, String), String> {
    let Some(start) = rfc3339_to_unix_secs(time_min) else {
        return Err("invalid time_min: must be RFC 3339".to_string());
    };
    let Some(end) = rfc3339_to_unix_secs(time_max) else {
        return Err("invalid time_max: must be RFC 3339".to_string());
    };
    if end <= start {
        return Err("time_max must be after time_min".to_string());
    }
    if end - start > MAX_EVENT_SPAN_SECS {
        return Err("time window must be at most 31 days".to_string());
    }
    Ok((unix_secs_to_rfc3339(start), unix_secs_to_rfc3339(end)))
}

/// True iff `id` is 1..=64 bytes of ASCII alphanumerics, `_`, or `-`.
///
/// Path ids interpolated into a sub-request URL must pass this, so `..`,
/// `../keys`, `/`, and `.` can never change the route (ADR 0009 §4).
pub fn is_safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Whether a tool may issue this sub-request through the REST router.
///
/// An explicit table of exact method-and-path pairs, never a prefix rule: a
/// `POST /api/tasks` prefix would also reach `/api/tasks/:id/complete`, and a
/// `/api/calendar/calendars` rule would reach the repair endpoint (ADR 0009
/// §4). `:id` segments must pass [`is_safe_id`], so `..`, embedded slashes, and
/// an empty or trailing-slash id are refused. Anything not in the table —
/// `DELETE`, `/api/keys`, the timer verbs, focus, occurrences — is refused, and
/// so is any path carrying a query string.
pub fn allow_subrequest(method: &str, path: &str) -> bool {
    match (method, path) {
        ("GET", "/auth/me")
        | ("GET", "/api/tasks")
        | ("POST", "/api/tasks")
        | ("GET", "/api/tasks/classify")
        | ("GET", "/api/agenda")
        | ("GET", "/api/calendar/events")
        | ("POST", "/api/calendar/events")
        | ("GET", "/api/calendar/calendars")
        | ("GET", "/api/reports/status")
        | ("GET", "/api/rules/scoreboard") => true,
        ("PATCH", path) => id_between(path, "/api/tasks/", ""),
        ("POST", path) => {
            id_between(path, "/api/tasks/", "/move")
                || id_between(path, "/api/agenda/items/", "/move")
                || id_between(path, "/api/agenda/items/", "/reschedule")
        }
        _ => false,
    }
}

/// True iff `path` is exactly `{prefix}{id}{suffix}` and `id` passes
/// [`is_safe_id`]. With an empty `suffix` the id must reach the end of the
/// path, so a trailing slash fails.
fn id_between(path: &str, prefix: &str, suffix: &str) -> bool {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(is_safe_id)
}

/// Truncates on a character boundary — never splits a UTF-8 sequence.
fn truncate_chars(message: &str, max: usize) -> String {
    if message.chars().count() <= max {
        return message.to_string();
    }
    message.chars().take(max).collect()
}

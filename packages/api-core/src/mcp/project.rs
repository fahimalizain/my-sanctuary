//! Pure projectors for the read tools (ADR 0009 §8).
//!
//! A tool hands the agent a small JSON value, not the REST envelope. These
//! functions do that projection and nothing else: no I/O, no clock, no
//! services. The Worker fetches the envelope through the REST router and passes
//! the parsed `Value` in, so a cap or a dropped field is unit-tested natively,
//! without a Worker, a D1 binding, or a network.
//!
//! Failure is a plain `String`: the caller turns it into
//! `ToolExecution::Failed`, which the protocol renders as an `isError` tool
//! result at HTTP 200 — never a JSON-RPC error (ADR 0009 §3).

use serde_json::{json, Map, Value};

/// Most tasks `list_tasks` returns; the rest are counted in `omitted`.
pub const MAX_TASKS: usize = 100;

/// Most agenda items `get_agenda` returns.
pub const MAX_AGENDA_ITEMS: usize = 80;

/// Most backlog rows `get_agenda` returns.
pub const MAX_AGENDA_BACKLOG: usize = 40;

/// Most events `list_events` returns.
pub const MAX_EVENTS: usize = 100;

/// Most calendars `list_calendars` returns.
pub const MAX_CALENDARS: usize = 50;

/// Reads the `get_agenda` window out of the tool arguments.
///
/// `from` and `to` are optional but all-or-nothing, as `YYYY-MM-DD`. The shape
/// only is checked: whether the date exists is the REST handler's problem (it
/// answers 400 for February 31), and an impossible-looking month such as
/// `2026-13-01` is accepted here so the same 400 comes back.
pub fn agenda_bounds(arguments: &Value) -> Result<Option<(String, String)>, String> {
    let from = arguments.get("from").filter(|value| !value.is_null());
    let to = arguments.get("to").filter(|value| !value.is_null());
    match (from, to) {
        // Not set at all: the REST handler's default window.
        (None, None) => Ok(None),
        (Some(from), Some(to)) => Ok(Some((
            date_argument(from, "from")?,
            date_argument(to, "to")?,
        ))),
        // One alone: "set together or not at all".
        _ => Err("from and to must be set together".to_string()),
    }
}

/// One bound: a string of exactly the `YYYY-MM-DD` shape, never trimmed.
fn date_argument(value: &Value, name: &str) -> Result<String, String> {
    match value.as_str() {
        Some(text) if is_date_shape(text) => Ok(text.to_string()),
        _ => Err(format!("{name} must be YYYY-MM-DD")),
    }
}

/// `YYYY-MM-DD`, bytes only — no calendar validation.
pub(crate) fn is_date_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// `now`: the caller plus today's civil date and its zone. No agenda items —
/// the tool is a cheap "where am I", and `get_agenda` is the list.
///
/// A `null` user is `unauthorized`: `/auth/me` answers 200 with `{"user":null}`
/// when the sub-request carried no usable session, which a bearer key cannot
/// have.
pub fn project_now(me: &Value, agenda: &Value) -> Result<Value, String> {
    let Some(user) = me.get("user").and_then(Value::as_object) else {
        return Err("unauthorized".to_string());
    };
    let today = agenda
        .get("today")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let time_zone = agenda
        .get("time_zone")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if today.is_empty() || time_zone.is_empty() {
        return Err("agenda response missing today".to_string());
    }
    Ok(json!({
        "user": {
            "id": text(user, "id"),
            "email": text(user, "email"),
            "name": text(user, "name"),
            "picture": text(user, "picture"),
        },
        "time_zone": time_zone,
        "today": today,
    }))
}

/// `list_tasks`: the first [`MAX_TASKS`] slim tasks.
pub fn project_tasks(body: &Value) -> Result<Value, String> {
    let Some(tasks) = body.get("tasks").and_then(Value::as_array) else {
        return Err("unexpected tasks response".to_string());
    };
    let (kept, omitted) = capped(tasks, MAX_TASKS);
    let mut result = Map::new();
    result.insert(
        "tasks".to_string(),
        Value::Array(kept.iter().filter_map(slim_task).collect()),
    );
    mark_truncated(&mut result, "truncated", "omitted", omitted);
    Ok(Value::Object(result))
}

/// `get_agenda`: the window and the backlog, each capped independently.
///
/// `backlog` is absent or `null` on a history read; that is an empty backlog,
/// not a malformed envelope.
pub fn project_agenda(body: &Value) -> Result<Value, String> {
    const NO_ROWS: &[Value] = &[];

    let Some(items) = body.get("items").and_then(Value::as_array) else {
        return Err("unexpected agenda response".to_string());
    };
    let backlog: &[Value] = match body.get("backlog") {
        None | Some(Value::Null) => NO_ROWS,
        Some(Value::Array(rows)) => rows.as_slice(),
        Some(_) => return Err("unexpected agenda response".to_string()),
    };
    let (items, omitted_items) = capped(items, MAX_AGENDA_ITEMS);
    let (backlog, omitted_backlog) = capped(backlog, MAX_AGENDA_BACKLOG);

    let mut result = Map::new();
    result.insert(
        "items".to_string(),
        Value::Array(items.iter().filter_map(slim_agenda_item).collect()),
    );
    result.insert(
        "backlog".to_string(),
        Value::Array(backlog.iter().filter_map(slim_agenda_item).collect()),
    );
    result.insert("from".to_string(), Value::from(envelope_text(body, "from")));
    result.insert("to".to_string(), Value::from(envelope_text(body, "to")));
    result.insert(
        "today".to_string(),
        Value::from(envelope_text(body, "today")),
    );
    result.insert(
        "time_zone".to_string(),
        Value::from(envelope_text(body, "time_zone")),
    );
    mark_truncated(
        &mut result,
        "truncated_items",
        "omitted_items",
        omitted_items,
    );
    mark_truncated(
        &mut result,
        "truncated_backlog",
        "omitted_backlog",
        omitted_backlog,
    );
    Ok(Value::Object(result))
}

/// `list_events`: the first [`MAX_EVENTS`] slim events.
///
/// The REST envelope's `sync` block is replica health for the web app; an agent
/// gets no part of it, so the result has no `sync` key at all.
pub fn project_events(body: &Value) -> Result<Value, String> {
    let Some(events) = body.get("events").and_then(Value::as_array) else {
        return Err("unexpected events response".to_string());
    };
    let (kept, omitted) = capped(events, MAX_EVENTS);
    let mut result = Map::new();
    result.insert(
        "events".to_string(),
        Value::Array(kept.iter().filter_map(slim_event).collect()),
    );
    result.insert(
        "source".to_string(),
        Value::from(envelope_text(body, "source")),
    );
    mark_truncated(&mut result, "truncated", "omitted", omitted);
    Ok(Value::Object(result))
}

/// `list_calendars`: the first [`MAX_CALENDARS`] slim calendars.
///
/// The REST row already drops sync internals; this keeps only the picker
/// fields, so a later envelope addition cannot leak into a tool result.
pub fn project_calendars(body: &Value) -> Result<Value, String> {
    let Some(calendars) = body.get("calendars").and_then(Value::as_array) else {
        return Err("unexpected calendars response".to_string());
    };
    let (kept, omitted) = capped(calendars, MAX_CALENDARS);
    let mut result = Map::new();
    result.insert(
        "calendars".to_string(),
        Value::Array(kept.iter().filter_map(slim_calendar).collect()),
    );
    mark_truncated(&mut result, "truncated", "omitted", omitted);
    Ok(Value::Object(result))
}

/// A top-level envelope string field; a non-object body reads as `""`.
fn envelope_text(body: &Value, key: &str) -> String {
    body.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Splits `rows` at `max`: the kept prefix, and how many were dropped.
fn capped(rows: &[Value], max: usize) -> (&[Value], usize) {
    if rows.len() > max {
        (&rows[..max], rows.len() - max)
    } else {
        (rows, 0)
    }
}

/// `truncated` is always present; `omitted` only when rows were dropped.
fn mark_truncated(
    result: &mut Map<String, Value>,
    truncated_key: &str,
    omitted_key: &str,
    omitted: usize,
) {
    result.insert(truncated_key.to_string(), Value::Bool(omitted > 0));
    if omitted > 0 {
        result.insert(omitted_key.to_string(), Value::from(omitted));
    }
}

/// One slim task, or `None` when the row is not an object (a malformed row is
/// skipped, never a reason to fail the whole list).
pub(crate) fn slim_task(value: &Value) -> Option<Value> {
    let task = value.as_object()?;
    Some(json!({
        "id": text(task, "id"),
        "title": text(task, "title"),
        "display_title": text(task, "display_title"),
        "status": text(task, "status"),
        "duration_minutes": integer(task, "duration_minutes"),
        "sort_order": integer(task, "sort_order"),
        "priority": text(task, "priority"),
        "difficulty": text(task, "difficulty"),
        "focused": boolean(task, "focused"),
        "category": slim_category(task.get("category")),
    }))
}

/// One slim agenda row, or `None` when the row is not an object.
pub(crate) fn slim_agenda_item(value: &Value) -> Option<Value> {
    let item = value.as_object()?;
    Some(json!({
        "id": text(item, "id"),
        "local_date": text(item, "local_date"),
        "kind": text(item, "kind"),
        "ref_id": text(item, "ref_id"),
        "sort_order": integer(item, "sort_order"),
        "task": item.get("task").and_then(slim_task).unwrap_or(Value::Null),
        "occurrence": item
            .get("occurrence")
            .and_then(slim_occurrence)
            .unwrap_or(Value::Null),
    }))
}

/// One slim occurrence, or `None` when the field is not an object.
///
/// `rrule` (the recurrence blob), timestamps, and the routine's deleted flag
/// are display internals; an agent reads `resolved_title` and `status`.
fn slim_occurrence(value: &Value) -> Option<Value> {
    let occurrence = value.as_object()?;
    Some(json!({
        "id": text(occurrence, "id"),
        "routine_id": text(occurrence, "routine_id"),
        "resolved_title": text(occurrence, "resolved_title"),
        "status": text(occurrence, "status"),
        "estimated_minutes": integer(occurrence, "estimated_minutes"),
        "category": slim_category(occurrence.get("category")),
    }))
}

/// One slim event, or `None` when the row is not an object.
///
/// The REST event is flat — every `CalendarEvent` field plus the painted
/// `color` on the same object — not nested under an `event` key.
pub(crate) fn slim_event(value: &Value) -> Option<Value> {
    let event = value.as_object()?;
    Some(json!({
        "id": text(event, "id"),
        "calendar_id": text(event, "calendar_id"),
        "title": text(event, "title"),
        "start_time": text(event, "start_time"),
        "end_time": text(event, "end_time"),
        "start_time_zone": text(event, "start_time_zone"),
        "status": text(event, "status"),
        "is_all_day": boolean(event, "is_all_day"),
        "category_id": optional_text(event, "category_id"),
        "color": text(event, "color"),
    }))
}

/// One slim calendar, or `None` when the row is not an object.
fn slim_calendar(value: &Value) -> Option<Value> {
    let calendar = value.as_object()?;
    Some(json!({
        "id": text(calendar, "id"),
        "google_calendar_id": text(calendar, "google_calendar_id"),
        "summary": text(calendar, "summary"),
        "time_zone": text(calendar, "time_zone"),
        "access_role": text(calendar, "access_role"),
        "is_primary": boolean(calendar, "is_primary"),
        "sync_enabled": boolean(calendar, "sync_enabled"),
    }))
}

/// A category summary reduced to the three fields a list row needs.
fn slim_category(value: Option<&Value>) -> Value {
    let Some(category) = value.and_then(Value::as_object) else {
        return Value::Null;
    };
    json!({
        "id": text(category, "id"),
        "title": text(category, "title"),
        "color": text(category, "color"),
    })
}

/// A string field; absent or non-string is `""`.
fn text(object: &Map<String, Value>, key: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An integer field; absent, non-integral, or non-numeric is `0`.
fn integer(object: &Map<String, Value>, key: &str) -> i64 {
    object.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// A boolean field; absent or non-boolean is `false`.
fn boolean(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A nullable string field: absent, `null`, or `""` is JSON `null`.
fn optional_text(object: &Map<String, Value>, key: &str) -> Value {
    match object.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Value::from(value),
        _ => Value::Null,
    }
}

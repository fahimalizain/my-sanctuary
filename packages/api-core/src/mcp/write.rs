//! Pure argument builders and result projectors for the write tools (ADR 0009
//! §8).
//!
//! A builder turns `tools/call` arguments into the exact JSON body a REST route
//! accepts, and rejects anything the route would mistake for something else:
//! an unknown key, a `status` on a create, a `sort_order` on a task move. The
//! projectors then turn the route's envelope into the small value an agent
//! sees, reusing the read tools' slim rows so a write result and a list row
//! have one shape.
//!
//! Everything here is pure: no I/O, no clock, no services. The Worker issues
//! the sub-request (ADR 0009 §4) and hands the parsed body in. Failure is a
//! plain `String` the caller renders as `isError` at HTTP 200 (ADR 0009 §3).

use serde_json::{json, Map, Value};

use super::project::{is_date_shape, slim_agenda_item, slim_event, slim_task};
use super::upstream::is_safe_id;

/// `create_task` and `update_task` never touch status; a status change is a
/// move. The message names the tool that does take it.
const STATUS_MOVES_THROUGH: &str = "status changes go through move_task";

/// The only statuses `move_task` accepts, exactly as the service spells them.
const TASK_STATUSES: [&str; 5] = ["OPEN", "PLANNED", "IN_PROGRESS", "COMPLETED", "DISCARDED"];

/// Task fields a create may set beyond `title`.
const CREATE_TASK_KEYS: [&str; 5] = [
    "title",
    "description",
    "duration_minutes",
    "priority",
    "difficulty",
];

/// Task fields an update may set, plus the id the path needs.
const UPDATE_TASK_KEYS: [&str; 6] = [
    "id",
    "title",
    "description",
    "duration_minutes",
    "priority",
    "difficulty",
];

/// `create_event` accepts an event, not a task-timer or occurrence carrier.
/// These are rejected together so the message can name all four families.
const CREATE_EVENT_REJECTED: [&str; 7] = [
    "task_id",
    "routine_id",
    "occurrence_id",
    "color_hex",
    "sanctuary_focus",
    "priority",
    "difficulty",
];

/// `create_event` fields beyond the rejected ones.
const CREATE_EVENT_KEYS: [&str; 7] = [
    "summary",
    "start",
    "end",
    "description",
    "is_all_day",
    "start_time_zone",
    "calendar_id",
];

/// The `POST /api/tasks` body: `title`, plus only the optional fields that
/// were set.
///
/// A negative `duration_minutes` is passed through — the route answers its own
/// domain 400, and duplicating that rule here would let the two drift.
pub fn build_create_task(arguments: &Value) -> Result<Value, String> {
    // Special cases before the unknown-field scan: a known-but-wrong key gets a
    // teaching message, not "unknown field".
    if has_key(arguments, "status") {
        return Err(STATUS_MOVES_THROUGH.to_string());
    }
    reject_unknown(arguments, &CREATE_TASK_KEYS)?;

    let Some(title) = arguments.get("title").and_then(Value::as_str) else {
        return Err("title is required".to_string());
    };
    if title.is_empty() {
        return Err("title is required".to_string());
    }

    let mut body = Map::new();
    body.insert("title".to_string(), Value::from(title));
    insert_optional_string(&mut body, arguments, "description")?;
    insert_optional_integer(&mut body, arguments, "duration_minutes")?;
    insert_optional_string(&mut body, arguments, "priority")?;
    insert_optional_string(&mut body, arguments, "difficulty")?;
    Ok(Value::Object(body))
}

/// The `PATCH /api/tasks/:id` path id and partial body.
///
/// The body carries no `id`: the route reads it from the path, and a partial
/// update is the whole contract. At least one field must be set.
pub fn build_update_task(arguments: &Value) -> Result<(String, Value), String> {
    if has_key(arguments, "status") {
        return Err(STATUS_MOVES_THROUGH.to_string());
    }
    reject_unknown(arguments, &UPDATE_TASK_KEYS)?;

    let id = path_id(arguments)?;
    let mut body = Map::new();
    insert_optional_string(&mut body, arguments, "title")?;
    insert_optional_string(&mut body, arguments, "description")?;
    insert_optional_integer(&mut body, arguments, "duration_minutes")?;
    insert_optional_string(&mut body, arguments, "priority")?;
    insert_optional_string(&mut body, arguments, "difficulty")?;
    if body.is_empty() {
        return Err("nothing to update".to_string());
    }
    Ok((id, Value::Object(body)))
}

/// The `POST /api/tasks/:id/move` path id and `{"status": …}` body.
///
/// `sort_order` is deliberately absent — and deliberately not "unknown": the
/// server applies the column default, so a rank the agent sends would be
/// silently dropped. Saying so is more useful than a generic rejection.
pub fn build_move_task(arguments: &Value) -> Result<(String, Value), String> {
    if has_key(arguments, "sort_order") {
        return Err("move_task does not take sort_order".to_string());
    }
    reject_unknown(arguments, &["id", "status"])?;

    let id = path_id(arguments)?;
    let status = arguments
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| TASK_STATUSES.contains(status));
    let Some(status) = status else {
        return Err(
            "status must be OPEN, PLANNED, IN_PROGRESS, COMPLETED, or DISCARDED".to_string(),
        );
    };
    Ok((id, json!({ "status": status })))
}

/// The `POST /api/agenda/items/:id/move` path id and `{"sort_order": n}` body.
///
/// A missing, `null`, or non-integer rank is the same failure: this route has
/// no "append" default, so there is nothing to send without one.
pub fn build_move_agenda_item(arguments: &Value) -> Result<(String, Value), String> {
    reject_unknown(arguments, &["id", "sort_order"])?;

    let id = path_id(arguments)?;
    let sort_order = match arguments.get("sort_order") {
        Some(value) => match value.as_i64() {
            Some(rank) => rank,
            None => return Err("sort_order is required".to_string()),
        },
        None => return Err("sort_order is required".to_string()),
    };
    if sort_order < 0 {
        return Err("sort_order must be >= 0".to_string());
    }
    Ok((id, json!({ "sort_order": sort_order })))
}

/// The `POST /api/agenda/items/:id/reschedule` path id and body: a dated
/// reschedule, a backlog drop, or an unschedule.
///
/// A backlog drop ignores `sort_order` (the Open column is the Board's task
/// rank), so the body is exactly `{"backlog":true}`; sending a rank alongside
/// would look like it placed the row. An unschedule keeps the task `PLANNED`
/// and parks its row in the Planned warehouse, so the body is exactly
/// `{"unschedule":true}`; a rank is ignored there too. The date is
/// shape-checked only — whether `2026-02-31` exists is the route's 400 to
/// give, exactly like [`super::project::agenda_bounds`].
pub fn build_reschedule_agenda_item(arguments: &Value) -> Result<(String, Value), String> {
    reject_unknown(
        arguments,
        &["id", "date", "sort_order", "backlog", "unschedule"],
    )?;

    let id = path_id(arguments)?;
    let backlog = match arguments.get("backlog") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err("backlog must be a boolean".to_string()),
    };
    let unschedule = match arguments.get("unschedule") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err("unschedule must be a boolean".to_string()),
    };
    let sort_order = match arguments.get("sort_order") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_i64() {
            Some(rank) if rank >= 0 => Some(rank),
            Some(_) => return Err("sort_order must be >= 0".to_string()),
            None => return Err("sort_order must be an integer".to_string()),
        },
    };
    let date = arguments.get("date").filter(|value| !value.is_null());

    if backlog && unschedule {
        return Err("backlog and unschedule are mutually exclusive".to_string());
    }
    if backlog {
        if date.is_some() {
            return Err("date is omitted when backlog is true".to_string());
        }
        return Ok((id, json!({ "backlog": true })));
    }
    if unschedule {
        if date.is_some() {
            return Err("date is omitted when unschedule is true".to_string());
        }
        return Ok((id, json!({ "unschedule": true })));
    }

    let Some(date) = date else {
        return Err("date is required, or set backlog or unschedule true".to_string());
    };
    let date = match date.as_str() {
        Some(text) if is_date_shape(text) => text,
        _ => return Err("date must be YYYY-MM-DD".to_string()),
    };
    let mut body = Map::new();
    body.insert("date".to_string(), Value::from(date));
    if let Some(sort_order) = sort_order {
        body.insert("sort_order".to_string(), Value::from(sort_order));
    }
    Ok((id, Value::Object(body)))
}

/// Validates the whole `create_event` argument object and reports whether the
/// caller named a calendar.
///
/// `Ok(None)` means "resolve the primary calendar"; `Ok(Some(id))` is the safe
/// local calendar id the caller chose. Every field is checked here so a bad
/// argument fails before the calendar list sub-request, not after it.
pub fn create_event_calendar(arguments: &Value) -> Result<Option<String>, String> {
    if CREATE_EVENT_REJECTED
        .iter()
        .any(|key| has_key(arguments, key))
    {
        return Err(
            "create_event does not accept task, routine, occurrence, or color fields".to_string(),
        );
    }
    reject_unknown(arguments, &CREATE_EVENT_KEYS)?;

    let required = ["summary", "start", "end"];
    if required.iter().any(|key| {
        !matches!(arguments.get(*key).and_then(Value::as_str), Some(text) if !text.is_empty())
    }) {
        return Err("summary, start, and end are required".to_string());
    }

    optional_string(arguments, "description")?;
    optional_string(arguments, "start_time_zone")?;
    match arguments.get("is_all_day") {
        None | Some(Value::Null) | Some(Value::Bool(_)) => {}
        Some(_) => return Err("is_all_day must be a boolean".to_string()),
    }

    match arguments.get("calendar_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if is_safe_id(id) => Ok(Some(id.clone())),
        Some(_) => Err("invalid calendar_id".to_string()),
    }
}

/// The `POST /api/calendar/events` body.
///
/// `explicit` is the `calendar_id_explicit` flag: `false` when the id came from
/// the primary calendar, so title classification may still retarget the event;
/// `true` when the caller named the calendar. Only event fields are copied —
/// never a task, routine, occurrence, colour, or priority carrier.
pub fn create_event_body(
    arguments: &Value,
    calendar_id: &str,
    explicit: bool,
) -> Result<Value, String> {
    let mut body = Map::new();
    body.insert("calendar_id".to_string(), Value::from(calendar_id));
    body.insert("calendar_id_explicit".to_string(), Value::Bool(explicit));
    for key in ["summary", "start", "end"] {
        let value = arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default();
        body.insert(key.to_string(), Value::from(value));
    }
    insert_optional_string(&mut body, arguments, "description")?;
    insert_optional_string(&mut body, arguments, "start_time_zone")?;
    match arguments.get("is_all_day") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(flag)) => {
            body.insert("is_all_day".to_string(), Value::Bool(*flag));
        }
        // `create_event_calendar` already rejected this; nothing to add.
        Some(_) => {}
    }
    Ok(Value::Object(body))
}

/// The id of the primary calendar from a `GET /api/calendar/calendars` body.
///
/// The list is small and the primary row is first, so this scans for the flag
/// rather than going through the capped [`super::project::project_calendars`].
/// A row is skipped unless its id is a safe path segment: the id is later
/// interpolated into the create body and must stay routable.
pub fn primary_calendar_id(calendars_body: &Value) -> Result<String, String> {
    let rows = calendars_body
        .get("calendars")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for row in rows {
        let Some(calendar) = row.as_object() else {
            continue;
        };
        if calendar.get("is_primary").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        if let Some(id) = calendar.get("id").and_then(Value::as_str) {
            if is_safe_id(id) {
                return Ok(id.to_string());
            }
        }
    }
    Err("no primary calendar".to_string())
}

/// `create_task` and `update_task`: the created or updated row, slimmed.
pub fn project_task_result(body: &Value) -> Result<Value, String> {
    let Some(task) = body.get("task").and_then(slim_task) else {
        return Err("unexpected task response".to_string());
    };
    Ok(json!({ "task": task }))
}

/// `move_task`: the moved row, plus the Google event the transition touched.
///
/// A `null` or absent `event` stays `null` — the key is always present so a
/// client can branch on it without guessing.
pub fn project_move_result(body: &Value) -> Result<Value, String> {
    let Some(task) = body.get("task").and_then(slim_task) else {
        return Err("unexpected task response".to_string());
    };
    let event = body
        .get("event")
        .and_then(slim_event)
        .unwrap_or(Value::Null);
    Ok(json!({ "task": task, "event": event }))
}

/// `move_agenda_item` and `reschedule_agenda_item`: the moved row, slimmed.
pub fn project_agenda_item_result(body: &Value) -> Result<Value, String> {
    let Some(item) = body.get("item").and_then(slim_agenda_item) else {
        return Err("unexpected agenda response".to_string());
    };
    Ok(json!({ "item": item }))
}

/// `create_event`: the created row plus the source the route reported.
///
/// `sync` and `raw_json` are replica internals; an agent gets neither.
pub fn project_created_event(body: &Value) -> Result<Value, String> {
    let Some(event) = body.get("event").and_then(slim_event) else {
        return Err("unexpected event response".to_string());
    };
    let source = body
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(json!({ "event": event, "source": source }))
}

/// Whether the caller sent this key at all. A JSON `null` counts: presence is
/// what the special-case messages are about, not the value.
fn has_key(arguments: &Value, key: &str) -> bool {
    arguments.get(key).is_some()
}

/// Rejects every key outside `allowed` with `unknown field: {name}`.
///
/// A non-object `arguments` has no keys to reject; the protocol already refuses
/// one before a tool runs.
fn reject_unknown(arguments: &Value, allowed: &[&str]) -> Result<(), String> {
    let Some(object) = arguments.as_object() else {
        return Ok(());
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown field: {key}"));
        }
    }
    Ok(())
}

/// The path id: a string that passes [`is_safe_id`], so it can never change the
/// route it is interpolated into.
fn path_id(arguments: &Value) -> Result<String, String> {
    let Some(id) = arguments.get("id").and_then(Value::as_str) else {
        return Err("id is required".to_string());
    };
    if !is_safe_id(id) {
        return Err("invalid id".to_string());
    }
    Ok(id.to_string())
}

/// An optional string field: absent or `null` is `None`, anything else must be
/// a string. An empty string is a value, not an omission.
fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// An optional integer field: absent or `null` is `None`, anything else must be
/// a JSON integer (`1.5` is not). Sign is the route's business.
fn optional_integer(arguments: &Value, key: &str) -> Result<Option<i64>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_i64() {
            Some(number) => Ok(Some(number)),
            None => Err(format!("{key} must be an integer")),
        },
    }
}

/// Copies `key` into `body` only when the caller set it.
fn insert_optional_string(
    body: &mut Map<String, Value>,
    arguments: &Value,
    key: &str,
) -> Result<(), String> {
    if let Some(value) = optional_string(arguments, key)? {
        body.insert(key.to_string(), Value::from(value));
    }
    Ok(())
}

/// Copies `key` into `body` only when the caller set it, as an integer.
fn insert_optional_integer(
    body: &mut Map<String, Value>,
    arguments: &Value,
    key: &str,
) -> Result<(), String> {
    if let Some(value) = optional_integer(arguments, key)? {
        body.insert(key.to_string(), Value::from(value));
    }
    Ok(())
}

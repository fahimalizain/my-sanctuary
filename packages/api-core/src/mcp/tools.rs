//! The v1 MCP tool catalog (ADR 0009 §8).
//!
//! This is the only list of tools: `tools/list` serialises it and `tools/call`
//! consults [`tool_by_name`], so an unknown name is rejected from the same
//! table that is advertised. Argument validation beyond "`arguments` is a JSON
//! object" is deliberately *not* here — a known tool's bad argument is an
//! `isError` result from the executor, not JSON-RPC `-32602`.

use serde_json::{json, Value};

/// One entry in the v1 catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
}

impl Tool {
    /// The exact `tools/list` entry: name, description, `inputSchema`, and the
    /// three annotation hints.
    pub fn wire(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "annotations": {
                "readOnlyHint": self.read_only_hint,
                "destructiveHint": self.destructive_hint,
                "idempotentHint": self.idempotent_hint,
            },
        })
    }
}

/// Every v1 tool, in the order `tools/list` returns them.
///
/// The hints follow the ADR table literally. In particular the read-only hints
/// are `true` even for tools that touch internal state (a replica cache, a job
/// flag): the hint means "an agent may call this without asking the user", not
/// "this writes nothing". Do not "correct" them.
pub fn tools() -> Vec<Tool> {
    vec![
        read_only(
            "now",
            "Current user, today's civil date, and the time zone. Takes no arguments. Call it before date-based tools.",
            no_arguments(),
        ),
        read_only(
            "list_tasks",
            "List the user's tasks as slim rows, not the REST envelope. Takes no arguments. The result is capped at 100 tasks.",
            no_arguments(),
        ),
        read_only(
            "get_agenda",
            "Agenda items for a date range. `from` and `to` are optional but must be set together as YYYY-MM-DD; omit both for the default window. The result is capped at 80 items and 40 backlog rows.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "First date (YYYY-MM-DD); set together with `to`" },
                    "to": { "type": "string", "description": "Last date (YYYY-MM-DD); set together with `from`" },
                },
                "additionalProperties": false,
            }),
        ),
        read_only(
            "list_events",
            "List calendar events. `time_min` and `time_max` are both required RFC 3339 timestamps and must span at most 31 days. The result is capped at 100 events and omits the sync envelope.",
            json!({
                "type": "object",
                "properties": {
                    "time_min": { "type": "string", "description": "RFC 3339 start of the window" },
                    "time_max": { "type": "string", "description": "RFC 3339 end of the window" },
                },
                "required": ["time_min", "time_max"],
                "additionalProperties": false,
            }),
        ),
        read_only(
            "list_calendars",
            "List the user's calendars as slim rows. Takes no arguments. The result is capped at 50 calendars.",
            no_arguments(),
        ),
        read_only(
            "reports_status",
            "Report backfill and digest status. Takes no arguments. May flip the backfill job to running; that is an internal flag, not a user mutation.",
            no_arguments(),
        ),
        read_only(
            "rules_scoreboard",
            "Rule scoreboard and history. Takes no arguments. May seed default rules and queue history, the way the rules page does.",
            no_arguments(),
        ),
        read_only(
            "classify_title",
            "Classify a task title against the user's categories. `title` is required; `category_id` is optional. Creates nothing.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Title to classify" },
                    "category_id": { "type": "string", "description": "Optional category to classify inside" },
                },
                "required": ["title"],
                "additionalProperties": false,
            }),
        ),
        mutating(
            "create_task",
            "Create a task. `title` is required; `description`, `duration_minutes`, `priority`, and `difficulty` are optional. Not idempotent: every call creates a row.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "duration_minutes": { "type": "integer", "minimum": 1 },
                    "priority": { "type": "string", "enum": ["high", "medium", "low"] },
                    "difficulty": { "type": "string", "enum": ["easy", "medium", "hard"] },
                },
                "required": ["title"],
                "additionalProperties": false,
            }),
            false,
            false,
        ),
        mutating(
            "update_task",
            "Update a task's fields. `id` is required; `title`, `description`, `duration_minutes`, `priority`, and `difficulty` are optional. Status changes go through `move_task`, not this tool.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "description": { "type": "string" },
                    "duration_minutes": { "type": "integer", "minimum": 1 },
                    "priority": { "type": "string", "enum": ["high", "medium", "low"] },
                    "difficulty": { "type": "string", "enum": ["easy", "medium", "hard"] },
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
            false,
            true,
        ),
        mutating(
            "move_task",
            "Move a task to `status`: OPEN, PLANNED, IN_PROGRESS, COMPLETED, or DISCARDED. `id` and `status` are required. Rank is the server default. Can discard or complete a task, and entering or leaving IN_PROGRESS writes Google.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "status": {
                        "type": "string",
                        "enum": ["OPEN", "PLANNED", "IN_PROGRESS", "COMPLETED", "DISCARDED"],
                    },
                },
                "required": ["id", "status"],
                "additionalProperties": false,
            }),
            true,
            true,
        ),
        mutating(
            "move_agenda_item",
            "Reorder an agenda item. `id` and `sort_order` (an integer of at least 0) are required.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "sort_order": { "type": "integer", "minimum": 0 },
                },
                "required": ["id", "sort_order"],
                "additionalProperties": false,
            }),
            false,
            true,
        ),
        mutating(
            "reschedule_agenda_item",
            "Reschedule an agenda item. `id` is required. Set `date` (YYYY-MM-DD), optionally with `sort_order` (>= 0), or set `backlog: true` to return it to the backlog; when `backlog` is true the rank is ignored.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "date": { "type": "string", "description": "Target date (YYYY-MM-DD)" },
                    "sort_order": { "type": "integer", "minimum": 0 },
                    "backlog": { "type": "boolean", "description": "Return the item to the backlog" },
                },
                "required": ["id"],
                "additionalProperties": false,
            }),
            false,
            true,
        ),
        mutating(
            "create_event",
            "Create a calendar event. `summary`, `start`, and `end` are required; `description`, `is_all_day`, `start_time_zone`, and `calendar_id` are optional. `start`/`end` are RFC 3339 instants, or civil dates when `is_all_day` is true. Omitting `calendar_id` lets title classification pick the calendar. Task, routine, and occurrence links are not accepted.",
            json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" },
                    "start": { "type": "string", "description": "RFC 3339 start instant; when `is_all_day` is true, a civil `YYYY-MM-DD` (or an RFC 3339 value whose date prefix is used)" },
                    "end": { "type": "string", "description": "RFC 3339 end instant; when `is_all_day` is true, a civil `YYYY-MM-DD` (or an RFC 3339 value whose date prefix is used)" },
                    "description": { "type": "string" },
                    "is_all_day": { "type": "boolean" },
                    "start_time_zone": { "type": "string", "description": "IANA zone for a timed event; ignored when `is_all_day` is true" },
                    "calendar_id": { "type": "string", "description": "The local `id` from `list_calendars` (not `google_calendar_id`); omit to classify the title" },
                },
                "required": ["summary", "start", "end"],
                "additionalProperties": false,
            }),
            false,
            false,
        ),
    ]
}

/// Looks a tool up in the catalog. `None` means the name is unknown.
pub fn tool_by_name(name: &str) -> Option<Tool> {
    tools().into_iter().find(|tool| tool.name == name)
}

/// A read-only tool: `readOnlyHint: true`, `destructiveHint: false`,
/// `idempotentHint: true`.
fn read_only(name: &'static str, description: &'static str, input_schema: Value) -> Tool {
    Tool {
        name,
        description,
        input_schema,
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
    }
}

/// A mutating tool; `destructive_hint` and `idempotent_hint` come from the ADR
/// table.
fn mutating(
    name: &'static str,
    description: &'static str,
    input_schema: Value,
    destructive_hint: bool,
    idempotent_hint: bool,
) -> Tool {
    Tool {
        name,
        description,
        input_schema,
        read_only_hint: false,
        destructive_hint,
        idempotent_hint,
    }
}

/// Schema for a tool that takes no arguments at all.
fn no_arguments() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}

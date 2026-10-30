//! MCP (Model Context Protocol) server core — ADR 0009.
//!
//! The protocol is hand-rolled and lives entirely in this crate: no MCP server
//! framework, no tokio runtime, no HTTP types, no I/O. A caller hands
//! [`handle_post`] the raw POST body and a [`ToolExecutor`]; it answers with an
//! [`McpResponse`] (a status and an optional JSON body) that the Worker sends
//! back verbatim.
//!
//! - [`protocol`] owns the JSON-RPC envelope, version negotiation, and method
//!   dispatch.
//! - [`tools`] is the v1 catalog: names, descriptions, argument schemas, and
//!   annotation hints.
//! - [`upstream`] holds the pure helpers the tool slices use to turn an
//!   upstream REST response into a tool outcome and to validate arguments.
//! - [`project`] turns a REST envelope into the small JSON value a tool
//!   returns, with every list capped.
//! - [`write`] holds the pure argument builders and result projectors the
//!   write tools use on either side of a sub-request.
//!
//! Everything is stateless: each POST stands alone, so `initialize` is not
//! required before `tools/list` or `tools/call`.

mod project;
mod protocol;
mod tools;
mod upstream;
mod write;

#[cfg(test)]
mod tests;

pub use project::{
    agenda_bounds, project_agenda, project_calendars, project_events, project_now, project_tasks,
};
pub use protocol::{
    handle_post, McpResponse, ToolExecution, ToolExecutor, LATEST_PROTOCOL_VERSION,
    MAX_TOOL_RESULT_BYTES, SERVER_NAME, SUPPORTED_PROTOCOL_VERSIONS,
};
pub use tools::{tool_by_name, tools, Tool};
pub use upstream::{
    allow_subrequest, is_safe_id, tool_outcome_from_upstream, validate_event_window,
    GOOGLE_ACCESS_EXPIRED, MAX_EVENT_SPAN_SECS,
};
pub use write::{
    build_create_task, build_move_agenda_item, build_move_task, build_reschedule_agenda_item,
    build_update_task, create_event_body, create_event_calendar, primary_calendar_id,
    project_agenda_item_result, project_created_event, project_move_result, project_task_result,
};

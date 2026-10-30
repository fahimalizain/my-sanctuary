//! Native tests for the MCP protocol core.
//!
//! `handle_post` is async but there is no tokio here: `pollster::block_on`
//! drives it, exactly like the rest of `api-core`.

use std::sync::Mutex;

use serde_json::{json, Value};

use super::project::{MAX_AGENDA_BACKLOG, MAX_AGENDA_ITEMS, MAX_TASKS};
use super::*;

/// Every v1 tool with its `readOnlyHint`, `destructiveHint`, and
/// `idempotentHint` from ADR 0009 §8, in `tools/list` order.
const EXPECTED_TOOLS: [(&str, bool, bool, bool); 14] = [
    ("now", true, false, true),
    ("list_tasks", true, false, true),
    ("get_agenda", true, false, true),
    ("list_events", true, false, true),
    ("list_calendars", true, false, true),
    ("reports_status", true, false, true),
    ("rules_scoreboard", true, false, true),
    ("classify_title", true, false, true),
    ("create_task", false, false, false),
    ("update_task", false, false, true),
    ("move_task", false, true, true),
    ("move_agenda_item", false, false, true),
    ("reschedule_agenda_item", false, false, true),
    ("create_event", false, false, false),
];

/// The version `post` passes in, so `serverInfo.version` is checkable.
const SERVER_VERSION: &str = "1.2.3";

/// Records every `execute` call and always returns one scripted outcome.
struct ScriptedExecutor {
    outcome: ToolExecution,
    calls: Mutex<Vec<(String, Value)>>,
}

impl ScriptedExecutor {
    fn new(outcome: ToolExecution) -> Self {
        Self {
            outcome,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn ok(value: Value) -> Self {
        Self::new(ToolExecution::Ok(value))
    }

    fn failed(message: &str) -> Self {
        Self::new(ToolExecution::Failed(message.to_string()))
    }

    /// `(name, arguments)` for every call, in order.
    fn recorded_calls(&self) -> Vec<(String, Value)> {
        self.calls
            .lock()
            .expect("scripted executor lock is not poisoned")
            .clone()
    }
}

#[async_trait::async_trait(?Send)]
impl ToolExecutor for ScriptedExecutor {
    async fn execute(&self, name: &str, arguments: &Value) -> ToolExecution {
        self.calls
            .lock()
            .expect("scripted executor lock is not poisoned")
            .push((name.to_string(), arguments.clone()));
        self.outcome.clone()
    }
}

fn post(executor: &ScriptedExecutor, request: &Value) -> McpResponse {
    post_raw(executor, &request.to_string())
}

fn post_raw(executor: &ScriptedExecutor, body: &str) -> McpResponse {
    pollster::block_on(handle_post(body, SERVER_VERSION, executor))
}

fn response_json(response: &McpResponse) -> Value {
    let body = response.body.as_deref().expect("response has a JSON body");
    serde_json::from_str(body).expect("response body is JSON")
}

fn error_code(body: &Value) -> i64 {
    body["error"]["code"].as_i64().expect("error has a code")
}

fn error_message(body: &Value) -> &str {
    body["error"]["message"]
        .as_str()
        .expect("error has a message")
}

// ──────────────────────────────────────────
// initialize and ping
// ──────────────────────────────────────────

#[test]
fn initialize_echoes_every_supported_version() {
    for version in SUPPORTED_PROTOCOL_VERSIONS {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(
            &executor,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": version,
                    "capabilities": {},
                    "clientInfo": { "name": "inspector", "version": "0.1" },
                    "_meta": { "ignored": true },
                },
            }),
        );

        assert_eq!(response.status, 200, "{version}");
        let body = response_json(&response);
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["id"], 1);
        assert!(
            body.get("error").is_none(),
            "{version} must not be an error"
        );
        assert_eq!(body["result"]["protocolVersion"], *version);
        assert!(executor.recorded_calls().is_empty());
    }
}

#[test]
fn initialize_falls_back_to_the_latest_version_for_an_unknown_one() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": { "protocolVersion": "1999-01-01" },
        }),
    );

    assert_eq!(response.status, 200);
    let body = response_json(&response);
    assert!(
        body.get("error").is_none(),
        "an unknown version is not an error"
    );
    assert_eq!(body["id"], "init");
    assert_eq!(body["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn initialize_advertises_only_the_tools_capability() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" },
        }),
    );

    let body = response_json(&response);
    let result = &body["result"];
    assert_eq!(
        result["capabilities"],
        json!({ "tools": { "listChanged": false } })
    );
    assert_eq!(
        result["serverInfo"],
        json!({ "name": "sanctuary", "version": SERVER_VERSION })
    );
    assert!(result.get("instructions").is_none());
    assert!(result["capabilities"].get("resources").is_none());
    assert!(result["capabilities"].get("prompts").is_none());
    assert!(result["capabilities"].get("logging").is_none());
}

#[test]
fn initialize_requires_a_protocol_version() {
    for request in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "inspector" } },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": 5 },
        }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": [] }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32602);
        assert_eq!(error_message(&body), "protocolVersion is required");
        assert_eq!(body["id"], 1);
    }
}

#[test]
fn ping_returns_an_empty_object_and_ignores_params() {
    for request in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
        json!({
            "jsonrpc": "2.0",
            "id": "p",
            "method": "ping",
            "params": { "anything": [1, 2, 3], "_meta": { "progressToken": 7 } },
        }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(body["result"], json!({}));
        assert!(executor.recorded_calls().is_empty());
    }
}

// ──────────────────────────────────────────
// Notifications and client responses
// ──────────────────────────────────────────

#[test]
fn notifications_are_acknowledged_with_an_empty_body() {
    for request in [
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 1, "reason": "user aborted" },
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/whatever" }),
        json!({ "jsonrpc": "2.0", "method": "ping" }),
        json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": "create_task", "arguments": { "title": "x" } },
        }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 202, "{request}");
        assert_eq!(response.body, None, "{request}");
        assert!(
            executor.recorded_calls().is_empty(),
            "a notification never runs a tool: {request}"
        );
    }
}

#[test]
fn a_client_response_is_acknowledged_with_an_empty_body() {
    for request in [
        json!({ "jsonrpc": "2.0", "id": 1, "result": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "not supported" },
        }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 202);
        assert_eq!(response.body, None);
        assert!(executor.recorded_calls().is_empty());
    }
}

// ──────────────────────────────────────────
// tools/list
// ──────────────────────────────────────────

#[test]
fn tools_list_returns_one_page_of_the_v1_catalog() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );

    assert_eq!(response.status, 200);
    let body = response_json(&response);
    let result = &body["result"];
    assert!(result.get("nextCursor").is_none(), "one page, no cursor");

    let listed = result["tools"].as_array().expect("tools is an array");
    let names: Vec<&str> = listed
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    let expected_names: Vec<&str> = EXPECTED_TOOLS.iter().map(|(name, ..)| *name).collect();
    assert_eq!(names, expected_names);
    assert!(executor.recorded_calls().is_empty());
}

#[test]
fn every_listed_tool_carries_its_annotations_schema_and_description() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    let body = response_json(&response);
    let listed = body["result"]["tools"]
        .as_array()
        .expect("tools is an array");

    assert_eq!(listed.len(), EXPECTED_TOOLS.len());
    for (tool, (name, read_only, destructive, idempotent)) in listed.iter().zip(EXPECTED_TOOLS) {
        assert_eq!(tool["name"], name);

        let annotations = &tool["annotations"];
        assert_eq!(annotations["readOnlyHint"], read_only, "{name}");
        assert_eq!(annotations["destructiveHint"], destructive, "{name}");
        assert_eq!(annotations["idempotentHint"], idempotent, "{name}");

        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        assert!(schema["properties"].is_object(), "{name}");

        let description = tool["description"].as_str().expect("description");
        assert!(!description.is_empty(), "{name}");
        assert!(description.len() < 500, "{name} description is too long");
    }
}

#[test]
fn the_catalog_schemas_stay_narrow() {
    let catalog = tools();

    let move_task = catalog
        .iter()
        .find(|tool| tool.name == "move_task")
        .expect("move_task is in the catalog");
    let properties = &move_task.input_schema["properties"];
    assert!(
        properties.get("sort_order").is_none(),
        "rank is the server default"
    );
    assert_eq!(
        properties["status"]["enum"],
        json!(["OPEN", "PLANNED", "IN_PROGRESS", "COMPLETED", "DISCARDED"])
    );
    assert_eq!(move_task.input_schema["required"], json!(["id", "status"]));

    let update_task = catalog
        .iter()
        .find(|tool| tool.name == "update_task")
        .expect("update_task is in the catalog");
    assert!(
        update_task.input_schema["properties"]
            .get("status")
            .is_none(),
        "status changes go through move_task"
    );
    assert_eq!(update_task.input_schema["required"], json!(["id"]));

    let create_event = catalog
        .iter()
        .find(|tool| tool.name == "create_event")
        .expect("create_event is in the catalog");
    let create_event_properties = &create_event.input_schema["properties"];
    for unexposed in ["task_id", "routine_id", "occurrence_id", "color_hex"] {
        assert!(
            create_event_properties.get(unexposed).is_none(),
            "{unexposed} is deliberately not exposed"
        );
    }
}

#[test]
fn descriptions_mention_the_documented_caps() {
    let catalog = tools();
    let description = |name: &str| {
        catalog
            .iter()
            .find(|tool| tool.name == name)
            .expect("tool is in the catalog")
            .description
    };
    assert!(description("list_events").contains("31"));
    assert!(description("list_tasks").contains("100"));
    assert!(description("list_calendars").contains("50"));
    assert!(description("get_agenda").contains("80"));
}

#[test]
fn tools_list_rejects_a_cursor() {
    for request in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "cursor": "next" },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "cursor": 5 },
        }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": [] }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32602);
        assert_eq!(error_message(&body), "unknown cursor");
    }
}

#[test]
fn tools_list_treats_an_empty_or_null_cursor_as_no_cursor() {
    for request in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "cursor": "" },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": { "cursor": null },
        }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert!(body.get("error").is_none(), "{request}");
        assert_eq!(
            body["result"]["tools"].as_array().expect("tools").len(),
            EXPECTED_TOOLS.len()
        );
        assert!(body["result"].get("nextCursor").is_none());
    }
}

// ──────────────────────────────────────────
// tools/call
// ──────────────────────────────────────────

#[test]
fn tools_call_preserves_numeric_and_string_ids() {
    let value = json!({ "tasks": [], "count": 0 });
    for id in [json!(1), json!("abc")] {
        let executor = ScriptedExecutor::ok(value.clone());
        let response = post(
            &executor,
            &json!({
                "jsonrpc": "2.0",
                "id": id.clone(),
                "method": "tools/call",
                "params": { "name": "list_tasks" },
            }),
        );

        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(body["id"], id);
        assert_eq!(
            body["id"].is_string(),
            id.is_string(),
            "id type is preserved"
        );
        assert!(body.get("error").is_none());

        let result = &body["result"];
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"], value);
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(
            result["content"][0]["text"],
            serde_json::to_string(&value).expect("value serializes")
        );
    }
}

#[test]
fn tools_call_invokes_the_executor_with_the_arguments_object() {
    let arguments = json!({ "title": "write the ADR", "duration_minutes": 25 });
    let executor = ScriptedExecutor::ok(json!({ "created": true }));
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": { "name": "create_task", "arguments": arguments.clone() },
        }),
    );

    assert_eq!(response.status, 200);
    assert_eq!(
        executor.recorded_calls(),
        vec![("create_task".to_string(), arguments)]
    );
}

#[test]
fn tools_call_without_arguments_passes_an_empty_object() {
    let executor = ScriptedExecutor::ok(json!({ "ok": true }));
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": { "name": "now" },
        }),
    );

    assert_eq!(response.status, 200);
    assert_eq!(
        executor.recorded_calls(),
        vec![("now".to_string(), json!({}))]
    );
}

#[test]
fn tools_call_rejects_non_object_arguments_before_the_executor() {
    for arguments in [json!([]), json!("x"), json!(1), json!(true), json!(null)] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(
            &executor,
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "now", "arguments": arguments.clone() },
            }),
        );

        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32602);
        assert_eq!(error_message(&body), "arguments must be an object");
        assert!(
            executor.recorded_calls().is_empty(),
            "{arguments} must not run a tool"
        );
    }
}

#[test]
fn unknown_tools_are_invalid_params_and_never_reach_the_executor() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": "nope", "arguments": {} },
        }),
    );

    assert_eq!(response.status, 200);
    let body = response_json(&response);
    assert_eq!(error_code(&body), -32602);
    assert_eq!(error_message(&body), "unknown tool: nope");
    assert!(executor.recorded_calls().is_empty());
}

#[test]
fn tools_call_requires_a_tool_name() {
    for request in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call" }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "" },
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": 7 },
        }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": [] }),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 200);
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32602);
        assert_eq!(error_message(&body), "tool name is required");
        assert!(executor.recorded_calls().is_empty());
    }
}

#[test]
fn a_failed_tool_is_a_result_with_is_error() {
    let executor = ScriptedExecutor::failed("nope");
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "tools/call",
            "params": { "name": "create_task", "arguments": { "title": "x" } },
        }),
    );

    assert_eq!(response.status, 200);
    let body = response_json(&response);
    assert_eq!(body["id"], "abc");
    assert!(
        body.get("error").is_none(),
        "a tool failure is not a protocol error"
    );
    let result = &body["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "nope");
    assert!(result.get("structuredContent").is_none());
}

#[test]
fn an_empty_failure_message_becomes_a_placeholder() {
    let executor = ScriptedExecutor::failed("");
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "now" },
        }),
    );

    let body = response_json(&response);
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(body["result"]["content"][0]["text"], "tool call failed");
}

#[test]
fn an_oversized_result_becomes_a_tool_error() {
    let value = json!("a".repeat(65_535));
    let text = serde_json::to_string(&value).expect("value serializes");
    assert_eq!(text.len(), 65_537, "65535 chars plus two quotes");

    let executor = ScriptedExecutor::ok(value);
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_events" },
        }),
    );

    assert_eq!(response.status, 200);
    let body = response_json(&response);
    assert!(body.get("error").is_none(), "not a JSON-RPC error");
    let result = &body["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][0]["text"], "result too large");
    assert!(result.get("structuredContent").is_none());
}

#[test]
fn a_result_at_the_size_limit_is_still_returned() {
    // 65536 bytes of compact JSON: 65534 chars plus two quotes.
    let value = json!("a".repeat(65_534));
    let text = serde_json::to_string(&value).expect("value serializes");
    assert_eq!(text.len(), MAX_TOOL_RESULT_BYTES);

    let executor = ScriptedExecutor::ok(value.clone());
    let response = post(
        &executor,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_events" },
        }),
    );

    let body = response_json(&response);
    assert_eq!(body["result"]["isError"], false);
    assert_eq!(body["result"]["content"][0]["text"], text);
    assert_eq!(body["result"]["structuredContent"], value);
}

// ──────────────────────────────────────────
// Framing errors
// ──────────────────────────────────────────

#[test]
fn unknown_methods_return_method_not_found_with_http_200() {
    let executor = ScriptedExecutor::ok(Value::Null);
    let response = post(
        &executor,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "resources/list" }),
    );
    assert_eq!(response.status, 200);
    let body = response_json(&response);
    assert_eq!(body["id"], 1, "the original id is preserved");
    assert_eq!(error_code(&body), -32601);
    assert_eq!(error_message(&body), "method not found");
}

#[test]
fn malformed_json_is_a_parse_error() {
    for raw in [
        "{",
        "",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"} trailing",
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post_raw(&executor, raw);
        assert_eq!(response.status, 400, "{raw:?}");
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32700, "{raw:?}");
        assert_eq!(error_message(&body), "parse error");
        assert!(body["id"].is_null());
    }
}

#[test]
fn batches_are_rejected_outright() {
    for raw in ["[]", r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post_raw(&executor, raw);
        assert_eq!(response.status, 400);
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32600);
        assert_eq!(error_message(&body), "JSON-RPC batches are not supported");
        assert!(body["id"].is_null());
        assert!(executor.recorded_calls().is_empty());
    }
}

#[test]
fn invalid_requests_are_rejected_with_http_400() {
    for request in [
        // missing / wrong `jsonrpc`
        json!({ "id": 1, "method": "ping" }),
        json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }),
        json!({ "jsonrpc": 2.0, "id": 1, "method": "ping" }),
        // neither a method nor a response
        json!({ "jsonrpc": "2.0", "id": 1 }),
        json!({ "jsonrpc": "2.0" }),
        json!({ "jsonrpc": "2.0", "id": null, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "id": true, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": { "a": 1 }, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 1, "method": 5 }),
        // not an object at all
        json!("not an object"),
        json!(42),
    ] {
        let executor = ScriptedExecutor::ok(Value::Null);
        let response = post(&executor, &request);
        assert_eq!(response.status, 400, "{request}");
        let body = response_json(&response);
        assert_eq!(error_code(&body), -32600, "{request}");
        assert_eq!(error_message(&body), "invalid request");
        assert!(body["id"].is_null(), "{request}");
        assert!(executor.recorded_calls().is_empty());
    }
}

// ──────────────────────────────────────────
// Upstream helper
// ──────────────────────────────────────────

#[test]
fn upstream_401_is_the_google_reauth_message() {
    assert_eq!(
        tool_outcome_from_upstream(401, r#"{"error":"unauthorized"}"#),
        ToolExecution::Failed(GOOGLE_ACCESS_EXPIRED.to_string())
    );
    assert_eq!(
        tool_outcome_from_upstream(401, ""),
        ToolExecution::Failed("Google access expired - sign in to the web app".to_string())
    );
}

#[test]
fn upstream_2xx_is_json_or_null_for_an_empty_body() {
    assert_eq!(
        tool_outcome_from_upstream(200, r#"{"a":1}"#),
        ToolExecution::Ok(json!({ "a": 1 }))
    );
    assert_eq!(
        tool_outcome_from_upstream(204, ""),
        ToolExecution::Ok(Value::Null)
    );
    assert_eq!(
        tool_outcome_from_upstream(200, "  \n\t "),
        ToolExecution::Ok(Value::Null)
    );
    assert_eq!(
        tool_outcome_from_upstream(201, "not json"),
        ToolExecution::Failed("upstream returned non-json".to_string())
    );
}

#[test]
fn upstream_errors_prefer_the_json_error_field() {
    assert_eq!(
        tool_outcome_from_upstream(400, r#"{"error":"invalid body"}"#),
        ToolExecution::Failed("invalid body".to_string())
    );
    assert_eq!(
        tool_outcome_from_upstream(500, "not json"),
        ToolExecution::Failed("upstream returned 500".to_string())
    );
    assert_eq!(
        tool_outcome_from_upstream(503, r#"{"message":"down"}"#),
        ToolExecution::Failed("upstream returned 503".to_string())
    );
    assert_eq!(
        tool_outcome_from_upstream(500, r#"{"error":{"code":1}}"#),
        ToolExecution::Failed("upstream returned 500".to_string())
    );
}

#[test]
fn upstream_error_messages_are_truncated_to_200_chars() {
    let body = json!({ "error": "x".repeat(300) }).to_string();
    match tool_outcome_from_upstream(409, &body) {
        ToolExecution::Failed(message) => assert_eq!(message.chars().count(), 200),
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ──────────────────────────────────────────
// Argument helpers
// ──────────────────────────────────────────

#[test]
fn validate_event_window_accepts_a_31_day_span() {
    assert_eq!(
        validate_event_window("2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z"),
        Ok((
            "2026-01-01T00:00:00Z".to_string(),
            "2026-02-01T00:00:00Z".to_string()
        ))
    );
}

#[test]
fn validate_event_window_normalizes_offsets_to_utc() {
    assert_eq!(
        validate_event_window("2026-01-01T00:00:00+05:30", "2026-02-01T00:00:00+05:30"),
        Ok((
            "2025-12-31T18:30:00Z".to_string(),
            "2026-01-31T18:30:00Z".to_string()
        ))
    );
}

#[test]
fn validate_event_window_rejects_a_span_over_31_days() {
    assert_eq!(
        validate_event_window("2026-01-01T00:00:00Z", "2026-02-01T00:00:01Z"),
        Err("time window must be at most 31 days".to_string())
    );
}

#[test]
fn validate_event_window_rejects_an_inverted_or_empty_window() {
    assert_eq!(
        validate_event_window("2026-02-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        Err("time_max must be after time_min".to_string())
    );
    assert_eq!(
        validate_event_window("2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z"),
        Err("time_max must be after time_min".to_string())
    );
}

#[test]
fn validate_event_window_reports_which_bound_is_bad() {
    let bad_min = validate_event_window("tomorrow", "2026-01-01T00:00:00Z").unwrap_err();
    assert!(bad_min.contains("time_min"), "{bad_min}");
    assert_eq!(bad_min, "invalid time_min: must be RFC 3339");

    assert_eq!(
        validate_event_window("2026-01-01T00:00:00Z", "soon"),
        Err("invalid time_max: must be RFC 3339".to_string())
    );
    // time_min is checked first.
    assert_eq!(
        validate_event_window("nope", "nope"),
        Err("invalid time_min: must be RFC 3339".to_string())
    );
}

#[test]
fn is_safe_id_accepts_uuids_and_simple_shapes() {
    assert!(is_safe_id("9f1c2a7e-0b3d-4c8a-9e5f-1a2b3c4d5e6f"));
    assert!(is_safe_id("a_b-C9"));
    assert!(is_safe_id(&"a".repeat(64)));
}

#[test]
fn is_safe_id_rejects_paths_spaces_and_oversized_ids() {
    for bad in ["", "..", ".", "a/b", "../keys", "/api/tasks", "a b", "a.b"] {
        assert!(!is_safe_id(bad), "{bad:?} must be rejected");
    }
    assert!(!is_safe_id(&"a".repeat(65)));
}

/// A real-shaped path id: a UUID passes [`is_safe_id`] and must stay allowed.
const UUID_TASK_PATH: &str = "/api/tasks/9f1c2a7e-0b3d-4c8a-9e5f-1a2b3c4d5e6f";

#[test]
fn allow_subrequest_accepts_every_pair_in_the_table() {
    for (method, path) in [
        ("GET", "/auth/me"),
        ("GET", "/api/tasks"),
        ("POST", "/api/tasks"),
        ("GET", "/api/tasks/classify"),
        ("PATCH", "/api/tasks/ab12"),
        ("PATCH", UUID_TASK_PATH),
        ("POST", "/api/tasks/ab12/move"),
        ("GET", "/api/agenda"),
        ("POST", "/api/agenda/items/ab12/move"),
        ("POST", "/api/agenda/items/ab12/reschedule"),
        ("GET", "/api/calendar/events"),
        ("POST", "/api/calendar/events"),
        ("GET", "/api/calendar/calendars"),
        ("GET", "/api/reports/status"),
        ("GET", "/api/rules/scoreboard"),
    ] {
        assert!(
            allow_subrequest(method, path),
            "{method} {path} must be allowed"
        );
    }
}

#[test]
fn allow_subrequest_refuses_unlisted_methods_and_paths() {
    for (method, path) in [
        // Deletes and the task timer verbs are not in the table.
        ("DELETE", "/api/tasks/ab12"),
        ("POST", "/api/tasks/ab12/start"),
        ("POST", "/api/tasks/ab12/stop"),
        ("POST", "/api/tasks/ab12/complete"),
        ("POST", "/api/tasks/ab12/focus"),
        // `classify` is a GET; the POST would create a task instead.
        ("POST", "/api/tasks/classify"),
        // Calendar repair and a single calendar have no entry.
        ("POST", "/api/calendar/calendars/ab12/repair"),
        ("GET", "/api/calendar/calendars/ab12"),
        ("GET", "/api/calendar/events/ab12"),
        // Key management stays cookie-only.
        ("GET", "/api/keys"),
        ("POST", "/api/keys"),
        ("DELETE", "/api/keys/ab12"),
        // The MCP endpoint itself is not a sub-request target.
        ("POST", "/api/mcp"),
        // Right path, wrong method.
        ("GET", "/api/agenda/items/ab12/move"),
        ("GET", "/api/tasks/ab12"),
        ("GET", "/api/agenda/items/ab12/reschedule"),
        // Nothing outside `/auth` and `/api` is reachable.
        ("GET", "/"),
        ("GET", "/api"),
    ] {
        assert!(
            !allow_subrequest(method, path),
            "{method} {path} must be refused"
        );
    }
}

#[test]
fn allow_subrequest_refuses_unsafe_ids_and_trailing_slashes() {
    for (method, path) in [
        // `..`, embedded slashes, an empty id, and a trailing slash.
        ("PATCH", "/api/tasks/../keys"),
        ("PATCH", "/api/tasks/.."),
        ("PATCH", "/api/tasks/a/b"),
        ("PATCH", "/api/tasks/"),
        ("POST", "/api/tasks/"),
        ("POST", "/api/tasks//move"),
        ("POST", "/api/agenda/items//move"),
        ("POST", "/api/agenda/items/../reschedule"),
        ("POST", "/api/agenda/items/a/b/move"),
        // A query string is never part of an allowlisted path.
        ("GET", "/api/tasks?include=all"),
        ("PATCH", "/api/tasks/ab12?force=1"),
    ] {
        assert!(
            !allow_subrequest(method, path),
            "{method} {path} must be refused"
        );
    }
    let too_long = format!("/api/tasks/{}", "a".repeat(65));
    assert!(!allow_subrequest("PATCH", &too_long));
}

// ──────────────────────────────────────────
// Projectors
// ──────────────────────────────────────────

/// The keys of a JSON object, sorted, so a test can assert an exact shape.
fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("expected a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// Compact byte length of a tool result — the measure `protocol` applies to
/// `content[0].text` before answering `isError: "result too large"`.
fn compact_len(value: &Value) -> usize {
    serde_json::to_string(value).expect("serialized").len()
}

/// One REST `TaskView`: every column plus the computed fields, including the
/// ones a slim row must drop.
fn rest_task(id: &str) -> Value {
    json!({
        "id": id,
        "user_id": "u1",
        "title": "Ship the slice",
        "display_title": "Ship the slice",
        "description": "internal notes the agent never sees",
        "duration_minutes": 45,
        "priority": "high",
        "difficulty": "medium",
        "sort_order": 3,
        "status": "OPEN",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z",
        "focused": true,
        "category": {
            "id": "cat1",
            "title": "Work",
            "slug": "work",
            "list_id": "list1",
            "inherited_list_id": "list1",
            "is_untracked": false,
            "color": "#112233",
        },
    })
}

/// A REST category summary, as embedded on tasks and occurrences.
fn rest_category() -> Value {
    json!({
        "id": "cat1",
        "title": "Work",
        "slug": "work",
        "list_id": null,
        "inherited_list_id": null,
        "is_untracked": false,
        "color": "#112233",
    })
}

/// A `kind: "task"` agenda row.
fn task_agenda_item(index: usize) -> Value {
    json!({
        "id": format!("i{index}"),
        "user_id": "u1",
        "local_date": "2026-02-01",
        "kind": "task",
        "ref_id": format!("t{index}"),
        "sort_order": index,
        "task": rest_task(&format!("t{index}")),
        "occurrence": null,
    })
}

/// A REST task whose title and display_title are `width` characters of `a`:
/// the payload shape that pushes a full list over the byte budget.
fn wide_task(id: &str, width: usize) -> Value {
    let title = "a".repeat(width);
    let mut task = rest_task(id);
    task["title"] = json!(title);
    task["display_title"] = json!(title);
    task
}

/// A tapped-wide agenda row; the embedded task is the part that grows.
fn wide_agenda_item(index: usize, width: usize) -> Value {
    let mut item = task_agenda_item(index);
    item["task"] = wide_task(&format!("t{index}"), width);
    item
}

/// A `kind: "occurrence"` agenda row, rrule blob and timestamps included.
fn occurrence_agenda_item(index: usize) -> Value {
    json!({
        "id": format!("i{index}"),
        "user_id": "u1",
        "local_date": "2026-02-01",
        "kind": "occurrence",
        "ref_id": format!("o{index}"),
        "sort_order": index,
        "task": null,
        "occurrence": {
            "id": format!("o{index}"),
            "routine_id": "r1",
            "user_id": "u1",
            "local_date": "2026-02-01",
            "title": null,
            "resolved_title": "Morning walk",
            "status": "pending",
            "estimated_minutes": 20,
            "rrule": "DTSTART:20260101T080000Z\nRRULE:FREQ=DAILY",
            "routine_deleted": false,
            "calendar_id": null,
            "google_event_id": null,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "category": rest_category(),
        },
    })
}

/// One flat REST event: every `CalendarEvent` field plus the painted `color`.
fn rest_event(index: usize) -> Value {
    json!({
        "id": format!("e{index}"),
        "calendar_id": "cal1",
        "google_event_id": format!("g{index}"),
        "google_etag": "\"etag\"",
        "google_updated_at": "2026-01-01T00:00:00Z",
        "last_synced_at": "2026-01-01T00:00:00Z",
        "title": "Standup",
        "description": "daily sync",
        "start_time": "2026-02-01T09:00:00Z",
        "end_time": "2026-02-01T09:15:00Z",
        "recurrence": "[]",
        "task_id": "",
        "ical_uid": "",
        "sequence": 0,
        "status": "confirmed",
        "recurring_event_id": "",
        "original_start": "",
        "start_time_zone": "UTC",
        "end_time_zone": "UTC",
        "is_all_day": false,
        "category_id": null,
        "is_self_organized": true,
        "event_type": "default",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
        "deleted_at": null,
        "color": "#445566",
    })
}

/// One `CalendarView` row plus a sync internal that must never be copied.
fn rest_calendar(index: usize) -> Value {
    json!({
        "id": format!("cal{index}"),
        "google_calendar_id": format!("g{index}@group.calendar.google.com"),
        "summary": "Home",
        "time_zone": "Asia/Kolkata",
        "is_primary": index == 0,
        "access_role": "owner",
        "sync_enabled": true,
        "sync_token": "private-sync-token",
        "last_synced_at": "2026-01-01T00:00:00Z",
        "deleted_at": null,
    })
}

#[test]
fn agenda_bounds_defaults_to_no_query_when_neither_bound_is_set() {
    assert_eq!(agenda_bounds(&json!({})), Ok(None));
    // A JSON null is "not set", exactly like an absent key.
    assert_eq!(
        agenda_bounds(&json!({ "from": null, "to": null })),
        Ok(None)
    );
    assert_eq!(agenda_bounds(&Value::Null), Ok(None));
}

#[test]
fn agenda_bounds_requires_from_and_to_together() {
    for arguments in [
        json!({ "from": "2026-01-01" }),
        json!({ "to": "2026-01-01" }),
        json!({ "from": "2026-01-01", "to": null }),
        json!({ "from": null, "to": "2026-01-01" }),
    ] {
        assert_eq!(
            agenda_bounds(&arguments),
            Err("from and to must be set together".to_string()),
            "{arguments}"
        );
    }
}

#[test]
fn agenda_bounds_checks_the_shape_of_each_bound() {
    assert_eq!(
        agenda_bounds(&json!({ "from": "2026-01-01", "to": "2026-01-07" })),
        Ok(Some(("2026-01-01".to_string(), "2026-01-07".to_string())))
    );
    // Shape only: a month that does not exist is the REST handler's 400.
    assert_eq!(
        agenda_bounds(&json!({ "from": "2026-13-01", "to": "2026-13-02" })),
        Ok(Some(("2026-13-01".to_string(), "2026-13-02".to_string())))
    );
    assert_eq!(
        agenda_bounds(&json!({ "from": "2026-1-01", "to": "2026-01-07" })),
        Err("from must be YYYY-MM-DD".to_string())
    );
    assert_eq!(
        agenda_bounds(&json!({ "from": "2026-01-01", "to": "07-01-2026" })),
        Err("to must be YYYY-MM-DD".to_string())
    );
    assert_eq!(
        agenda_bounds(&json!({ "from": 20260101, "to": "2026-01-07" })),
        Err("from must be YYYY-MM-DD".to_string())
    );
    // `from` is reported first when both are wrong.
    assert_eq!(
        agenda_bounds(&json!({ "from": false, "to": 5 })),
        Err("from must be YYYY-MM-DD".to_string())
    );
    // Never trimmed.
    assert_eq!(
        agenda_bounds(&json!({ "from": " 2026-01-01", "to": "2026-01-07" })),
        Err("from must be YYYY-MM-DD".to_string())
    );
}

#[test]
fn project_tasks_caps_at_100_and_slims_each_row() {
    let tasks: Vec<Value> = (0..101)
        .map(|index| rest_task(&format!("t{index:03}")))
        .collect();
    let projected = project_tasks(&json!({ "tasks": tasks })).expect("projected");

    let rows = projected["tasks"].as_array().expect("array");
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[0]["id"], json!("t000"));
    assert_eq!(rows[99]["id"], json!("t099"));
    assert_eq!(projected["truncated"], json!(true));
    assert_eq!(projected["omitted"], json!(1));

    let first = &rows[0];
    assert!(first.get("description").is_none());
    assert!(first.get("user_id").is_none());
    assert!(first.get("created_at").is_none());
    assert_eq!(first["duration_minutes"], json!(45));
    assert_eq!(first["sort_order"], json!(3));
    assert_eq!(first["focused"], json!(true));
    assert_eq!(
        keys(first),
        vec![
            "category",
            "difficulty",
            "display_title",
            "duration_minutes",
            "focused",
            "id",
            "priority",
            "sort_order",
            "status",
            "title",
        ]
    );
    // The category is three fields, not the REST summary.
    assert_eq!(keys(&first["category"]), vec!["color", "id", "title"]);
    assert_eq!(first["category"]["color"], json!("#112233"));
}

#[test]
fn project_tasks_reports_no_truncation_for_a_short_list() {
    let projected = project_tasks(&json!({ "tasks": [] })).expect("projected");
    assert_eq!(projected["tasks"], json!([]));
    assert_eq!(projected["truncated"], json!(false));
    assert!(projected.get("omitted").is_none());
}

#[test]
fn project_tasks_leaves_two_short_tasks_untruncated() {
    let projected =
        project_tasks(&json!({ "tasks": [rest_task("t1"), rest_task("t2")] })).expect("projected");
    assert_eq!(projected["tasks"].as_array().expect("array").len(), 2);
    assert_eq!(projected["truncated"], json!(false));
    assert!(projected.get("omitted").is_none());
}

/// 100 rows at the cap's expected title width must still fit the protocol's
/// budget: the row cap is not a byte cap, so nothing may be dropped here.
#[test]
fn project_tasks_fits_the_result_budget_with_long_titles() {
    let tasks: Vec<Value> = (0..MAX_TASKS)
        .map(|index| wide_task(&format!("t{index:03}"), 200))
        .collect();
    let projected = project_tasks(&json!({ "tasks": tasks })).expect("projected");

    assert!(compact_len(&projected) <= MAX_TOOL_RESULT_BYTES);
    assert_eq!(
        projected["tasks"].as_array().expect("array").len(),
        MAX_TASKS
    );
    assert_eq!(projected["truncated"], json!(false));
    assert!(projected.get("omitted").is_none());
}

#[test]
fn project_tasks_drops_the_tail_when_the_list_is_over_budget() {
    let tasks: Vec<Value> = (0..MAX_TASKS)
        .map(|index| wide_task(&format!("t{index:03}"), 400))
        .collect();
    let projected = project_tasks(&json!({ "tasks": tasks })).expect("projected");

    assert!(compact_len(&projected) <= MAX_TOOL_RESULT_BYTES);
    let kept = projected["tasks"].as_array().expect("array");
    assert!(kept.len() < MAX_TASKS, "the rows must be over budget");
    assert_eq!(kept[0]["id"], json!("t000"));
    assert_eq!(projected["truncated"], json!(true));
    assert_eq!(projected["omitted"], json!(MAX_TASKS - kept.len()));
}

#[test]
fn project_tasks_skips_a_non_object_row_but_still_counts_it() {
    let body = json!({ "tasks": [rest_task("t1"), 7, null, rest_task("t2")] });
    let projected = project_tasks(&body).expect("projected");
    let rows = projected["tasks"].as_array().expect("array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["id"], json!("t2"));
    assert_eq!(projected["truncated"], json!(false));
}

#[test]
fn project_tasks_zeroes_fields_of_the_wrong_json_type() {
    let body = json!({
        "tasks": [{
            "id": "t1",
            "duration_minutes": 1.5,
            "sort_order": "7",
            "focused": "yes",
            "priority": 3,
            "category": "Work",
        }]
    });
    let projected = project_tasks(&body).expect("projected");
    let task = &projected["tasks"][0];
    assert_eq!(task["duration_minutes"], json!(0));
    assert_eq!(task["sort_order"], json!(0));
    assert_eq!(task["focused"], json!(false));
    assert_eq!(task["priority"], json!(""));
    assert_eq!(task["category"], Value::Null);
}

#[test]
fn project_agenda_caps_items_and_backlog_independently() {
    let items: Vec<Value> = (0..81)
        .map(|index| {
            if index % 2 == 0 {
                task_agenda_item(index)
            } else {
                occurrence_agenda_item(index)
            }
        })
        .collect();
    let backlog: Vec<Value> = (0..41).map(task_agenda_item).collect();
    let body = json!({
        "items": items,
        "from": "2026-02-01",
        "to": "2026-02-07",
        "today": "2026-02-03",
        "time_zone": "Asia/Kolkata",
        "anchor_date": "2026-02-01",
        "earliest_date": "2025-01-01",
        "backlog": backlog,
    });

    let projected = project_agenda(&body).expect("projected");
    assert_eq!(projected["items"].as_array().expect("array").len(), 80);
    assert_eq!(projected["backlog"].as_array().expect("array").len(), 40);
    assert_eq!(projected["truncated_items"], json!(true));
    assert_eq!(projected["omitted_items"], json!(1));
    assert_eq!(projected["truncated_backlog"], json!(true));
    assert_eq!(projected["omitted_backlog"], json!(1));
    assert_eq!(projected["from"], json!("2026-02-01"));
    assert_eq!(projected["to"], json!("2026-02-07"));
    assert_eq!(projected["today"], json!("2026-02-03"));
    assert_eq!(projected["time_zone"], json!("Asia/Kolkata"));
    // Window internals the browser scrolls with are not part of the tool.
    assert!(projected.get("anchor_date").is_none());
    assert!(projected.get("earliest_date").is_none());

    let task_item = &projected["items"][0];
    assert_eq!(
        keys(task_item),
        vec![
            "id",
            "kind",
            "local_date",
            "occurrence",
            "ref_id",
            "sort_order",
            "task"
        ]
    );
    assert!(task_item["task"].get("description").is_none());
    assert_eq!(task_item["occurrence"], Value::Null);

    let occurrence_item = &projected["items"][1];
    let occurrence = &occurrence_item["occurrence"];
    assert!(occurrence.get("rrule").is_none());
    assert!(occurrence.get("user_id").is_none());
    assert!(occurrence.get("created_at").is_none());
    assert_eq!(
        keys(occurrence),
        vec![
            "category",
            "estimated_minutes",
            "id",
            "resolved_title",
            "routine_id",
            "status",
        ]
    );
    assert_eq!(occurrence["resolved_title"], json!("Morning walk"));
    assert_eq!(occurrence["estimated_minutes"], json!(20));
    assert_eq!(keys(&occurrence["category"]), vec!["color", "id", "title"]);
}

#[test]
fn project_agenda_treats_a_missing_or_null_backlog_as_empty() {
    for body in [
        json!({ "items": [], "today": "2026-02-03" }),
        json!({ "items": [], "backlog": null, "today": "2026-02-03" }),
    ] {
        let projected = project_agenda(&body).expect("projected");
        assert_eq!(projected["backlog"], json!([]));
        assert_eq!(projected["truncated_backlog"], json!(false));
        assert!(projected.get("omitted_backlog").is_none());
        assert_eq!(projected["truncated_items"], json!(false));
        assert!(projected.get("omitted_items").is_none());
        // A missing envelope scalar is an empty string, not an error.
        assert_eq!(projected["from"], json!(""));
    }
}

/// A full window at its own caps must still fit the protocol's byte budget —
/// otherwise the agent gets `isError: "result too large"` and no agenda at all.
#[test]
fn project_agenda_fits_the_result_budget_at_its_caps() {
    let items: Vec<Value> = (0..MAX_AGENDA_ITEMS)
        .map(|index| wide_agenda_item(index, 80))
        .collect();
    let backlog: Vec<Value> = (0..MAX_AGENDA_BACKLOG)
        .map(|index| wide_agenda_item(index, 80))
        .collect();
    let projected =
        project_agenda(&json!({ "items": items, "backlog": backlog })).expect("projected");

    assert!(compact_len(&projected) <= MAX_TOOL_RESULT_BYTES);
    let items_left = projected["items"].as_array().expect("array").len();
    let backlog_left = projected["backlog"].as_array().expect("array").len();
    if items_left < MAX_AGENDA_ITEMS {
        assert_eq!(projected["truncated_items"], json!(true));
        assert!(projected["omitted_items"].as_u64().expect("count") > 0);
    }
    if backlog_left < MAX_AGENDA_BACKLOG {
        assert_eq!(projected["truncated_backlog"], json!(true));
        assert!(projected["omitted_backlog"].as_u64().expect("count") > 0);
    }
}

/// The backlog goes first: it is the older, less urgent half of the window.
#[test]
fn project_agenda_drops_backlog_rows_before_items_to_fit_the_budget() {
    let items: Vec<Value> = (0..MAX_AGENDA_ITEMS)
        .map(|index| wide_agenda_item(index, 200))
        .collect();
    let backlog: Vec<Value> = (0..MAX_AGENDA_BACKLOG)
        .map(|index| wide_agenda_item(index, 200))
        .collect();
    let projected =
        project_agenda(&json!({ "items": items, "backlog": backlog })).expect("projected");

    assert!(compact_len(&projected) <= MAX_TOOL_RESULT_BYTES);
    let backlog_left = projected["backlog"].as_array().expect("array").len();
    let dropped = MAX_AGENDA_BACKLOG - backlog_left;
    assert!(dropped > 0, "the rows must be over budget at this width");
    // The cap itself dropped nothing (exactly 40 rows), so `omitted` counts
    // only what the byte budget shed.
    assert_eq!(projected["truncated_backlog"], json!(true));
    assert_eq!(projected["omitted_backlog"], json!(dropped));
    // The backlog alone covered the overshoot, so the window is untouched.
    assert_eq!(
        projected["items"].as_array().expect("array").len(),
        MAX_AGENDA_ITEMS
    );
    assert_eq!(projected["truncated_items"], json!(false));
    assert!(projected.get("omitted_items").is_none());
}

#[test]
fn clamp_scoreboard_leaves_a_small_scoreboard_untouched() {
    let value = json!({ "rules": [{ "name": "a", "evidence": [1], "strip": [1] }] });
    let clamped = clamp_scoreboard(value.clone());
    assert_eq!(clamped, value);
    assert_eq!(clamped["rules"][0]["evidence"], json!([1]));
    assert!(clamped.get("truncated").is_none());
}

#[test]
fn clamp_scoreboard_sheds_evidence_before_strip() {
    let evidence = "x".repeat(MAX_TOOL_RESULT_BYTES + 1024);
    let value = json!({ "rules": [{ "evidence": [evidence], "strip": [1], "name": "a" }] });

    let clamped = clamp_scoreboard(value);
    assert!(compact_len(&clamped) <= MAX_TOOL_RESULT_BYTES);
    assert!(clamped["rules"][0].get("evidence").is_none());
    // The strip array is enough on its own, so it stays.
    assert_eq!(clamped["rules"][0]["strip"], json!([1]));
    assert_eq!(clamped["truncated"], json!(true));
}

#[test]
fn clamp_scoreboard_pops_rules_when_the_fields_are_not_enough() {
    // Too wide to fit even without `strip`, so whole rules must go.
    let rule = json!({ "name": "y".repeat(40_000), "strip": [1] });
    let value = json!({ "rules": [rule.clone(), rule.clone(), rule] });

    let clamped = clamp_scoreboard(value);
    assert!(compact_len(&clamped) <= MAX_TOOL_RESULT_BYTES);
    assert_eq!(clamped["rules"].as_array().expect("array").len(), 1);
    assert!(clamped["rules"][0].get("strip").is_none());
    assert_eq!(clamped["truncated"], json!(true));
}

#[test]
fn clamp_scoreboard_leaves_a_foreign_shape_alone() {
    let value = json!({ "rules": ["x".repeat(MAX_TOOL_RESULT_BYTES + 1)] });
    let clamped = clamp_scoreboard(value.clone());
    assert_eq!(clamped, value);
}

#[test]
fn project_events_drops_the_sync_envelope_and_caps_at_100() {
    let events: Vec<Value> = (0..101).map(rest_event).collect();
    let body = json!({
        "events": events,
        "source": "replica",
        "sync": { "state": "healthy", "last_synced_at": "2026-01-01T00:00:00Z" },
    });

    let projected = project_events(&body).expect("projected");
    assert!(projected.get("sync").is_none());
    let rows = projected["events"].as_array().expect("array");
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[0]["id"], json!("e0"));
    assert_eq!(rows[99]["id"], json!("e99"));
    assert_eq!(projected["truncated"], json!(true));
    assert_eq!(projected["omitted"], json!(1));
    assert_eq!(projected["source"], json!("replica"));

    let first = &rows[0];
    assert_eq!(first["color"], json!("#445566"));
    assert_eq!(first["start_time_zone"], json!("UTC"));
    assert_eq!(first["is_all_day"], json!(false));
    assert!(first.get("description").is_none());
    assert!(first.get("google_event_id").is_none());
    assert_eq!(
        keys(first),
        vec![
            "calendar_id",
            "category_id",
            "color",
            "end_time",
            "id",
            "is_all_day",
            "start_time",
            "start_time_zone",
            "status",
            "title",
        ]
    );
}

#[test]
fn project_events_nulls_a_blank_or_missing_category_id() {
    let body = json!({
        "events": [
            { "id": "e1", "category_id": "" },
            { "id": "e2" },
            { "id": "e3", "category_id": "cat1" },
            { "id": "e4", "category_id": null },
        ],
        "source": "google",
    });
    let projected = project_events(&body).expect("projected");
    let rows = projected["events"].as_array().expect("array");
    for index in [0, 1, 3] {
        assert_eq!(rows[index]["category_id"], Value::Null, "row {index}");
    }
    assert_eq!(rows[2]["category_id"], json!("cat1"));
}

#[test]
fn project_calendars_caps_at_50_and_drops_sync_internals() {
    let calendars: Vec<Value> = (0..51).map(rest_calendar).collect();
    let projected = project_calendars(&json!({ "calendars": calendars })).expect("projected");

    let rows = projected["calendars"].as_array().expect("array");
    assert_eq!(rows.len(), 50);
    assert_eq!(rows[49]["id"], json!("cal49"));
    assert_eq!(projected["truncated"], json!(true));
    assert_eq!(projected["omitted"], json!(1));

    let first = &rows[0];
    assert!(first.get("sync_token").is_none());
    assert!(first.get("last_synced_at").is_none());
    assert_eq!(first["is_primary"], json!(true));
    assert_eq!(first["sync_enabled"], json!(true));
    assert_eq!(
        keys(first),
        vec![
            "access_role",
            "google_calendar_id",
            "id",
            "is_primary",
            "summary",
            "sync_enabled",
            "time_zone",
        ]
    );
}

#[test]
fn project_now_requires_a_user() {
    let agenda = json!({ "today": "2026-02-03", "time_zone": "UTC" });
    assert_eq!(
        project_now(&json!({ "user": null }), &agenda),
        Err("unauthorized".to_string())
    );
    assert_eq!(
        project_now(&json!({}), &agenda),
        Err("unauthorized".to_string())
    );
    assert_eq!(
        project_now(&json!({ "user": "u1" }), &agenda),
        Err("unauthorized".to_string())
    );
}

#[test]
fn project_now_returns_the_user_today_and_zone_without_items() {
    let me = json!({
        "user": {
            "id": "u1",
            "email": "a@b.c",
            "name": "A",
            "picture": "https://example.test/p.png",
        }
    });
    let agenda = json!({
        "items": [task_agenda_item(0)],
        "today": "2026-02-03",
        "time_zone": "Asia/Kolkata",
    });

    let projected = project_now(&me, &agenda).expect("projected");
    assert_eq!(projected["today"], json!("2026-02-03"));
    assert_eq!(projected["time_zone"], json!("Asia/Kolkata"));
    assert_eq!(projected["user"]["email"], json!("a@b.c"));
    assert_eq!(
        projected["user"]["picture"],
        json!("https://example.test/p.png")
    );
    assert_eq!(
        keys(&projected["user"]),
        vec!["email", "id", "name", "picture"]
    );
    assert!(projected.get("items").is_none());
    assert_eq!(keys(&projected), vec!["time_zone", "today", "user"]);

    // A missing picture is an empty string, never null.
    let sparse = project_now(&json!({ "user": { "id": "u1" } }), &agenda).expect("projected");
    assert_eq!(sparse["user"]["picture"], json!(""));
}

#[test]
fn project_now_rejects_an_agenda_without_today_or_zone() {
    let me = json!({ "user": { "id": "u1" } });
    for agenda in [
        json!({ "today": "", "time_zone": "UTC" }),
        json!({ "today": "2026-02-03" }),
        json!({ "today": "2026-02-03", "time_zone": "" }),
        json!({}),
    ] {
        assert_eq!(
            project_now(&me, &agenda),
            Err("agenda response missing today".to_string()),
            "{agenda}"
        );
    }
}

#[test]
fn every_projector_rejects_a_missing_or_non_array_list() {
    assert_eq!(
        project_tasks(&json!({ "tasks": {} })),
        Err("unexpected tasks response".to_string())
    );
    assert_eq!(
        project_tasks(&json!({})),
        Err("unexpected tasks response".to_string())
    );
    assert_eq!(
        project_agenda(&json!({ "items": "nope" })),
        Err("unexpected agenda response".to_string())
    );
    assert_eq!(
        project_agenda(&json!({ "items": [], "backlog": 3 })),
        Err("unexpected agenda response".to_string())
    );
    assert_eq!(
        project_events(&json!({ "events": null })),
        Err("unexpected events response".to_string())
    );
    assert_eq!(
        project_calendars(&json!({ "calendars": 1 })),
        Err("unexpected calendars response".to_string())
    );
}

// ──────────────────────────────────────────
// Write tools: argument builders
// ──────────────────────────────────────────

/// A minimal valid `create_event` argument object.
fn event_arguments() -> Value {
    json!({
        "summary": "Focus block",
        "start": "2026-02-01T09:00:00Z",
        "end": "2026-02-01T09:30:00Z",
    })
}

#[test]
fn build_create_task_keeps_only_the_fields_that_were_set() {
    let body = build_create_task(&json!({ "title": "Ship it" })).expect("built");
    assert_eq!(keys(&body), vec!["title"]);
    assert_eq!(body["title"], json!("Ship it"));

    // An empty string is a value; a null is an omission.
    let body = build_create_task(&json!({
        "title": "Ship it",
        "description": "",
        "duration_minutes": 30,
        "priority": "high",
        "difficulty": null,
    }))
    .expect("built");
    assert_eq!(
        keys(&body),
        vec!["description", "duration_minutes", "priority", "title"]
    );
    assert_eq!(body["description"], json!(""));
    assert_eq!(body["duration_minutes"], json!(30));
    assert_eq!(body["priority"], json!("high"));

    // A negative duration is the route's domain 400, not the builder's.
    let body = build_create_task(&json!({ "title": "x", "duration_minutes": -5 })).expect("built");
    assert_eq!(body["duration_minutes"], json!(-5));
}

#[test]
fn build_create_task_requires_a_non_empty_title() {
    for arguments in [
        json!({}),
        json!({ "title": "" }),
        json!({ "title": 3 }),
        json!({ "title": null }),
    ] {
        assert_eq!(
            build_create_task(&arguments),
            Err("title is required".to_string()),
            "{arguments}"
        );
    }
}

#[test]
fn build_create_task_sends_status_to_move_task() {
    assert_eq!(
        build_create_task(&json!({ "title": "x", "status": "PLANNED" })),
        Err("status changes go through move_task".to_string())
    );
    // Presence is the trigger, even with a null value: status is not a field of
    // this tool at all.
    assert_eq!(
        build_create_task(&json!({ "title": "x", "status": null })),
        Err("status changes go through move_task".to_string())
    );
}

#[test]
fn build_create_task_rejects_non_integer_and_unknown_fields() {
    assert_eq!(
        build_create_task(&json!({ "title": "x", "duration_minutes": 1.5 })),
        Err("duration_minutes must be an integer".to_string())
    );
    assert_eq!(
        build_create_task(&json!({ "title": "x", "nope": 1 })),
        Err("unknown field: nope".to_string())
    );
    for field in ["description", "priority", "difficulty"] {
        let mut arguments = serde_json::Map::new();
        arguments.insert("title".to_string(), json!("x"));
        arguments.insert(field.to_string(), json!(1));
        assert_eq!(
            build_create_task(&Value::Object(arguments)),
            Err(format!("{field} must be a string")),
            "{field}"
        );
    }
}

#[test]
fn build_update_task_requires_an_id_and_a_change() {
    assert_eq!(
        build_update_task(&json!({ "id": "t1" })),
        Err("nothing to update".to_string())
    );
    for arguments in [json!({ "title": "x" }), json!({ "id": 7, "title": "x" })] {
        assert_eq!(
            build_update_task(&arguments),
            Err("id is required".to_string()),
            "{arguments}"
        );
    }
}

#[test]
fn build_update_task_rejects_an_unsafe_id() {
    for id in ["../x", "/api/keys", "", "a b"] {
        assert_eq!(
            build_update_task(&json!({ "id": id, "title": "x" })),
            Err("invalid id".to_string()),
            "{id}"
        );
    }
    assert_eq!(
        build_move_task(&json!({ "id": "../x", "status": "PLANNED" })),
        Err("invalid id".to_string())
    );
}

#[test]
fn build_update_task_sends_no_id_and_no_status() {
    let (id, body) =
        build_update_task(&json!({ "id": "t1", "title": "New", "duration_minutes": 20 }))
            .expect("built");
    assert_eq!(id, "t1");
    assert_eq!(keys(&body), vec!["duration_minutes", "title"]);
    assert!(body.get("id").is_none());
    assert_eq!(
        build_update_task(&json!({ "id": "t1", "status": "COMPLETED" })),
        Err("status changes go through move_task".to_string())
    );
}

#[test]
fn build_move_task_sends_status_only() {
    let (id, body) = build_move_task(&json!({ "id": "t1", "status": "PLANNED" })).expect("built");
    assert_eq!(id, "t1");
    assert_eq!(body, json!({ "status": "PLANNED" }));
    assert_eq!(keys(&body), vec!["status"]);
}

#[test]
fn build_move_task_rejects_sort_order_before_the_unknown_scan() {
    assert_eq!(
        build_move_task(&json!({ "id": "t1", "status": "PLANNED", "sort_order": 3 })),
        Err("move_task does not take sort_order".to_string())
    );
    assert_eq!(
        build_move_task(&json!({ "id": "t1", "status": "PLANNED", "nope": 1 })),
        Err("unknown field: nope".to_string())
    );
}

#[test]
fn build_move_task_accepts_exactly_the_five_statuses() {
    for status in ["OPEN", "PLANNED", "IN_PROGRESS", "COMPLETED", "DISCARDED"] {
        let (_, body) = build_move_task(&json!({ "id": "t1", "status": status })).expect("built");
        assert_eq!(body, json!({ "status": status }));
    }
    for status in ["open", "DONE", "", "PLANNED "] {
        assert_eq!(
            build_move_task(&json!({ "id": "t1", "status": status })),
            Err("status must be OPEN, PLANNED, IN_PROGRESS, COMPLETED, or DISCARDED".to_string()),
            "{status}"
        );
    }
    assert_eq!(
        build_move_task(&json!({ "id": "t1" })),
        Err("status must be OPEN, PLANNED, IN_PROGRESS, COMPLETED, or DISCARDED".to_string())
    );
    assert_eq!(
        build_move_task(&json!({ "id": "t1", "status": null })),
        Err("status must be OPEN, PLANNED, IN_PROGRESS, COMPLETED, or DISCARDED".to_string())
    );
}

#[test]
fn build_move_agenda_item_requires_a_non_negative_rank() {
    assert_eq!(
        build_move_agenda_item(&json!({ "id": "i1", "sort_order": 2 })),
        Ok(("i1".to_string(), json!({ "sort_order": 2 })))
    );
    assert_eq!(
        build_move_agenda_item(&json!({ "id": "i1", "sort_order": -1 })),
        Err("sort_order must be >= 0".to_string())
    );
    // Missing, null, and non-integer are one message: there is no append
    // default on this route.
    for arguments in [
        json!({ "id": "i1" }),
        json!({ "id": "i1", "sort_order": null }),
        json!({ "id": "i1", "sort_order": "2" }),
    ] {
        assert_eq!(
            build_move_agenda_item(&arguments),
            Err("sort_order is required".to_string()),
            "{arguments}"
        );
    }
    assert_eq!(
        build_move_agenda_item(&json!({ "id": "i1", "sort_order": 2, "date": "2026-02-01" })),
        Err("unknown field: date".to_string())
    );
}

#[test]
fn build_reschedule_agenda_item_backlog_drops_the_date_and_rank() {
    assert_eq!(
        build_reschedule_agenda_item(&json!({ "id": "i1", "backlog": true, "sort_order": 4 })),
        Ok(("i1".to_string(), json!({ "backlog": true })))
    );
    assert_eq!(
        build_reschedule_agenda_item(&json!({ "id": "i1", "backlog": true, "date": "2026-02-01" })),
        Err("date is omitted when backlog is true".to_string())
    );
    assert_eq!(
        build_reschedule_agenda_item(
            &json!({ "id": "i1", "backlog": "yes", "date": "2026-02-01" })
        ),
        Err("backlog must be a boolean".to_string())
    );
}

#[test]
fn build_reschedule_agenda_item_dated_body_has_no_backlog() {
    assert_eq!(
        build_reschedule_agenda_item(&json!({ "id": "i1", "date": "2026-02-01" })),
        Ok(("i1".to_string(), json!({ "date": "2026-02-01" })))
    );
    let (_, body) =
        build_reschedule_agenda_item(&json!({ "id": "i1", "date": "2026-02-01", "sort_order": 1 }))
            .expect("built");
    assert_eq!(keys(&body), vec!["date", "sort_order"]);
    assert!(body.get("backlog").is_none());

    // `backlog: false` behaves exactly like an absent flag.
    let (_, body) = build_reschedule_agenda_item(
        &json!({ "id": "i1", "date": "2026-02-01", "backlog": false }),
    )
    .expect("built");
    assert_eq!(keys(&body), vec!["date"]);
}

#[test]
fn build_reschedule_agenda_item_requires_a_date_or_backlog() {
    for arguments in [json!({ "id": "i1" }), json!({ "id": "i1", "date": null })] {
        assert_eq!(
            build_reschedule_agenda_item(&arguments),
            Err("date is required, or set backlog true".to_string()),
            "{arguments}"
        );
    }
    // Only the shape is checked: a month that does not exist is the route's 400.
    let (_, body) =
        build_reschedule_agenda_item(&json!({ "id": "i1", "date": "2026-13-01" })).expect("built");
    assert_eq!(body["date"], json!("2026-13-01"));

    for arguments in [
        json!({ "id": "i1", "date": "2026-2-01" }),
        json!({ "id": "i1", "date": 20260201 }),
        json!({ "id": "i1", "date": "2026-02-01T00:00:00Z" }),
    ] {
        assert_eq!(
            build_reschedule_agenda_item(&arguments),
            Err("date must be YYYY-MM-DD".to_string()),
            "{arguments}"
        );
    }
    assert_eq!(
        build_reschedule_agenda_item(
            &json!({ "id": "i1", "date": "2026-02-01", "sort_order": -1 })
        ),
        Err("sort_order must be >= 0".to_string())
    );
    assert_eq!(
        build_reschedule_agenda_item(
            &json!({ "id": "i1", "date": "2026-02-01", "sort_order": 1.5 })
        ),
        Err("sort_order must be an integer".to_string())
    );
    assert_eq!(
        build_reschedule_agenda_item(&json!({ "id": "i1", "date": "2026-02-01", "nope": 1 })),
        Err("unknown field: nope".to_string())
    );
}

#[test]
fn create_event_rejects_task_routine_and_colour_fields() {
    for key in [
        "task_id",
        "routine_id",
        "occurrence_id",
        "color_hex",
        "sanctuary_focus",
        "priority",
        "difficulty",
    ] {
        let mut arguments = event_arguments();
        arguments[key] = json!("x");
        assert_eq!(
            create_event_calendar(&arguments),
            Err(
                "create_event does not accept task, routine, occurrence, or color fields"
                    .to_string()
            ),
            "{key}"
        );
    }
}

#[test]
fn create_event_calendar_reports_whether_a_calendar_was_named() {
    let mut arguments = event_arguments();
    assert_eq!(create_event_calendar(&arguments), Ok(None));
    arguments["calendar_id"] = json!(null);
    assert_eq!(create_event_calendar(&arguments), Ok(None));
    arguments["calendar_id"] = json!("cal1");
    assert_eq!(
        create_event_calendar(&arguments),
        Ok(Some("cal1".to_string()))
    );
    for bad in [json!("../x"), json!(""), json!(7)] {
        arguments["calendar_id"] = bad.clone();
        assert_eq!(
            create_event_calendar(&arguments),
            Err("invalid calendar_id".to_string()),
            "{bad}"
        );
    }
}

#[test]
fn create_event_requires_summary_start_and_end() {
    for key in ["summary", "start", "end"] {
        let mut missing = event_arguments();
        missing[key] = Value::Null;
        assert_eq!(
            create_event_calendar(&missing),
            Err("summary, start, and end are required".to_string()),
            "{key}"
        );
        let mut blank = event_arguments();
        blank[key] = json!("");
        assert_eq!(
            create_event_calendar(&blank),
            Err("summary, start, and end are required".to_string()),
            "{key}"
        );
    }

    let mut arguments = event_arguments();
    arguments["nope"] = json!(1);
    assert_eq!(
        create_event_calendar(&arguments),
        Err("unknown field: nope".to_string())
    );

    let mut arguments = event_arguments();
    arguments["is_all_day"] = json!("yes");
    assert_eq!(
        create_event_calendar(&arguments),
        Err("is_all_day must be a boolean".to_string())
    );
    for field in ["description", "start_time_zone"] {
        let mut arguments = event_arguments();
        arguments[field] = json!(4);
        assert_eq!(
            create_event_calendar(&arguments),
            Err(format!("{field} must be a string")),
            "{field}"
        );
    }
}

#[test]
fn create_event_body_flags_an_explicit_calendar() {
    let arguments = event_arguments();
    let body = create_event_body(&arguments, "primary-1", false).expect("built");
    assert_eq!(body["calendar_id"], json!("primary-1"));
    assert_eq!(body["calendar_id_explicit"], json!(false));
    assert_eq!(
        keys(&body),
        vec![
            "calendar_id",
            "calendar_id_explicit",
            "end",
            "start",
            "summary"
        ]
    );

    let mut explicit = event_arguments();
    explicit["calendar_id"] = json!("cal1");
    let calendar_id = create_event_calendar(&explicit)
        .expect("valid")
        .expect("named calendar");
    let body = create_event_body(&explicit, &calendar_id, true).expect("built");
    assert_eq!(body["calendar_id"], json!("cal1"));
    assert_eq!(body["calendar_id_explicit"], json!(true));
    // The arguments' `calendar_id` key is not copied through as-is.
    assert_eq!(keys(&body).len(), 5);

    let mut optional = event_arguments();
    optional["description"] = json!("notes");
    optional["is_all_day"] = json!(true);
    optional["start_time_zone"] = json!("Asia/Kolkata");
    let body = create_event_body(&optional, "cal1", true).expect("built");
    assert_eq!(body["description"], json!("notes"));
    assert_eq!(body["is_all_day"], json!(true));
    assert_eq!(body["start_time_zone"], json!("Asia/Kolkata"));
    assert!(body.get("task_id").is_none());
}

#[test]
fn primary_calendar_id_picks_the_flagged_row() {
    let body = json!({
        "calendars": [
            { "id": "cal-1", "is_primary": false },
            { "id": "cal-2", "is_primary": true },
            { "id": "cal-3", "is_primary": true },
        ]
    });
    assert_eq!(primary_calendar_id(&body), Ok("cal-2".to_string()));

    // An unsafe primary id is skipped, never interpolated.
    let body = json!({
        "calendars": [
            { "id": "../x", "is_primary": true },
            { "id": "cal-2", "is_primary": true },
        ]
    });
    assert_eq!(primary_calendar_id(&body), Ok("cal-2".to_string()));

    for body in [
        json!({ "calendars": [{ "id": "cal-1", "is_primary": false }] }),
        json!({ "calendars": [{ "id": "../x", "is_primary": true }] }),
        json!({ "calendars": [] }),
        json!({}),
        json!({ "calendars": "nope" }),
    ] {
        assert_eq!(
            primary_calendar_id(&body),
            Err("no primary calendar".to_string()),
            "{body}"
        );
    }
}

// ──────────────────────────────────────────
// Write tools: response projectors
// ──────────────────────────────────────────

#[test]
fn project_task_result_keeps_only_the_slim_row() {
    let projected = project_task_result(&json!({ "task": rest_task("t1") })).expect("projected");
    assert_eq!(keys(&projected), vec!["task"]);
    assert_eq!(projected["task"]["id"], json!("t1"));
    assert!(projected["task"].get("description").is_none());
    assert!(projected["task"].get("user_id").is_none());

    assert_eq!(
        project_task_result(&json!({ "task": null })),
        Err("unexpected task response".to_string())
    );
    assert_eq!(
        project_task_result(&json!({})),
        Err("unexpected task response".to_string())
    );
}

#[test]
fn project_move_result_copies_a_null_event_as_null() {
    let projected =
        project_move_result(&json!({ "task": rest_task("t1"), "event": null })).expect("projected");
    assert_eq!(projected["event"], Value::Null);
    assert_eq!(projected["task"]["id"], json!("t1"));

    // An absent event is the same null.
    let absent = project_move_result(&json!({ "task": rest_task("t1") })).expect("projected");
    assert_eq!(absent["event"], Value::Null);

    // A real event keeps the slim shape; a missing `color` is "", not a failure.
    let mut event = rest_event(0);
    event.as_object_mut().expect("object").remove("color");
    let projected = project_move_result(&json!({ "task": rest_task("t1"), "event": event }))
        .expect("projected");
    assert_eq!(projected["event"]["id"], json!("e0"));
    assert_eq!(projected["event"]["color"], json!(""));

    assert_eq!(
        project_move_result(&json!({ "event": rest_event(0) })),
        Err("unexpected task response".to_string())
    );
}

#[test]
fn project_agenda_item_result_keeps_the_slim_item() {
    let projected =
        project_agenda_item_result(&json!({ "item": task_agenda_item(1) })).expect("projected");
    assert_eq!(keys(&projected), vec!["item"]);
    assert_eq!(projected["item"]["id"], json!("i1"));
    assert_eq!(projected["item"]["task"]["id"], json!("t1"));
    assert!(projected["item"].get("user_id").is_none());

    assert_eq!(
        project_agenda_item_result(&json!({ "item": "nope" })),
        Err("unexpected agenda response".to_string())
    );
    assert_eq!(
        project_agenda_item_result(&json!({})),
        Err("unexpected agenda response".to_string())
    );
}

#[test]
fn project_created_event_drops_sync_and_raw_json() {
    let mut event = rest_event(0);
    event["sync"] = json!({ "state": "healthy" });
    event["raw_json"] = json!({ "google": "payload" });

    let projected =
        project_created_event(&json!({ "event": event, "source": "google" })).expect("projected");
    assert_eq!(keys(&projected), vec!["event", "source"]);
    assert_eq!(projected["source"], json!("google"));
    assert_eq!(projected["event"]["id"], json!("e0"));
    assert!(projected["event"].get("sync").is_none());
    assert!(projected["event"].get("raw_json").is_none());
    assert!(projected["event"].get("google_event_id").is_none());

    let projected = project_created_event(&json!({ "event": rest_event(0) })).expect("projected");
    assert_eq!(projected["source"], json!(""));

    assert_eq!(
        project_created_event(&json!({ "event": null })),
        Err("unexpected event response".to_string())
    );
    assert_eq!(
        project_created_event(&json!({})),
        Err("unexpected event response".to_string())
    );
}

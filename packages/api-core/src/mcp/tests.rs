//! Native tests for the MCP protocol core.
//!
//! `handle_post` is async but there is no tokio here: `pollster::block_on`
//! drives it, exactly like the rest of `api-core`.

use std::sync::Mutex;

use serde_json::{json, Value};

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

//! Stateless MCP JSON-RPC framing (ADR 0009 §2–3).
//!
//! [`handle_post`] takes the raw POST body, the server version string, and a
//! [`ToolExecutor`], and answers with the HTTP status plus an optional JSON
//! body. It performs no I/O and keeps no state: every POST stands alone, so
//! `initialize` is not required before `tools/list` or `tools/call`.
//!
//! Only a parse failure, an invalid request, or a batch is an HTTP 400. A
//! well-formed request that fails is HTTP 200 with a JSON-RPC error. A tool
//! that fails — including an upstream Google 401 — is a *result* with
//! `isError: true` at HTTP 200, never a JSON-RPC error.

use serde_json::{json, Value};

use super::tools::{tool_by_name, tools};

/// `serverInfo.name`: the name every client displays.
pub const SERVER_NAME: &str = "sanctuary";

/// The revision answered when the client asks for an unknown one.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// Accepted `protocolVersion` strings, oldest first.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2025-06-18", "2025-11-25"];

/// A `tools/call` payload whose compact JSON is larger than this becomes an
/// `isError` result with "result too large" (ADR 0009 §8), rather than a
/// truncated blob that is no longer valid JSON.
pub const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;

const JSONRPC_VERSION: &str = "2.0";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const PARSE_ERROR_MESSAGE: &str = "parse error";
const INVALID_REQUEST_MESSAGE: &str = "invalid request";
const BATCH_MESSAGE: &str = "JSON-RPC batches are not supported";
const METHOD_NOT_FOUND_MESSAGE: &str = "method not found";
const UNKNOWN_CURSOR_MESSAGE: &str = "unknown cursor";
const PROTOCOL_VERSION_REQUIRED: &str = "protocolVersion is required";
const TOOL_NAME_REQUIRED: &str = "tool name is required";
const ARGUMENTS_MUST_BE_OBJECT: &str = "arguments must be an object";
const RESULT_TOO_LARGE: &str = "result too large";
const TOOL_CALL_FAILED: &str = "tool call failed";

/// Outcome of a [`ToolExecutor::execute`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolExecution {
    /// Successful payload, echoed as `structuredContent` and as compact JSON
    /// text.
    Ok(Value),
    /// Failure message shown to the agent with `isError: true`.
    Failed(String),
}

/// Runs one tool. The Worker implements this in a later slice by issuing an
/// in-process sub-request to the existing REST router; tests script it.
///
/// `#[async_trait(?Send)]` because wasm futures are `!Send`, like
/// [`HttpClient`](crate::oauth::HttpClient).
#[async_trait::async_trait(?Send)]
pub trait ToolExecutor {
    async fn execute(&self, name: &str, arguments: &Value) -> ToolExecution;
}

/// An HTTP response the Worker sends back verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct McpResponse {
    pub status: u16,
    /// `None` is an empty body (the 202 case); `Some` is JSON text.
    pub body: Option<String>,
}

/// Handles one `POST /api/mcp` body.
///
/// Notifications and client JSON-RPC responses are acknowledged with an empty
/// 202 and never reach `executor`. `server_version` only appears in
/// `initialize`'s `serverInfo.version`; this crate does not read `APP_VERSION`.
pub async fn handle_post(
    body: &str,
    server_version: &str,
    executor: &dyn ToolExecutor,
) -> McpResponse {
    let request: Value = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(_) => return error_body(400, Value::Null, PARSE_ERROR, PARSE_ERROR_MESSAGE),
    };

    if request.is_array() {
        return error_body(400, Value::Null, INVALID_REQUEST, BATCH_MESSAGE);
    }

    let Some(request) = request.as_object() else {
        return invalid_request();
    };

    if request.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
        return invalid_request();
    }

    let Some(method) = request.get("method") else {
        // A client JSON-RPC response: acknowledged, then dropped. There is no
        // session and no server-initiated request to answer.
        if request.contains_key("result") || request.contains_key("error") {
            return accepted();
        }
        return invalid_request();
    };

    let Some(method) = method.as_str() else {
        return invalid_request();
    };

    // No `id` key at all means a notification. `"id": null` is NOT a
    // notification — it is an invalid request, handled below.
    let Some(id) = request.get("id") else {
        return accepted();
    };

    let id = match id {
        Value::String(text) => Value::String(text.clone()),
        Value::Number(number) => Value::Number(number.clone()),
        _ => return invalid_request(),
    };

    let params = request.get("params");
    match method {
        "initialize" => initialize(id, params, server_version),
        "ping" => result_body(id, json!({})),
        "tools/list" => tools_list(id, params),
        "tools/call" => tools_call(id, params, executor).await,
        _ => error_body(200, id, METHOD_NOT_FOUND, METHOD_NOT_FOUND_MESSAGE),
    }
}

/// Version negotiation. An unsupported client version is not an error: the
/// answer is [`LATEST_PROTOCOL_VERSION`], which this server does support.
fn initialize(id: Value, params: Option<&Value>, server_version: &str) -> McpResponse {
    let Some(requested) = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
    else {
        return error_body(200, id, INVALID_PARAMS, PROTOCOL_VERSION_REQUIRED);
    };

    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        LATEST_PROTOCOL_VERSION
    };

    result_body(
        id,
        json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": server_version },
        }),
    )
}

/// One page with every v1 tool and no `nextCursor`. A cursor has nothing to
/// page: an empty or null one means the first page, anything else is unknown.
fn tools_list(id: Value, params: Option<&Value>) -> McpResponse {
    let cursor = match params {
        None | Some(Value::Null) => None,
        Some(Value::Object(params)) => params.get("cursor"),
        Some(_) => return error_body(200, id, INVALID_PARAMS, UNKNOWN_CURSOR_MESSAGE),
    };

    match cursor {
        None | Some(Value::Null) => {}
        Some(Value::String(cursor)) if cursor.is_empty() => {}
        Some(_) => return error_body(200, id, INVALID_PARAMS, UNKNOWN_CURSOR_MESSAGE),
    }

    let entries: Vec<Value> = tools().iter().map(|tool| tool.wire()).collect();
    result_body(id, json!({ "tools": entries }))
}

/// Dispatches a known tool. Unknown names and non-object `arguments` are
/// protocol errors (`-32602`) and never reach the executor; a known tool's
/// failure is a result with `isError: true`.
async fn tools_call(id: Value, params: Option<&Value>, executor: &dyn ToolExecutor) -> McpResponse {
    let params = params.and_then(Value::as_object);

    let Some(name) = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    else {
        return error_body(200, id, INVALID_PARAMS, TOOL_NAME_REQUIRED);
    };

    let Some(tool) = tool_by_name(name) else {
        return error_body(200, id, INVALID_PARAMS, format!("unknown tool: {name}"));
    };

    let arguments = match params.and_then(|params| params.get("arguments")) {
        None => json!({}),
        Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
        Some(_) => return error_body(200, id, INVALID_PARAMS, ARGUMENTS_MUST_BE_OBJECT),
    };

    match executor.execute(tool.name, &arguments).await {
        ToolExecution::Ok(value) => {
            // Compact, exactly what `structuredContent` holds.
            let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
            if text.len() > MAX_TOOL_RESULT_BYTES {
                return tool_error_result(id, RESULT_TOO_LARGE);
            }
            result_body(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "structuredContent": value,
                    "isError": false,
                }),
            )
        }
        ToolExecution::Failed(message) => {
            if message.is_empty() {
                tool_error_result(id, TOOL_CALL_FAILED)
            } else {
                tool_error_result(id, &message)
            }
        }
    }
}

/// A 202 with no body: a notification, or a client JSON-RPC response.
fn accepted() -> McpResponse {
    McpResponse {
        status: 202,
        body: None,
    }
}

fn invalid_request() -> McpResponse {
    error_body(400, Value::Null, INVALID_REQUEST, INVALID_REQUEST_MESSAGE)
}

fn result_body(id: Value, result: Value) -> McpResponse {
    json_response(
        200,
        json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result }),
    )
}

/// A failed tool is a result, never a top-level `error`.
fn tool_error_result(id: Value, message: &str) -> McpResponse {
    result_body(
        id,
        json!({
            "content": [{ "type": "text", "text": message }],
            "isError": true,
        }),
    )
}

fn error_body(status: u16, id: Value, code: i64, message: impl Into<String>) -> McpResponse {
    let message: String = message.into();
    json_response(
        status,
        json!({
            "jsonrpc": JSONRPC_VERSION,
            "id": id,
            "error": { "code": code, "message": message },
        }),
    )
}

fn json_response(status: u16, value: Value) -> McpResponse {
    McpResponse {
        status,
        body: Some(value.to_string()),
    }
}

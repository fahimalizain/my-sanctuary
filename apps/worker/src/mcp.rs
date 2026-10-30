//! `POST /api/mcp` — the stateless MCP endpoint (ADR 0009 §2, §5, §6).
//!
//! Bearer API keys are the only credential. There is no OAuth flow and no
//! session, so a request without an `Authorization` header is refused before
//! `auth::authenticate` could fall back to the session cookie. Every response
//! carries this endpoint's own CORS headers — never the shared
//! `auth::cors_headers`, which must keep `Authorization` off every other route.
//!
//! Tool execution goes back through the REST router as an in-process
//! sub-request (ADR 0009 §4), restricted to [`api_core::mcp::allow_subrequest`].
//! The tool slices land later: until then [`ToolExecutor::execute`] answers
//! "not implemented" and [`RestExecutor::call_rest`] has no caller.

use api_core::mcp::{McpResponse, ToolExecution, ToolExecutor, SUPPORTED_PROTOCOL_VERSIONS};
use worker::*;

/// Largest accepted `POST /api/mcp` body: 256 KiB. A real request is a
/// `tools/call` envelope; anything bigger is refused before parsing.
const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024;

/// Largest sub-request response kept: 1 MiB. A larger one is an error rather
/// than a truncated body that is no longer valid JSON.
const MAX_UPSTREAM_BODY_BYTES: usize = 1024 * 1024;

/// Dummy origin for in-process sub-requests. A web_sys `Request` needs an
/// absolute URL; the router only matches on `Request::path()` and the handlers
/// read the query string from `url()`.
const SUBREQUEST_ORIGIN: &str = "https://sanctuary.internal";

/// `POST /api/mcp`: gates, then one stateless JSON-RPC exchange.
pub(crate) async fn post(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let origin = crate::auth::frontend_url(&ctx).to_string();

    // 1. Origin first: a browser page from anywhere else is refused before
    // authentication. An absent `Origin` (curl, Claude, Inspector) is fine, and
    // the configured frontend origin is echoed — never the request's.
    if let Some(request_origin) = header(&req, "Origin") {
        if request_origin != origin {
            return json_error(&origin, 403, "forbidden");
        }
    }

    // 2. Bearer only. The header's presence is checked before `authenticate`,
    // which treats an absent header as "use the session cookie". A malformed,
    // unknown, expired, or revoked key is the same 401, with no
    // `WWW-Authenticate` to invite an OAuth flow that cannot complete.
    let Some(authorization) = header(&req, "Authorization") else {
        return json_error(&origin, 401, "unauthorized");
    };
    if crate::auth::authenticate(&req, &ctx).await.is_none() {
        return json_error(&origin, 401, "unauthorized");
    }

    // 3. A protocol version header, when sent, must be one this server speaks.
    // Absent is fine: this is stateless and the oldest revision is supported.
    if let Some(version) = header(&req, "MCP-Protocol-Version") {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version.as_str()) {
            return json_error(&origin, 400, "unsupported protocol version");
        }
    }

    // 4. JSON only, optionally with parameters such as `; charset=utf-8`.
    let content_type = header(&req, "Content-Type").unwrap_or_default();
    if !is_json_content_type(&content_type) {
        return json_error(&origin, 415, "unsupported media type");
    }

    // 5. The body is read only after every header check has passed, and is
    // never logged.
    let Ok(body) = req.text().await else {
        return json_error(&origin, 400, "invalid body");
    };
    if body.len() > MAX_REQUEST_BODY_BYTES {
        return json_error(&origin, 400, "body too large");
    }

    let executor = RestExecutor {
        authorization,
        config: ctx.data.clone(),
        env: ctx.env.clone(),
    };
    let response = api_core::mcp::handle_post(&body, env!("APP_VERSION"), &executor).await;
    json_response(&origin, response)
}

/// Preflight for `/api/mcp`. The allow-list is this endpoint's own: the shared
/// CORS helper deliberately does not advertise `Authorization` (ADR 0009 §6).
pub(crate) fn options(
    _req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let headers = cors_headers(crate::auth::frontend_url(&ctx))?;
    headers.set("Access-Control-Allow-Methods", "POST, OPTIONS")?;
    headers.set(
        "Access-Control-Allow-Headers",
        "Authorization, Content-Type, Accept, Mcp-Protocol-Version",
    )?;
    Ok(Response::empty()?.with_status(204).with_headers(headers))
}

/// The [`ToolExecutor`] behind `/api/mcp`.
///
/// It holds what a sub-request needs: the caller's credential, the configuration
/// the router was built with, and the bindings. It never logs the credential.
struct RestExecutor {
    /// The caller's `Authorization` header, copied verbatim onto sub-requests.
    authorization: String,
    config: Option<api_core::Config>,
    env: Env,
}

#[async_trait::async_trait(?Send)]
impl ToolExecutor for RestExecutor {
    async fn execute(&self, _name: &str, _arguments: &serde_json::Value) -> ToolExecution {
        // The read and write slices map a tool name and its validated
        // arguments onto `call_rest`. Until then every known tool fails as a
        // tool result, never as a JSON-RPC error.
        ToolExecution::Failed("not implemented".to_string())
    }
}

impl RestExecutor {
    /// Issues one in-process sub-request through the REST router (ADR 0009 §4)
    /// and returns its status and body text.
    ///
    /// Slices 4 and 5 are the callers; nothing dispatches here yet, which is
    /// why this is allowed to be unused today.
    #[allow(dead_code)]
    pub(crate) async fn call_rest(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<(u16, String), String> {
        let method_name = match method {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            _ => return Err("sub-request refused".to_string()),
        };
        // The allowlist is checked before anything is built, so a refused path
        // never reaches the router.
        if !api_core::mcp::allow_subrequest(method_name, path) {
            return Err("sub-request refused".to_string());
        }

        let mut url = url::Url::parse(&format!("{SUBREQUEST_ORIGIN}{path}"))
            .map_err(|_| "sub-request failed".to_string())?;
        // `append_pair` percent-encodes, so an RFC 3339 `+` or `:` in a value
        // survives the round trip.
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for &(name, value) in query {
                pairs.append_pair(name, value);
            }
        }

        // Only the bearer is copied: no Cookie (the session must not leak into
        // a sub-request), no Origin, no Accept, and no MCP headers.
        let headers = Headers::new();
        headers
            .set("Authorization", &self.authorization)
            .map_err(|_| "sub-request failed".to_string())?;
        if body.is_some() {
            headers
                .set("Content-Type", "application/json")
                .map_err(|_| "sub-request failed".to_string())?;
        }

        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(body) = body {
            init.with_body(Some(js_sys::Uint8Array::from(body).into()));
        }

        let sub_request = match Request::new_with_init(url.as_str(), &init) {
            Ok(sub_request) => sub_request,
            Err(err) => {
                console_log!("mcp: sub-request build failed: {err}");
                return Err("sub-request failed".to_string());
            }
        };

        // `Env` is `Clone` and the config captured at fetch time is cloned
        // too, so a sub-request sees exactly the routes and the Google gating
        // the web app does.
        let mut response = match crate::router(self.config.clone())
            .run(sub_request, self.env.clone())
            .await
        {
            Ok(response) => response,
            Err(err) => {
                console_log!("mcp: sub-request run failed: {err}");
                return Err("sub-request failed".to_string());
            }
        };

        let status = response.status_code();
        match response.text().await {
            Ok(text) if text.len() > MAX_UPSTREAM_BODY_BYTES => {
                Err("upstream body too large".to_string())
            }
            Ok(text) => Ok((status, text)),
            // A body that will not decode is empty: the status still tells the
            // tool what happened.
            Err(_) => Ok((status, String::new())),
        }
    }
}

/// One request header, treating a `Headers::get` failure as absent.
fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().get(name).ok().flatten()
}

/// `application/json`, optionally with parameters. The media type is
/// everything before the first `;`, trimmed and compared ASCII-case-insensitively.
fn is_json_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or_default();
    media_type.trim().eq_ignore_ascii_case("application/json")
}

/// CORS headers present on every `/api/mcp` response (ADR 0009 §6).
///
/// `Content-Type` belongs to the caller: `Response::with_headers` replaces the
/// header map, so a Content-Type set by `Response::from_json` would be lost.
fn cors_headers(origin: &str) -> Result<Headers> {
    let headers = Headers::new();
    headers.set("Access-Control-Allow-Origin", origin)?;
    headers.set("Access-Control-Allow-Credentials", "true")?;
    headers.set("Vary", "Origin")?;
    Ok(headers)
}

/// A JSON `{ "error": … }` with MCP CORS headers and no `WWW-Authenticate`.
fn json_error(origin: &str, status: u16, message: &str) -> Result<Response> {
    json_response(
        origin,
        McpResponse {
            status,
            body: Some(serde_json::json!({ "error": message }).to_string()),
        },
    )
}

/// Sends an [`McpResponse`] verbatim: its exact body text, its status, and
/// `Content-Type` only when there is a body. No `Mcp-Session-Id`.
fn json_response(origin: &str, response: McpResponse) -> Result<Response> {
    let McpResponse { status, body } = response;
    let headers = cors_headers(origin)?;
    let response = match body {
        Some(body) => {
            headers.set("Content-Type", "application/json")?;
            Response::from_body(ResponseBody::Body(body.into_bytes()))?.with_status(status)
        }
        None => Response::empty()?.with_status(status),
    };
    Ok(response.with_headers(headers))
}

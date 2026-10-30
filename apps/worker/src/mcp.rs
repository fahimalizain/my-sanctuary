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
//! All fourteen v1 tools are implemented here: eight reads and six writes.
//! Argument mistakes and upstream failures — including a Google 401 — are
//! failed tool results, never an MCP HTTP error.

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
    async fn execute(&self, name: &str, arguments: &serde_json::Value) -> ToolExecution {
        // Every known tool reaches one method; a bad argument is a failed tool
        // result, never a JSON-RPC error.
        match name {
            "now" => self.now().await,
            "list_tasks" => self.list_tasks().await,
            "get_agenda" => self.get_agenda(arguments).await,
            "list_events" => self.list_events(arguments).await,
            "list_calendars" => self.list_calendars().await,
            "reports_status" => self.reports_status().await,
            "rules_scoreboard" => self.rules_scoreboard().await,
            "classify_title" => self.classify_title(arguments).await,
            "create_task" => self.create_task(arguments).await,
            "update_task" => self.update_task(arguments).await,
            "move_task" => self.move_task(arguments).await,
            "move_agenda_item" => self.move_agenda_item(arguments).await,
            "reschedule_agenda_item" => self.reschedule_agenda_item(arguments).await,
            "create_event" => self.create_event(arguments).await,
            _ => ToolExecution::Failed("not implemented".to_string()),
        }
    }
}

impl RestExecutor {
    /// One `GET` sub-request, mapped onto a tool outcome.
    ///
    /// A transport-level `Err` becomes [`ToolExecution::Failed`] with the same
    /// message; an upstream response goes through
    /// [`api_core::mcp::tool_outcome_from_upstream`] unchanged, so a 401 is the
    /// Google re-auth message and a non-2xx is never projected.
    async fn rest(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> ToolExecution {
        match self.call_rest(method, path, query, body).await {
            Ok((status, text)) => api_core::mcp::tool_outcome_from_upstream(status, &text),
            Err(message) => ToolExecution::Failed(message),
        }
    }

    /// `now`: `/auth/me` first, then `/api/agenda` with no query. A failed
    /// first call short-circuits — there is no point spending a second
    /// sub-request on a user the agenda call cannot identify either.
    async fn now(&self) -> ToolExecution {
        let me = match self.rest(Method::Get, "/auth/me", &[], None).await {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        let agenda = match self.rest(Method::Get, "/api/agenda", &[], None).await {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        projected(api_core::mcp::project_now(&me, &agenda))
    }

    /// `list_tasks`: the whole task list, slimmed and capped.
    async fn list_tasks(&self) -> ToolExecution {
        let body = match self.rest(Method::Get, "/api/tasks", &[], None).await {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        projected(api_core::mcp::project_tasks(&body))
    }

    /// `get_agenda`: `from`/`to` together or neither. A bad pair is a failed
    /// tool result *before* any sub-request.
    async fn get_agenda(&self, arguments: &serde_json::Value) -> ToolExecution {
        let bounds = match api_core::mcp::agenda_bounds(arguments) {
            Ok(bounds) => bounds,
            Err(message) => return ToolExecution::Failed(message),
        };
        let body = match bounds {
            None => self.rest(Method::Get, "/api/agenda", &[], None).await,
            Some((from, to)) => {
                self.rest(
                    Method::Get,
                    "/api/agenda",
                    &[("from", &from), ("to", &to)],
                    None,
                )
                .await
            }
        };
        let body = match body {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        projected(api_core::mcp::project_agenda(&body))
    }

    /// `list_events`: both bounds are required strings, and the window is
    /// validated before the sub-request so an out-of-range span never reaches
    /// the REST route (which has no span cap of its own).
    async fn list_events(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (Some(time_min), Some(time_max)) = (
            arguments.get("time_min").and_then(|value| value.as_str()),
            arguments.get("time_max").and_then(|value| value.as_str()),
        ) else {
            return ToolExecution::Failed("time_min and time_max are required".to_string());
        };
        let (min, max) = match api_core::mcp::validate_event_window(time_min, time_max) {
            Ok(window) => window,
            Err(message) => return ToolExecution::Failed(message),
        };
        let body = match self
            .rest(
                Method::Get,
                "/api/calendar/events",
                &[("time_min", &min), ("time_max", &max)],
                None,
            )
            .await
        {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        projected(api_core::mcp::project_events(&body))
    }

    /// `list_calendars`: the picker rows, slimmed and capped.
    async fn list_calendars(&self) -> ToolExecution {
        let body = match self
            .rest(Method::Get, "/api/calendar/calendars", &[], None)
            .await
        {
            ToolExecution::Ok(value) => value,
            failed => return failed,
        };
        projected(api_core::mcp::project_calendars(&body))
    }

    /// `reports_status`: the status object passed through unchanged.
    async fn reports_status(&self) -> ToolExecution {
        self.object_through("/api/reports/status", "unexpected status response")
            .await
    }

    /// `rules_scoreboard`: the scoreboard object, shrunk to fit the protocol's
    /// result budget (evidence first, then strip, then whole rules).
    async fn rules_scoreboard(&self) -> ToolExecution {
        match self
            .object_through("/api/rules/scoreboard", "unexpected scoreboard response")
            .await
        {
            ToolExecution::Ok(value) => ToolExecution::Ok(api_core::mcp::clamp_scoreboard(value)),
            failed => failed,
        }
    }

    /// `classify_title`: `title` is required (empty is allowed — the REST
    /// handler decides whether a blank title is a 400), `category_id` is an
    /// optional safe id. The classification object passes through unchanged.
    async fn classify_title(&self, arguments: &serde_json::Value) -> ToolExecution {
        let Some(title) = arguments.get("title").and_then(|value| value.as_str()) else {
            return ToolExecution::Failed("title is required".to_string());
        };
        let mut query: Vec<(&str, &str)> = vec![("title", title)];
        match arguments.get("category_id") {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::String(category_id)) => {
                // A lock travels in the query string, so the same id shape the
                // path rules use applies here.
                if !api_core::mcp::is_safe_id(category_id) {
                    return ToolExecution::Failed("invalid category_id".to_string());
                }
                query.push(("category_id", category_id));
            }
            Some(_) => return ToolExecution::Failed("invalid category_id".to_string()),
        }
        match self
            .rest(Method::Get, "/api/tasks/classify", &query, None)
            .await
        {
            ToolExecution::Ok(value) if value.is_object() => ToolExecution::Ok(value),
            ToolExecution::Ok(_) => {
                ToolExecution::Failed("unexpected classify response".to_string())
            }
            failed => failed,
        }
    }

    /// A `GET` whose 2xx body is passed through when it is a JSON object.
    /// `unexpected` is the failure text for anything else, including `null`.
    async fn object_through(&self, path: &str, unexpected: &str) -> ToolExecution {
        match self.rest(Method::Get, path, &[], None).await {
            ToolExecution::Ok(value) if value.is_object() => ToolExecution::Ok(value),
            ToolExecution::Ok(_) => ToolExecution::Failed(unexpected.to_string()),
            failed => failed,
        }
    }

    /// `create_task`: one `POST /api/tasks`. Every call creates a row, so the
    /// tool is not idempotent and nothing here dedupes it.
    async fn create_task(&self, arguments: &serde_json::Value) -> ToolExecution {
        match api_core::mcp::build_create_task(arguments) {
            Ok(body) => {
                self.write(
                    Method::Post,
                    "/api/tasks".to_string(),
                    &body,
                    api_core::mcp::project_task_result,
                )
                .await
            }
            Err(message) => ToolExecution::Failed(message),
        }
    }

    /// `update_task`: `PATCH /api/tasks/:id`. The body never carries a status;
    /// that is `move_task`'s verb (ADR 0009 §8).
    async fn update_task(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (id, body) = match api_core::mcp::build_update_task(arguments) {
            Ok(pair) => pair,
            Err(message) => return ToolExecution::Failed(message),
        };
        let path = match safe_path("/api/tasks/", &id, "") {
            Ok(path) => path,
            Err(message) => return ToolExecution::Failed(message),
        };
        self.write(
            Method::Patch,
            path,
            &body,
            api_core::mcp::project_task_result,
        )
        .await
    }

    /// `move_task`: `POST /api/tasks/:id/move` with `status` only, so the route
    /// applies its own default rank.
    async fn move_task(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (id, body) = match api_core::mcp::build_move_task(arguments) {
            Ok(pair) => pair,
            Err(message) => return ToolExecution::Failed(message),
        };
        let path = match safe_path("/api/tasks/", &id, "/move") {
            Ok(path) => path,
            Err(message) => return ToolExecution::Failed(message),
        };
        self.write(
            Method::Post,
            path,
            &body,
            api_core::mcp::project_move_result,
        )
        .await
    }

    /// `move_agenda_item`: `POST /api/agenda/items/:id/move` with the target
    /// rank.
    async fn move_agenda_item(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (id, body) = match api_core::mcp::build_move_agenda_item(arguments) {
            Ok(pair) => pair,
            Err(message) => return ToolExecution::Failed(message),
        };
        let path = match safe_path("/api/agenda/items/", &id, "/move") {
            Ok(path) => path,
            Err(message) => return ToolExecution::Failed(message),
        };
        self.write(
            Method::Post,
            path,
            &body,
            api_core::mcp::project_agenda_item_result,
        )
        .await
    }

    /// `reschedule_agenda_item`: `POST /api/agenda/items/:id/reschedule` with a
    /// date, `backlog: true`, or `unschedule: true`; never more than one.
    async fn reschedule_agenda_item(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (id, body) = match api_core::mcp::build_reschedule_agenda_item(arguments) {
            Ok(pair) => pair,
            Err(message) => return ToolExecution::Failed(message),
        };
        let path = match safe_path("/api/agenda/items/", &id, "/reschedule") {
            Ok(path) => path,
            Err(message) => return ToolExecution::Failed(message),
        };
        self.write(
            Method::Post,
            path,
            &body,
            api_core::mcp::project_agenda_item_result,
        )
        .await
    }

    /// `create_event`: resolve the primary calendar only when the caller did
    /// not name one, then `POST /api/calendar/events`.
    ///
    /// A failed calendar list short-circuits: posting to an id the tool guessed
    /// would be worse than the failure. The `calendar_id_explicit` flag is
    /// `false` only for that resolved primary, so title classification can
    /// still retarget the event (ADR 0009 §8).
    async fn create_event(&self, arguments: &serde_json::Value) -> ToolExecution {
        let (calendar_id, explicit) = match api_core::mcp::create_event_calendar(arguments) {
            Ok(Some(calendar_id)) => (calendar_id, true),
            Ok(None) => {
                let calendars = match self
                    .rest(Method::Get, "/api/calendar/calendars", &[], None)
                    .await
                {
                    ToolExecution::Ok(value) => value,
                    failed => return failed,
                };
                match api_core::mcp::primary_calendar_id(&calendars) {
                    Ok(calendar_id) => (calendar_id, false),
                    Err(message) => return ToolExecution::Failed(message),
                }
            }
            Err(message) => return ToolExecution::Failed(message),
        };
        let body = match api_core::mcp::create_event_body(arguments, &calendar_id, explicit) {
            Ok(body) => body,
            Err(message) => return ToolExecution::Failed(message),
        };
        self.write(
            Method::Post,
            "/api/calendar/events".to_string(),
            &body,
            api_core::mcp::project_created_event,
        )
        .await
    }

    /// One write sub-request: the JSON body, no query, then `project` over the
    /// 2xx body.
    ///
    /// A [`ToolExecution::Failed`] from [`Self::rest`] is returned unchanged, so
    /// a Google 401 keeps the exact re-auth message `tool_outcome_from_upstream`
    /// produced and never becomes an MCP HTTP 401. The body is never logged.
    async fn write(
        &self,
        method: Method,
        path: String,
        body: &serde_json::Value,
        project: fn(&serde_json::Value) -> Result<serde_json::Value, String>,
    ) -> ToolExecution {
        let bytes = match serde_json::to_vec(body) {
            Ok(bytes) => bytes,
            Err(_) => return ToolExecution::Failed("invalid body".to_string()),
        };
        match self.rest(method, &path, &[], Some(&bytes)).await {
            ToolExecution::Ok(value) => projected(project(&value)),
            failed => failed,
        }
    }

    /// Issues one in-process sub-request through the REST router (ADR 0009 §4)
    /// and returns its status and body text.
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

/// A projector's result as a tool outcome: `Err(message)` is a failed tool
/// result with that exact text, never a JSON-RPC error.
fn projected(result: Result<serde_json::Value, String>) -> ToolExecution {
    match result {
        Ok(value) => ToolExecution::Ok(value),
        Err(message) => ToolExecution::Failed(message),
    }
}

/// Builds `{prefix}{id}{suffix}`, refusing an id that is not a safe path
/// segment.
///
/// The argument builder already checked the id; this re-check is deliberate.
/// Nothing interpolated into a sub-request URL is trusted twice (ADR 0009 §4),
/// and `invalid id` is the message an agent can act on.
fn safe_path(prefix: &str, id: &str, suffix: &str) -> Result<String, String> {
    if !api_core::mcp::is_safe_id(id) {
        return Err("invalid id".to_string());
    }
    Ok(format!("{prefix}{id}{suffix}"))
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

//! Per-user hibernatable Durable Object hub for realtime calendar hints.
//!
//! Browser clients upgrade `GET /api/realtime` (session cookie) onto the
//! user's `UserHub`. After a successful calendar write or webhook sync the
//! Worker POSTs a `RealtimeMessage` to the stub; the DO fans it out to every
//! open socket and hibernates again.
//!
//! No calendar / D1 / Google logic lives here — only connect + notify + DO.

use worker::*;

/// Session-gated WebSocket upgrade → the caller's `UserHub` stub.
///
/// - Missing/invalid session → `401 {"error":"unauthorized"}`.
/// - Missing `Upgrade: websocket` → `426`.
/// - Missing `USER_HUB` binding → logged `500`.
pub async fn connect(req: Request, env: Env, config: Option<&api_core::Config>) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, config) else {
        return Ok(Response::from_json(&serde_json::json!({ "error": "unauthorized" }))?
            .with_status(401));
    };

    let upgrade = req
        .headers()
        .get("Upgrade")?
        .unwrap_or_default();
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Response::error("Expected Upgrade: websocket", 426);
    }

    let namespace = match env.durable_object("USER_HUB") {
        Ok(ns) => ns,
        Err(err) => {
            console_log!("user_hub: USER_HUB binding missing: {err}");
            return Response::error("USER_HUB binding missing", 500);
        }
    };
    let id = match namespace.id_from_name(&user.id) {
        Ok(id) => id,
        Err(err) => {
            console_log!("user_hub: id_from_name failed for {}: {err}", user.id);
            return Response::error("failed to resolve user hub", 500);
        }
    };
    let stub = match id.get_stub() {
        Ok(stub) => stub,
        Err(err) => {
            console_log!("user_hub: get_stub failed for {}: {err}", user.id);
            return Response::error("failed to reach user hub", 500);
        }
    };

    stub.fetch_with_request(req).await
}

/// Fire-and-forget fan-out to every socket on `user_id`'s hub.
///
/// Never fails the caller: missing binding, stub, or fetch is logged and
/// swallowed. This is the **only** notify path — public HTTP must not expose
/// `/notify`.
pub async fn notify_user(env: &Env, user_id: &str, calendar_id: Option<&str>) {
    let Ok(namespace) = env.durable_object("USER_HUB") else {
        console_log!("user_hub: notify skipped — USER_HUB binding missing");
        return;
    };
    let Ok(id) = namespace.id_from_name(user_id) else {
        console_log!("user_hub: notify skipped — id_from_name failed for {user_id}");
        return;
    };
    let Ok(stub) = id.get_stub() else {
        console_log!("user_hub: notify skipped — get_stub failed for {user_id}");
        return;
    };

    let body = match serde_json::to_string(&api_core::RealtimeMessage::calendar_changed(
        calendar_id.map(str::to_string),
    )) {
        Ok(body) => body,
        Err(err) => {
            console_log!("user_hub: notify skipped — serialize failed: {err}");
            return;
        }
    };

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_body(Some(body.into()));
    let req = match Request::new_with_init("https://user-hub/notify", &init) {
        Ok(req) => req,
        Err(err) => {
            console_log!("user_hub: notify skipped — request build failed: {err}");
            return;
        }
    };

    match stub.fetch_with_request(req).await {
        Ok(_) => {
            console_log!("user_hub: notify fetch ok for {user_id}");
        }
        Err(err) => {
            console_log!("user_hub: notify fetch failed for {user_id}: {err}");
        }
    }
}

/// One hibernatable isolate per user (`id_from_name(user_id)`).
///
/// Sockets are accepted via `accept_web_socket` so the DO can sleep while
/// connections stay open. Ping/pong is handled by the runtime auto-response
/// (does not wake the isolate). WS message/close/error handlers are no-ops —
/// trait defaults are `unimplemented!()` and would crash on client traffic.
#[durable_object]
pub struct UserHub {
    state: State,
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for UserHub {
    fn new(state: State, env: Env) -> Self {
        // Heartbeats answered by the runtime — do not wake the isolate.
        // `WebSocketRequestResponsePair::new` returns Result in worker 0.8;
        // a failed pair just means pings wake the isolate (still correct).
        if let Ok(pair) = WebSocketRequestResponsePair::new("ping", "pong") {
            state.set_websocket_auto_response(&pair);
        }
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if req.method() == Method::Post {
            // Internal notify from `notify_user` — broadcast body to all sockets.
            let body = req.text().await.unwrap_or_default();
            let sockets = self.state.get_websockets();
            console_log!("user_hub: notify broadcast sockets={}", sockets.len());
            for ws in sockets {
                let _ = ws.send_with_str(&body);
            }
            return Response::ok("ok");
        }

        // Forwarded WebSocket upgrade from `connect`.
        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        Response::from_websocket(pair.client)
    }

    async fn websocket_message(
        &self,
        _ws: WebSocket,
        _message: WebSocketIncomingMessage,
    ) -> Result<()> {
        Ok(())
    }

    async fn websocket_close(
        &self,
        ws: WebSocket,
        code: usize,
        reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        let _ = ws.close(Some(code as u16), Some(reason.as_str()));
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

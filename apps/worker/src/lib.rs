mod auth;
mod calendar;
mod categories;
mod cron;
mod db;
mod http;
mod lists;
mod tasks;

use worker::*;

fn health(_req: Request, _ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    Response::from_json(&api_core::HealthResponse {
        status: "ok".to_string(),
    })
}

fn version(_req: Request, _ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    Response::from_json(&api_core::VersionResponse {
        version: env!("APP_VERSION").to_string(),
    })
}

/// Loads the configuration from the Worker environment.
///
/// Returns `None` when `SESSION_SECRET` is missing or too short (e.g. an empty
/// `.dev.vars` placeholder), or when `GOOGLE_CREDENTIALS_JSON` is present but
/// invalid. `/health`, `/version` and the logged-out `/auth/*` routes must
/// keep working in that case, so a missing config never fails the Worker.
fn load_config(env: &Env) -> Option<api_core::Config> {
    let session_secret = env.secret("SESSION_SECRET").ok()?.to_string();
    let frontend_url = env.var("FRONTEND_URL").ok().map(|var| var.to_string());
    let secure_cookie = env.var("SECURE_COOKIE").ok().map(|var| var.to_string());
    let google_json = env
        .secret("GOOGLE_CREDENTIALS_JSON")
        .ok()
        .map(|var| var.to_string());
    // A public URL, not a secret: Google needs it in every events.watch body.
    let watch_callback_url = env.var("WATCH_CALLBACK_URL").ok().map(|var| var.to_string());
    api_core::Config::from_env(|key| match key {
        "SESSION_SECRET" => Some(session_secret.clone()),
        "FRONTEND_URL" => frontend_url.clone().filter(|value| !value.is_empty()),
        "SECURE_COOKIE" => secure_cookie.clone(),
        "GOOGLE_CREDENTIALS_JSON" => google_json.clone(),
        "WATCH_CALLBACK_URL" => watch_callback_url.clone().filter(|value| !value.is_empty()),
        _ => None,
    })
    .ok()
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let config = load_config(&env);

    // Google Calendar push webhooks (ADR 0001 § Webhook) are intercepted
    // before the Router: `Router::run` never sees the fetch `Context`, and
    // the background sync after a verified push must run via
    // `ctx.wait_until`. Only a POST to the configured callback path (or the
    // documented default) is a webhook; everything else — including GETs to
    // that path — falls through to the Router.
    if is_webhook_request(&req, config.as_ref()) {
        return calendar::notifications(req, env, ctx).await;
    }

    Router::with_data(config)
        .get("/health", health)
        .get("/version", version)
        .get("/auth/me", auth::me)
        .post("/auth/logout", auth::logout)
        .get("/auth/google", auth::google)
        .get_async("/auth/google/callback", auth::google_callback)
        .options("/auth/me", auth::options)
        .options("/auth/logout", auth::options)
        .options("/auth/google", auth::options)
        .options("/auth/google/callback", auth::options)
        .get_async("/api/calendar/events", calendar::list_events)
        .post_async("/api/calendar/events", calendar::create_event)
        .options("/api/calendar/events", auth::options)
        .get_async("/api/calendar/calendars", calendar::list_calendars)
        .options("/api/calendar/calendars", auth::options)
        .get_async("/api/lists", lists::list_lists)
        .post_async("/api/lists", lists::create_list)
        .patch_async("/api/lists/:id", lists::update_list)
        .delete_async("/api/lists/:id", lists::delete_list)
        .options("/api/lists", auth::options)
        .options("/api/lists/:id", auth::options)
        .get_async("/api/categories", categories::list_categories)
        .post_async("/api/categories", categories::create_category)
        .patch_async("/api/categories/:id", categories::update_category)
        .delete_async("/api/categories/:id", categories::delete_category)
        .options("/api/categories", auth::options)
        .options("/api/categories/:id", auth::options)
        .get_async("/api/tasks", tasks::list_tasks)
        .get_async("/api/tasks/classify", tasks::classify_title)
        .post_async("/api/tasks", tasks::create_task)
        .patch_async("/api/tasks/:id", tasks::update_task)
        .delete_async("/api/tasks/:id", tasks::delete_task)
        .post_async("/api/tasks/:id/start", tasks::start_task)
        .post_async("/api/tasks/:id/stop", tasks::stop_task)
        .post_async("/api/tasks/:id/pause", tasks::pause_task)
        .post_async("/api/tasks/:id/complete", tasks::complete_task)
        .post_async("/api/tasks/:id/discard", tasks::discard_task)
        .post_async("/api/tasks/:id/move", tasks::move_task)
        .post_async("/api/tasks/:id/focus", tasks::focus_task)
        // `/api/focus` is a fixed DELETE (not under `/api/tasks/:id`) — it is
        // registered before any `/:id`-style routes that could swallow it.
        .delete_async("/api/focus", tasks::delete_focus)
        .options("/api/tasks", auth::options)
        .options("/api/tasks/classify", auth::options)
        .options("/api/tasks/:id", auth::options)
        .options("/api/tasks/:id/start", auth::options)
        .options("/api/tasks/:id/stop", auth::options)
        .options("/api/tasks/:id/pause", auth::options)
        .options("/api/tasks/:id/complete", auth::options)
        .options("/api/tasks/:id/discard", auth::options)
        .options("/api/tasks/:id/move", auth::options)
        .options("/api/tasks/:id/focus", auth::options)
        .options("/api/focus", auth::options)
        .run(req, env)
        .await
}

/// Whether `req` is a Google Calendar push notification: a POST whose path
/// matches the `WATCH_CALLBACK_URL` path (when set and parseable) or the
/// documented default `/api/calendar/notifications`.
///
/// Path-only comparison — query strings and fragments are ignored, and the
/// `run_worker_first = ["/api/*"]` asset config already keeps these paths on
/// the Worker.
fn is_webhook_request(req: &Request, config: Option<&api_core::Config>) -> bool {
    if req.method() != Method::Post {
        return false;
    }
    let path = req.path();
    if path == "/api/calendar/notifications" {
        return true;
    }
    config
        .and_then(|config| config.watch_callback_url.as_deref())
        .and_then(|raw| url::Url::parse(raw).ok())
        .is_some_and(|callback| callback.path() == path)
}

mod auth;
mod calendar;
mod db;
mod http;

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
    api_core::Config::from_env(|key| match key {
        "SESSION_SECRET" => Some(session_secret.clone()),
        "FRONTEND_URL" => frontend_url.clone().filter(|value| !value.is_empty()),
        "SECURE_COOKIE" => secure_cookie.clone(),
        "GOOGLE_CREDENTIALS_JSON" => google_json.clone(),
        _ => None,
    })
    .ok()
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let config = load_config(&env);
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
        .run(req, env)
        .await
}

//! `GET /auth/me` and `POST /auth/logout` handlers.
//!
//! Both endpoints are reachable cross-origin when the frontend overrides
//! `VITE_API_BASE_URL`, so they set CORS headers from `FRONTEND_URL`
//! (default `http://localhost:5173`, matching the Vite dev proxy). The
//! same-origin proxy path does not need CORS but is unaffected by it.

use worker::*;

fn frontend_url(ctx: &RouteContext<Option<api_core::Config>>) -> &str {
    ctx.data
        .as_ref()
        .map(|config| config.frontend_url.as_str())
        .unwrap_or(api_core::DEFAULT_FRONTEND_URL)
}

fn cors_headers(origin: &str) -> Result<Headers> {
    let headers = Headers::new();
    headers.set("Access-Control-Allow-Origin", origin)?;
    headers.set("Access-Control-Allow-Credentials", "true")?;
    headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")?;
    headers.set("Access-Control-Allow-Headers", "Accept, Content-Type")?;
    Ok(headers)
}

/// Returns the session user carried by the request, or `None` when the cookie
/// is absent, the secret is missing/invalid, or the cookie fails to unseal
/// (including leftover gorilla cookies). Errors never propagate: a bad cookie
/// is a logged-out request, not a 500.
fn session_user(req: &Request, config: Option<&api_core::Config>) -> Option<api_core::SessionUser> {
    let config = config?;
    let cookie_header = req.headers().get("Cookie").ok().flatten();
    let token = api_core::cookie_value_from_header(
        cookie_header.as_deref(),
        api_core::SESSION_COOKIE_NAME,
    )?;
    // Unix seconds; `Date::now` avoids `SystemTime`, which is unreliable on wasm.
    let now = (worker::Date::now().as_millis() / 1000) as i64;
    api_core::unseal(&config.session_secret, token, now).ok()
}

/// `GET /auth/me` → 200 `{"user":null}` (logged out) or `{"user":{...}}`.
pub fn me(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let user = session_user(&req, ctx.data.as_ref());
    let response = Response::from_json(&api_core::MeResponse { user })?;
    Ok(response.with_headers(cors_headers(frontend_url(&ctx))?))
}

/// `POST /auth/logout` → 200 `{"success":true}` plus a `Set-Cookie` that
/// expires the session cookie.
pub fn logout(_req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let secure = ctx.data.as_ref().map(|c| c.secure_cookie).unwrap_or(false);
    let headers = cors_headers(frontend_url(&ctx))?;
    headers.set("Set-Cookie", &api_core::clear_session_cookie_header(secure))?;
    let response = Response::from_json(&api_core::LogoutResponse { success: true })?;
    Ok(response.with_headers(headers))
}

/// `OPTIONS /auth/*` → 204 with CORS headers (preflight support).
pub fn options(_req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let response = Response::empty()?.with_status(204);
    Ok(response.with_headers(cors_headers(frontend_url(&ctx))?))
}

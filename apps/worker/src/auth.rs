//! `/auth/*` handlers: session `me`/`logout`, plus the Google OAuth login
//! flow (`/auth/google` → consent screen → `/auth/google/callback`).
//!
//! All endpoints are reachable cross-origin when the frontend overrides
//! `VITE_API_BASE_URL`, so they set CORS headers from `FRONTEND_URL`
//! (default `http://localhost:5173`, matching the Vite dev proxy). The
//! same-origin proxy path does not need CORS but is unaffected by it.

use worker::*;

/// Name of the CSRF/state cookie set by `/auth/google` and cleared on callback.
const OAUTH_STATE_COOKIE: &str = "oauth_state";
/// State cookie lifetime in seconds (10 minutes, matching the old Go code).
const OAUTH_STATE_MAX_AGE: i64 = 600;

pub(crate) fn frontend_url(ctx: &RouteContext<Option<api_core::Config>>) -> &str {
    ctx.data
        .as_ref()
        .map(|config| config.frontend_url.as_str())
        .unwrap_or(api_core::DEFAULT_FRONTEND_URL)
}

pub(crate) fn cors_headers(origin: &str) -> Result<Headers> {
    let headers = Headers::new();
    headers.set("Access-Control-Allow-Origin", origin)?;
    headers.set("Access-Control-Allow-Credentials", "true")?;
    headers.set("Access-Control-Allow-Methods", "GET, POST, PATCH, DELETE, OPTIONS")?;
    headers.set("Access-Control-Allow-Headers", "Accept, Content-Type")?;
    Ok(headers)
}

/// Builds a plain-text error response with CORS headers. When `clear_state` is
/// set it also expires the `oauth_state` cookie (used after the state check
/// passes, so a half-finished login never leaves a stale cookie behind).
fn error_response(
    ctx: &RouteContext<Option<api_core::Config>>,
    status: u16,
    body: &str,
    clear_state: bool,
) -> Result<Response> {
    let headers = cors_headers(frontend_url(ctx))?;
    if clear_state {
        headers.set("Set-Cookie", &clear_oauth_state_cookie_header(ctx))?;
    }
    Ok(Response::error(body, status)?.with_headers(headers))
}

/// `Set-Cookie` value for the OAuth state cookie (10-minute lifetime).
fn oauth_state_cookie_header(state: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!(
        "{OAUTH_STATE_COOKIE}={state}; Path=/; HttpOnly; SameSite=Lax; Max-Age={OAUTH_STATE_MAX_AGE}{secure_attr}"
    )
}

/// `Set-Cookie` value that expires the OAuth state cookie.
fn clear_oauth_state_cookie_header(ctx: &RouteContext<Option<api_core::Config>>) -> String {
    let secure = ctx.data.as_ref().map(|c| c.secure_cookie).unwrap_or(false);
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("{OAUTH_STATE_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure_attr}")
}

/// Constant-time string equality for the OAuth state check.
fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Returns the session user carried by the request, or `None` when the cookie
/// is absent, the secret is missing/invalid, or the cookie fails to unseal
/// (including leftover gorilla cookies). Errors never propagate: a bad cookie
/// is a logged-out request, not a 500.
pub(crate) fn session_user(req: &Request, config: Option<&api_core::Config>) -> Option<api_core::SessionUser> {
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

/// `GET /auth/google` → 302 to Google's consent screen.
///
/// Requires `GOOGLE_CREDENTIALS_JSON` to be configured; otherwise responds
/// 500 "oauth not configured" so `/health` and `/auth/me` keep working.
pub fn google(_req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(config) = ctx.data.as_ref() else {
        return error_response(&ctx, 500, "oauth not configured", false);
    };
    let Some(oauth) = config.oauth.as_ref() else {
        return error_response(&ctx, 500, "oauth not configured", false);
    };

    let state = api_core::generate_state();
    let location = api_core::authorization_url(oauth, &state);

    // Note: `Response::redirect` yields a response with an *immutable* header
    // guard (Fetch spec), so Set-Cookie/CORS can't be added. A plain 302 with
    // an explicit `Location` header stays writable.
    let headers = cors_headers(frontend_url(&ctx))?;
    headers.set("Location", &location)?;
    headers.set(
        "Set-Cookie",
        &oauth_state_cookie_header(&state, config.secure_cookie),
    )?;
    let response = Response::empty()?.with_status(302).with_headers(headers);
    Ok(response)
}

/// `GET /auth/google/callback` → exchanges the code, persists user + tokens,
/// seals the session cookie, and 302s to `FRONTEND_URL`.
pub async fn google_callback(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let url = req.url()?;
    let query_state = query_param(&url, "state");
    let code = query_param(&url, "code");

    let cookie_header = req.headers().get("Cookie").ok().flatten();
    let cookie_state =
        api_core::cookie_value_from_header(cookie_header.as_deref(), OAUTH_STATE_COOKIE);

    // CSRF check: the state query parameter must match the state cookie set
    // by `/auth/google` (same origin via the Vite proxy, Path=/).
    let Some(cookie_state) = cookie_state else {
        return error_response(&ctx, 400, "invalid state", false);
    };
    if !ct_eq(cookie_state, query_state.as_deref().unwrap_or("")) {
        return error_response(&ctx, 400, "invalid state", false);
    }
    // State matched: from here on every response also expires the state cookie.
    let state_cleared = error_response(&ctx, 400, "code not found", true);

    let Some(code) = code else {
        return state_cleared;
    };
    if code.is_empty() {
        return state_cleared;
    }

    let Some(config) = ctx.data.as_ref() else {
        return error_response(&ctx, 500, "oauth not configured", true);
    };
    let Some(oauth) = config.oauth.as_ref() else {
        return error_response(&ctx, 500, "oauth not configured", true);
    };
    let users = crate::db::D1UserRepo::new(ctx.d1("DB").map_err(|_| Error::RustError(
        "d1 binding not configured".to_string(),
    ))?);
    let tokens = crate::db::D1TokenRepo::new(ctx.d1("DB").map_err(|_| Error::RustError(
        "d1 binding not configured".to_string(),
    ))?);

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let user = match api_core::exchange_and_login(
        &crate::http::WorkerHttp,
        &users,
        &tokens,
        oauth,
        &code,
        now_unix,
    )
    .await
    {
        Ok(user) => user,
        Err(err) => {
            // Log the detail for the operator; the client gets no internals.
            console_log!("oauth callback failed: {err}");
            return error_response(&ctx, 500, "login failed", true);
        }
    };

    let sealed = match api_core::seal(&config.session_secret, &user, now_unix) {
        Ok(sealed) => sealed,
        Err(err) => {
            console_log!("failed to seal session: {err}");
            return error_response(&ctx, 500, "login failed", true);
        }
    };

    let headers = cors_headers(frontend_url(&ctx))?;
    headers.set("Location", &config.frontend_url)?;
    headers.set("Set-Cookie", &clear_oauth_state_cookie_header(&ctx))?;
    headers.append(
        "Set-Cookie",
        &api_core::session_cookie_header(&sealed, config.secure_cookie),
    )?;
    let response = Response::empty()?.with_status(302).with_headers(headers);
    Ok(response)
}

/// Extracts the first occurrence of a query parameter from a parsed URL.
pub(crate) fn query_param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

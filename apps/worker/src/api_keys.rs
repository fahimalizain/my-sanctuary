//! `/api/keys/*` handlers: create, list, and revoke API keys.
//!
//! Key management is **cookie-session only**. A bearer-only request gets 401
//! by design so a leaked key cannot mint a replacement or revoke its siblings —
//! `session_user` is the only accepted identity here. Bearer authentication on
//! data routes lands in a later slice; nothing in this file reads the
//! `Authorization` header.
//!
//! The plaintext secret is returned by `POST /api/keys` exactly once and never
//! again: only its SHA-256 hash is stored, no handler echoes or logs it, and
//! every response here is `Cache-Control: no-store` because the bodies carry
//! key metadata (and, once, the secret).
//!
//! The orchestration lives in `api_core::api_key` (pure, unit-tested); this
//! file extracts the session user, wires the D1 repos, and maps errors to HTTP
//! responses.
//!
//! Status map: 401 unauthorized, 400 invalid body/name/expiry, 404 unknown,
//! revoked, or other-user keys, 409 too many active keys, 500 logged failures.

use worker::*;

use api_core::ApiKeyError;

/// 401 body for missing/invalid sessions.
fn unauthorized(ctx: &RouteContext<Option<api_core::Config>>) -> Result<Response> {
    json_error(ctx, 401, "unauthorized")
}

/// Builds a JSON `{"error": msg}` response with JSON + CORS headers.
fn json_error(
    ctx: &RouteContext<Option<api_core::Config>>,
    status: u16,
    message: &str,
) -> Result<Response> {
    let headers = crate::auth::json_headers(crate::auth::frontend_url(ctx))?;
    let response = Response::from_json(&serde_json::json!({ "error": message }))?
        .with_status(status)
        .with_headers(headers);
    Ok(response)
}

/// Builds a JSON success response that browsers and proxies must not cache.
fn no_store_json<T: serde::Serialize>(
    ctx: &RouteContext<Option<api_core::Config>>,
    body: &T,
) -> Result<Response> {
    let headers = crate::auth::json_headers(crate::auth::frontend_url(ctx))?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::from_json(body)?.with_headers(headers))
}

/// A fresh D1 handle: `D1Database` is not `Clone`, so each repo gets its own.
fn d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<D1Database> {
    ctx.d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))
}

/// Maps a service error to its HTTP response; `failure` is the logged 500 body.
fn map_error(
    ctx: &RouteContext<Option<api_core::Config>>,
    err: ApiKeyError,
    failure: &str,
) -> Result<Response> {
    match &err {
        ApiKeyError::InvalidName(_) | ApiKeyError::InvalidExpiry | ApiKeyError::PastExpiry => {
            json_error(ctx, 400, &err.to_string())
        }
        ApiKeyError::TooManyKeys => json_error(ctx, 409, "too many active api keys"),
        ApiKeyError::NotFound => json_error(ctx, 404, "api key not found"),
        ApiKeyError::Entropy => {
            console_log!("api_keys: failed to obtain randomness");
            json_error(ctx, 500, failure)
        }
        // The row's hash never reaches a log line.
        ApiKeyError::Repo(err) => {
            console_log!("api_keys: database error: {err}");
            json_error(ctx, 500, failure)
        }
    }
}

/// `GET /api/keys` → 200 `{"keys":[...]}`; never includes a hash or secret.
pub async fn list_keys(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let repo = crate::db::D1ApiKeyRepo::new(d1(&ctx)?);
    match api_core::list_api_keys(&repo, &user.id).await {
        Ok(response) => no_store_json(&ctx, &response),
        Err(err) => map_error(&ctx, err, "failed to load api keys"),
    }
}

/// `POST /api/keys` → 200 `{"key":{...},"secret":"sk_…"}`. Body:
/// `{name, expires_on?}` — the secret is shown only in this response.
pub async fn create_key(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let input: api_core::CreateApiKeyInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let repo = crate::db::D1ApiKeyRepo::new(d1(&ctx)?);
    let digest = crate::db::D1DigestRepo::new(d1(&ctx)?);
    // Unix seconds from the Worker clock; `SystemTime` is unreliable on wasm.
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    // The user's civil "today" (primary calendar zone, UTC fallback) decides
    // what counts as a past expiry and which local midnight ends the day.
    let (today, zone) = match api_core::today(&digest, &user.id, now_unix).await {
        Ok(value) => value,
        Err(err) => {
            console_log!("api_keys: failed to resolve today's date: {err}");
            return json_error(&ctx, 500, "failed to create api key");
        }
    };
    let id = uuid::Uuid::new_v4().to_string();
    match api_core::create_api_key(&repo, &user.id, input, &id, now_unix, &today, &zone).await {
        Ok(response) => no_store_json(&ctx, &response),
        Err(err) => map_error(&ctx, err, "failed to create api key"),
    }
}

/// `DELETE /api/keys/:id` → 200 `{"success":true}`; 404 when the key is
/// unknown, someone else's, or already revoked.
pub async fn revoke_key(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "api key not found");
    };

    let repo = crate::db::D1ApiKeyRepo::new(d1(&ctx)?);
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    match api_core::revoke_api_key(&repo, &user.id, id, &now_rfc3339).await {
        Ok(response) => no_store_json(&ctx, &response),
        Err(err) => map_error(&ctx, err, "failed to revoke api key"),
    }
}

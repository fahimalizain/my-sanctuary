//! `/api/calendar/*` handlers: cached event listing (with Google sync) and
//! event creation, mirroring the old Go `handlers/calendar.go`.
//!
//! Both endpoints are session-gated and refresh the Google access token when
//! stale — a user whose token cannot be refreshed gets `401
//! {"error":"unauthorized"}`, exactly like the Go handlers. The sync/create
//! orchestration lives in `api_core::calendar` (pure, unit-tested); this file
//! only extracts the session user, wires the D1 repos and `WorkerHttp`, and
//! maps errors to HTTP responses.

use worker::*;

use api_core::{models::NewEventInput, CalendarError, OAuthConfig};

/// 401 body for missing/invalid sessions and failed token refreshes.
fn unauthorized(ctx: &RouteContext<Option<api_core::Config>>) -> Result<Response> {
    json_error(ctx, 401, "unauthorized")
}

/// Builds a JSON `{"error": msg}` response with CORS headers.
fn json_error(
    ctx: &RouteContext<Option<api_core::Config>>,
    status: u16,
    message: &str,
) -> Result<Response> {
    let headers = crate::auth::cors_headers(crate::auth::frontend_url(ctx))?;
    let response = Response::from_json(&serde_json::json!({ "error": message }))?
        .with_status(status)
        .with_headers(headers);
    Ok(response)
}

/// Session user id + OAuth config, or a 401 response when unavailable.
fn session_and_oauth<'a>(
    req: &Request,
    ctx: &'a RouteContext<Option<api_core::Config>>,
) -> Result<Option<(String, &'a OAuthConfig)>> {
    let Some(user) = crate::auth::session_user(req, ctx.data.as_ref()) else {
        return Ok(None);
    };
    let Some(config) = ctx.data.as_ref() else {
        return Ok(None);
    };
    let Some(oauth) = config.oauth.as_ref() else {
        return Ok(None);
    };
    Ok(Some((user.id, oauth)))
}

/// `GET /api/calendar/events` → 200 `{"events":[...],"source":"cache"}`.
///
/// Session-gated; refreshes the Google token when stale; syncs each stale
/// sync-enabled calendar (awaited, never fire-and-forget); then serves the
/// overlap window from the D1 cache.
pub async fn list_events(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx)? else {
        return unauthorized(&ctx);
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let tokens = crate::db::D1TokenRepo::new(d1()?);

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let access = match api_core::refresh_if_needed(&crate::http::WorkerHttp, &tokens, oauth, &user_id, now_unix).await {
        Ok(access) => access,
        Err(err) => {
            console_log!("calendar: token refresh failed: {err}");
            return unauthorized(&ctx);
        }
    };

    let url = req.url()?;
    let time_min = crate::auth::query_param(&url, "time_min");
    let time_max = crate::auth::query_param(&url, "time_max");
    let (start, end) = match api_core::parse_event_time_range(
        time_min.as_deref(),
        time_max.as_deref(),
        now_unix,
    ) {
        Ok(range) => range,
        Err(err) => return json_error(&ctx, 400, &err.to_string()),
    };

    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let events = crate::db::D1CalendarEventRepo::new(d1()?);
    let output = match api_core::list_events(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &access,
        &user_id,
        &start,
        &end,
        now_unix,
    )
    .await
    {
        Ok(output) => output,
        Err(err) => {
            console_log!("calendar: list_events failed: {err}");
            return json_error(&ctx, 500, "failed to load events");
        }
    };
    for error in &output.sync_errors {
        console_log!("calendar sync: {error}");
    }

    let response = Response::from_json(&api_core::CalendarEventsResponse {
        events: output.events,
        source: "cache".to_string(),
    })?;
    Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
}

/// `POST /api/calendar/events` → 200 `{"event":{...},"source":"google"}`.
///
/// Body: `{calendar_id, summary, description?, start, end}`. Creates the
/// event on Google, then upserts the returned row into the cache (a cache
/// failure is logged, never fatal).
pub async fn create_event(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx)? else {
        return unauthorized(&ctx);
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let tokens = crate::db::D1TokenRepo::new(d1()?);

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let access = match api_core::refresh_if_needed(&crate::http::WorkerHttp, &tokens, oauth, &user_id, now_unix).await {
        Ok(access) => access,
        Err(err) => {
            console_log!("calendar: token refresh failed: {err}");
            return unauthorized(&ctx);
        }
    };

    let input: NewEventInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let events = crate::db::D1CalendarEventRepo::new(d1()?);
    match api_core::create_event(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &access,
        &input,
        now_unix,
    )
    .await
    {
        Ok(output) => {
            if let Some(error) = &output.cache_error {
                console_log!("calendar: cache upsert failed for created event: {error}");
            }
            let response = Response::from_json(&api_core::CreateEventResponse {
                event: output.event,
                source: output.source,
            })?;
            Ok(response
                .with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(CalendarError::NotFound) => json_error(&ctx, 404, "calendar not found"),
        Err(CalendarError::GoogleApi(message)) => json_error(&ctx, 502, &message),
        Err(CalendarError::GoogleNotFound) => {
            json_error(&ctx, 502, "google returned 404 for events.list")
        }
        Err(err) => {
            console_log!("calendar: create_event failed: {err}");
            json_error(&ctx, 500, "failed to create event")
        }
    }
}

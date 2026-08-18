//! `/api/calendar/*` handlers: cached event listing (with Google sync), event
//! creation, and the Google push-notification webhook, mirroring the old Go
//! `handlers/calendar.go`.
//!
//! The list/create endpoints are session-gated and refresh the Google access
//! token when stale — a user whose token cannot be refreshed gets `401
//! {"error":"unauthorized"}`, exactly like the Go handlers. The webhook is
//! unauthenticated by design (ADR 0001 § Webhook): Google cannot send a
//! session cookie, so verification is the `X-Goog-Channel-*` headers, and
//! every failure is swallowed into a 200.
//!
//! The sync/create/webhook orchestration lives in `api_core::calendar` (pure,
//! unit-tested); this file only extracts the session user (or the webhook
//! headers), wires the D1 repos and `WorkerHttp`, and maps errors to HTTP
//! responses.

use worker::*;

use api_core::repo::{CalendarRepo, WatchChannelRepo};
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
    let watches = crate::db::D1WatchChannelRepo::new(d1()?);
    let watch_callback_url = ctx
        .data
        .as_ref()
        .and_then(|config| config.watch_callback_url.as_deref());
    let output = match api_core::list_events(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &watches,
        &access,
        &user_id,
        &start,
        &end,
        now_unix,
        watch_callback_url,
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

/// `POST <WATCH_CALLBACK_URL path>` — Google Calendar push notification
/// (ADR 0001 § Webhook).
///
/// Always returns 200: Google retries any other status, so verification
/// failures (missing/unknown channel id, missing/bad token, missing or
/// disabled calendar, the `sync` handshake, missing config or D1 binding)
/// are logged and swallowed — never 401/404/500, and never a session or CORS
/// check (Google is not a browser).
///
/// A verified `X-Goog-Resource-State: exists` still returns 200 immediately
/// and runs `sync_calendar` in the background via `ctx.wait_until`, so the
/// response never blocks on Google or D1. Token refresh happens before the
/// 200: if the calendar owner's Google token cannot be refreshed the sync is
/// skipped (logged), and Google still gets its 200.
///
/// Invoked from `fetch` (see `crate::is_webhook_request`) *before* the
/// Router, because `Router::run` never sees the fetch `Context` that
/// `wait_until` needs.
pub async fn notifications(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let headers = req.headers();
    let Some(channel_id) = headers
        .get("X-Goog-Channel-ID")?
        .filter(|id| !id.is_empty())
    else {
        console_log!("calendar webhook: missing X-Goog-Channel-ID — ignoring");
        return Response::empty();
    };
    let presented_token = headers.get("X-Goog-Channel-Token")?;
    let resource_state = headers
        .get("X-Goog-Resource-State")?
        .unwrap_or_default();

    let Some(config) = crate::load_config(&env) else {
        console_log!("calendar webhook: config unavailable — ignoring channel {channel_id}");
        return Response::empty();
    };
    let Some(oauth) = config.oauth.as_ref().cloned() else {
        console_log!("calendar webhook: oauth not configured — ignoring channel {channel_id}");
        return Response::empty();
    };
    let Ok(db) = env.d1("DB") else {
        console_log!("calendar webhook: DB binding missing — ignoring channel {channel_id}");
        return Response::empty();
    };

    let watches = crate::db::D1WatchChannelRepo::new(db);
    let channel = match watches.get_by_channel_id(&channel_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            console_log!("calendar webhook: unknown channel {channel_id} — ignoring");
            return Response::empty();
        }
        Err(err) => {
            console_log!("calendar webhook: channel lookup failed: {err}");
            return Response::empty();
        }
    };

    // A fresh D1 handle per repo: `D1Database` is not Clone in worker 0.8.5,
    // but `Env::d1` returns a new wrapper around the same binding each call.
    let calendars = match env.d1("DB") {
        Ok(db) => crate::db::D1CalendarRepo::new(db),
        Err(err) => {
            console_log!("calendar webhook: DB binding missing: {err}");
            return Response::empty();
        }
    };
    // `get_by_id` filters `deleted_at IS NULL`, so `None` covers soft-deleted
    // calendars (decide_webhook rule 3).
    let calendar = match calendars.get_by_id(&channel.calendar_id).await {
        Ok(calendar) => calendar,
        Err(err) => {
            console_log!(
                "calendar webhook: calendar lookup for {} failed: {err}",
                channel.calendar_id
            );
            return Response::empty();
        }
    };

    let decision = api_core::decide_webhook(
        &resource_state,
        Some(&channel),
        presented_token.as_deref(),
        calendar.as_ref(),
    );
    if let (api_core::WebhookDecision::Sync { .. }, Some(calendar)) = (decision, calendar) {
        // Verified push: refresh the calendar owner's token now (still before
        // the 200). A failed refresh just skips the sync — Google still gets
        // its 200 and will push again on the next change.
        let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
        let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
        let tokens = match env.d1("DB") {
            Ok(db) => crate::db::D1TokenRepo::new(db),
            Err(err) => {
                console_log!("calendar webhook: DB binding missing: {err}");
                return Response::empty();
            }
        };
        let access = match api_core::refresh_if_needed(
            &crate::http::WorkerHttp,
            &tokens,
            &oauth,
            &calendar.user_id,
            now_unix,
        )
        .await
        {
            Ok(access) => access,
            Err(err) => {
                console_log!(
                    "calendar webhook: token refresh for user {} failed: {err}",
                    calendar.user_id
                );
                return Response::empty();
            }
        };

        // The 200 is already decided; run the sync on the fetch Context. The
        // future must be `'static` (worker 0.8 `wait_until`), so it takes
        // owned values and rebuilds its D1 repos from the (Clone) `Env`.
        ctx.wait_until(async move {
            let calendars = match env.d1("DB") {
                Ok(db) => crate::db::D1CalendarRepo::new(db),
                Err(err) => {
                    console_log!(
                        "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                        calendar.id
                    );
                    return;
                }
            };
            let events = match env.d1("DB") {
                Ok(db) => crate::db::D1CalendarEventRepo::new(db),
                Err(err) => {
                    console_log!(
                        "calendar webhook: background sync for {} skipped (DB binding missing): {err}",
                        calendar.id
                    );
                    return;
                }
            };
            if let Err(err) = api_core::sync_calendar(
                &crate::http::WorkerHttp,
                &calendars,
                &events,
                &access,
                &calendar,
                &now_rfc3339,
            )
            .await
            {
                console_log!("calendar webhook: background sync for {} failed: {err}", calendar.id);
            }
        });
    } else {
        console_log!(
            "calendar webhook: ignored channel {channel_id} (state {resource_state:?})"
        );
    }

    Response::empty()
}

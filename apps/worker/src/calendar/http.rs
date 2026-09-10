use worker::*;

use api_core::repo::CalendarRepo;
use api_core::{
    models::NewEventInput, models::PatchEventFields, paint_events_default, paint_events_for_user,
    CalendarError, CalendarEventView, OAuthConfig,
};

/// 401 body for missing/invalid sessions and failed token refreshes.
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

/// Paint cached events with category colors. Taxonomy failures are logged and
/// every event falls back to [`api_core::DEFAULT_EVENT_LABEL_COLOR`] — listing
/// must never 500 because paint failed.
async fn paint_listed_events(
    ctx: &RouteContext<Option<api_core::Config>>,
    user_id: &str,
    events: Vec<api_core::models::CalendarEvent>,
) -> Result<Vec<CalendarEventView>> {
    paint_events_with_fallback(ctx, user_id, events).await
}

async fn paint_single_event(
    ctx: &RouteContext<Option<api_core::Config>>,
    user_id: &str,
    event: api_core::models::CalendarEvent,
) -> Result<CalendarEventView> {
    let mut views = paint_events_with_fallback(ctx, user_id, vec![event]).await?;
    Ok(views
        .pop()
        .expect("paint_events_with_fallback preserves one event"))
}

async fn paint_events_with_fallback(
    ctx: &RouteContext<Option<api_core::Config>>,
    user_id: &str,
    events: Vec<api_core::models::CalendarEvent>,
) -> Result<Vec<CalendarEventView>> {
    let d1 = || {
        ctx.d1("DB")
            .map_err(|_| Error::RustError("d1 binding not configured".to_string()))
    };
    let list_repo = crate::db::D1TaskListRepo::new(d1()?);
    let category_repo = crate::db::D1TaskCategoryRepo::new(d1()?);
    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let cals = match calendars.list_by_user_id(user_id).await {
        Ok(cals) => cals,
        Err(err) => {
            console_log!("calendar: paint calendars load failed: {err}");
            return Ok(paint_events_default(events));
        }
    };
    // Clone so taxonomy failure can still return a default-colored response.
    let fallback = events.clone();
    match paint_events_for_user(&list_repo, &category_repo, &cals, events, user_id).await {
        Ok(views) => Ok(views),
        Err(err) => {
            console_log!("calendar: paint events failed: {err}");
            Ok(paint_events_default(fallback))
        }
    }
}

/// `GET /api/calendar/events` → 200
/// `{"events":[...],"source":"cache"|"window"|"mixed","sync":{...}}`.
///
/// Session-gated; refreshes the Google token when stale; for each
/// never-initialized sync-enabled calendar runs a bounded window fetch
/// (awaited, never fire-and-forget) without waiting on the replica token;
/// then serves the overlap window from the D1 cache (plus ephemeral rows on
/// lease miss) and a sanitized replica-health `sync` envelope. Google auth
/// failures on this path are logged in `sync_errors` — they are **not**
/// mapped to HTTP 401 (401 stays session-only).
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

    let events = paint_listed_events(&ctx, &user_id, output.events).await?;

    let response = Response::from_json(&api_core::CalendarEventsResponse {
        events,
        source: output.source,
        sync: output.sync,
    })?;
    Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
}

/// `GET /api/calendar/calendars` → 200 `{"calendars":[...]}`.
///
/// Session-gated; refreshes the Google token when stale. Serves the user's
/// imported calendars from the D1 cache — no Google calls once the store has
/// rows; an empty store runs the same first-contact `calendarList` import
/// as `GET /api/calendar/events` (no event sync, no watch setup).
///
/// Missing session or a failed token refresh → 401 `{"error":"unauthorized"}`;
/// a `calendarList` import failure → 502 with Google's message; anything else
/// → 500 `{"error":"failed to load calendars"}`.
pub async fn list_calendars(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx)? else {
        return unauthorized(&ctx);
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let tokens = crate::db::D1TokenRepo::new(d1()?);

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    let access = match api_core::refresh_if_needed(&crate::http::WorkerHttp, &tokens, oauth, &user_id, now_unix).await {
        Ok(access) => access,
        Err(err) => {
            console_log!("calendar: token refresh failed: {err}");
            return unauthorized(&ctx);
        }
    };

    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let response = match api_core::list_calendars(
        &crate::http::WorkerHttp,
        &calendars,
        &access,
        &user_id,
        &now_rfc3339,
    )
    .await
    {
            Ok(output) => Response::from_json(&output)?,
            Err(CalendarError::GoogleApi(message)) => return json_error(&ctx, 502, &message),
            Err(err) => {
                console_log!("calendar: list_calendars failed: {err}");
                return json_error(&ctx, 500, "failed to load calendars");
            }
        };
    Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
}

/// `POST /api/calendar/events` → 200 `{"event":{...},"source":"google"}`.
///
/// Body: `{calendar_id, summary, description?, start, end}`. Journals the
/// outbound insert, creates the event on Google, then upserts the returned
/// row into the cache. A cache failure after Google commit is 500
/// (`CalendarError::Repo`) — never 200 with a phantom local id.
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
    let operations = crate::db::D1CalendarEventOperationRepo::new(d1()?);
    match api_core::create_event(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &access,
        &input,
        now_unix,
    )
    .await
    {
        Ok(output) => {
            // create_event never returns Ok with cache_error set (issue #50).
            let event = paint_single_event(&ctx, &user_id, output.event).await?;
            let _ = crate::user_hub::notify_user(
                &ctx.env,
                &user_id,
                Some(&event.event.calendar_id),
            )
            .await;
            let response = Response::from_json(&api_core::CreateEventResponse {
                event,
                source: output.source,
            })?;
            Ok(response
                .with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(CalendarError::NotFound) => json_error(&ctx, 404, "calendar not found"),
        Err(CalendarError::Invalid(message)) => json_error(&ctx, 400, &message),
        Err(CalendarError::GoogleApi(message)) => json_error(&ctx, 502, &message),
        Err(CalendarError::GoogleNotFound) => {
            json_error(&ctx, 502, "google returned 404 for events.list")
        }
        Err(err) => {
            // Includes cache-after-Google Repo failures (journal left
            // google_committed for repair).
            console_log!("calendar: create_event failed: {err}");
            json_error(&ctx, 500, "failed to create event")
        }
    }
}

/// `PATCH /api/calendar/events/:id` → 200 `{"event":{...},"source":"google"}`.
///
/// Body: `{start?, end?, summary?, description?, is_all_day?, start_time_zone?,
/// calendar_id?}` — at least one field required. `calendar_id` is exclusive
/// (local dest calendar id → Google `events.move`); cannot combine with
/// start/end/summary/description/is_all_day/start_time_zone. All-day patches
/// send Google `start.date`/`end.date`; timed patches may include `timeZone`
/// on start/end. Looks up the local event, verifies calendar ownership, then
/// patches or moves on Google.
pub async fn update_event(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx)? else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").map(|s| s.to_string()) else {
        return json_error(&ctx, 404, "event not found");
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

    let fields: PatchEventFields = match req.json().await {
        Ok(fields) => fields,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let events = crate::db::D1CalendarEventRepo::new(d1()?);
    let operations = crate::db::D1CalendarEventOperationRepo::new(d1()?);
    match api_core::update_event_for_user(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &access,
        &user_id,
        &id,
        &fields,
        now_unix,
    )
    .await
    {
        Ok(output) => {
            // Journaled patch never returns Ok with cache_error set (issue #50).
            let event = paint_single_event(&ctx, &user_id, output.event).await?;
            let _ = crate::user_hub::notify_user(
                &ctx.env,
                &user_id,
                Some(&event.event.calendar_id),
            )
            .await;
            let response = Response::from_json(&api_core::CreateEventResponse {
                event,
                source: output.source,
            })?;
            Ok(response
                .with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(CalendarError::NotFound) => json_error(&ctx, 404, "event not found"),
        Err(CalendarError::Invalid(message)) => json_error(&ctx, 400, &message),
        Err(CalendarError::Conflict) => json_error(&ctx, 409, "event write conflict"),
        Err(CalendarError::GoogleApi(message)) => json_error(&ctx, 502, &message),
        Err(CalendarError::GoogleNotFound) => {
            json_error(&ctx, 502, "google returned 404 for events.patch")
        }
        Err(err) => {
            console_log!("calendar: update_event failed: {err}");
            json_error(&ctx, 500, "failed to update event")
        }
    }
}

/// `DELETE /api/calendar/events/:id` → 200 `{"success":true}`.
///
/// Cancels the event on Google (`status: cancelled`) and soft-deletes the
/// local cache row. Ownership is checked via the parent calendar.
pub async fn delete_event(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx)? else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").map(|s| s.to_string()) else {
        return json_error(&ctx, 404, "event not found");
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

    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let events = crate::db::D1CalendarEventRepo::new(d1()?);
    let operations = crate::db::D1CalendarEventOperationRepo::new(d1()?);
    match api_core::delete_event_for_user(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &access,
        &user_id,
        &id,
        now_unix,
    )
    .await
    {
        Ok(()) => {
            let _ = crate::user_hub::notify_user(&ctx.env, &user_id, None).await;
            let response = Response::from_json(&api_core::DeleteEventResponse { success: true })?;
            Ok(response
                .with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(CalendarError::NotFound) => json_error(&ctx, 404, "event not found"),
        Err(CalendarError::Invalid(message)) => json_error(&ctx, 400, &message),
        Err(CalendarError::Conflict) => json_error(&ctx, 409, "event write conflict"),
        Err(CalendarError::GoogleApi(message)) => json_error(&ctx, 502, &message),
        Err(CalendarError::GoogleNotFound) => {
            json_error(&ctx, 502, "google returned 404 for events.patch")
        }
        Err(err) => {
            console_log!("calendar: delete_event failed: {err}");
            json_error(&ctx, 500, "failed to delete event")
        }
    }
}

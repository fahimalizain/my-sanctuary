//! `/api/agenda/*` + `/api/occurrences/*` handlers (ADR 0004).
//!
//! Session-gated via the session cookie or an API key bearer — like
//! `/api/lists/*` and `/api/routines`. The Google-touching verbs gate on a
//! refreshable token:
//! `/start` always, pause/complete/skip **when the occurrence is `in_progress`
//! with stored ids** (load first, like the focused delete), and the title
//! PATCH **when a chip exists** (`google_event_id` set). Everything else is
//! session-only. The orchestration lives in `api_core::agenda` (pure,
//! unit-tested); this file extracts the session user, wires the D1 repos,
//! refreshes tokens, and maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized (missing session, or a Google-touching verb
//! without a refreshable token), 400 invalid input (invalid date, malformed
//! body, negative rank, reschedule into the past / past today+10y / of a
//! running or terminal task, a reschedule that sets both `date` and `backlog`
//! or neither, a backlog drop of a running/terminal task or of an occurrence,
//! move-to-today of a day that is not in the past, empty PATCH body, start on
//! a non-today or terminal occurrence, no writable calendar), 404 "not found"
//! (missing/other-user/soft-deleted task, occurrence, item, or routine —
//! existence is never leaked), 502 Google write failure, 500 logged database
//! errors.

use worker::*;

use api_core::agenda::AgendaError;
use api_core::models::{MoveAgendaItemInput, RescheduleAgendaItemInput, UpdateOccurrence};
use api_core::{GoogleAccess, OccurrenceRepo, OAuthConfig, UserRepo};

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

/// Maps a service error to its HTTP response.
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: AgendaError) -> Result<Response> {
    match err {
        AgendaError::Invalid(message) => json_error(ctx, 400, &message),
        AgendaError::NotFound => json_error(ctx, 404, "not found"),
        AgendaError::GoogleApi(message) => json_error(ctx, 502, &message),
        AgendaError::Calendar(api_core::CalendarError::Conflict) => {
            json_error(ctx, 409, "event write conflict")
        }
        AgendaError::Calendar(err) => {
            console_log!("agenda: calendar error: {err}");
            json_error(ctx, 500, "failed to update occurrence")
        }
        AgendaError::Repo(err) => {
            console_log!("agenda: database error: {err}");
            json_error(ctx, 500, "failed to load agenda")
        }
    }
}

fn agenda_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1AgendaItemRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1AgendaItemRepo::new(db))
}

fn occurrences_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1OccurrenceRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1OccurrenceRepo::new(db))
}

fn routines_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1RoutineRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1RoutineRepo::new(db))
}

fn tasks_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskRepo::new(db))
}

fn categories_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1TaskCategoryRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskCategoryRepo::new(db))
}

fn task_logs_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskLogRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskLogRepo::new(db))
}

fn lists_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskListRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskListRepo::new(db))
}

fn users_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1UserRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1UserRepo::new(db))
}

fn calendars_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1CalendarRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1CalendarRepo::new(db))
}

fn events_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1CalendarEventRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1CalendarEventRepo::new(db))
}

fn operations_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1CalendarEventOperationRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1CalendarEventOperationRepo::new(db))
}

fn tokens_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TokenRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TokenRepo::new(db))
}

/// OAuth config for the Google gate, or `None` — the caller has already
/// authenticated, so a missing config or a missing grant both stay 401.
fn google_access<'a>(ctx: &'a RouteContext<Option<api_core::Config>>) -> Option<&'a OAuthConfig> {
    ctx.data.as_ref()?.oauth.as_ref()
}

/// Refreshes the user's Google access token. `None` when the stored token
/// cannot be refreshed (missing/expired refresh token, Google 400) — callers
/// respond 401 `{"error":"unauthorized"}` exactly like `/api/calendar/*`.
async fn refresh_access(
    tokens: &crate::db::D1TokenRepo,
    oauth: &OAuthConfig,
    user_id: &str,
    now_unix: i64,
) -> Option<GoogleAccess> {
    match api_core::refresh_if_needed(&crate::http::WorkerHttp, tokens, oauth, user_id, now_unix)
        .await
    {
        Ok(access) => Some(access),
        Err(err) => {
            console_log!("agenda: token refresh failed: {err}");
            None
        }
    }
}

/// The user's `focused_task_id` pointer — a read-only paint for embedded
/// `TaskView`s; a missing/soft-deleted user row (or a failed load) just
/// yields `None`, which paints `focused: false` everywhere (same contract as
/// `tasks::list_tasks`).
async fn focused_task_id(
    ctx: &RouteContext<Option<api_core::Config>>,
    user_id: &str,
) -> Option<String> {
    users_d1(ctx)
        .ok()?
        .get_by_id(user_id)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.focused_task_id)
}

/// `GET /api/agenda?from=YYYY-MM-DD&to=YYYY-MM-DD` → 200
/// `{"items":[...],"from":"YYYY-MM-DD","to":"YYYY-MM-DD","today":"YYYY-MM-DD",
/// "time_zone":"IANA","anchor_date":"YYYY-MM-DD","earliest_date":"YYYY-MM-DD",
/// "backlog":[...]}`.
///
/// Both bounds are **optional** and must be set together: omitted reads
/// `anchor_date … today+6` (the oldest past date that still holds a pending
/// item — civil today when nothing pends — through six days after today, with
/// no 31-day cap) plus the `backlog` array of every row past that window. An
/// explicit window is **history only**: at most 31 inclusive days, `to` is
/// clamped to the day before `anchor_date` (the anchor day is on the default
/// read), and a window that lands entirely past the anchor is an empty 200,
/// never an error. `backlog` stays empty. This GET is a
/// **pure range read**: it seeds nothing and writes no occurrence / agenda row
/// (the old `?date=` parameter is ignored). The response carries the
/// server-computed `today` + `time_zone`.
pub async fn get_agenda(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let (from, to) = {
        let url = req.url()?;
        let mut from = None;
        let mut to = None;
        for (key, value) in url.query_pairs() {
            let value = value.into_owned();
            if value.is_empty() {
                continue;
            }
            match key.as_ref() {
                "from" => from = Some(value),
                "to" => to = Some(value),
                _ => {}
            }
        }
        (from, to)
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::get_agenda(
        &calendars_d1(&ctx)?,
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &user.id,
        from.as_deref(),
        to.as_deref(),
        now_unix,
        focused_task_id.as_deref(),
    )
    .await
    {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/agenda/items/:id/move` → 200 `{"item":{...}}`. Body:
/// `{sort_order}` (>= 0, required). Reorders that item's date pile only;
/// same-rank is a 200 no-op.
pub async fn move_agenda_item(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    let input: MoveAgendaItemInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::move_agenda_item(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &user.id,
        id,
        input.sort_order,
        focused_task_id.as_deref(),
    )
    .await
    {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/agenda/items/:id/reschedule` → 200 `{"item":{...}}`. Body:
/// `{date, sort_order?}` (`date` is `YYYY-MM-DD`, today … today+10y-1, and
/// `sort_order` is the optional landing rank) or `{backlog: true}` (drop the
/// task into the backlog; `date` must be absent, `sort_order` is ignored).
/// Relocates the slot to that day — the occurrence moves its agenda item only
/// (no exdate anywhere; `pending | skipped` only, never into the backlog), a
/// task reschedule plans it (`OPEN` → `PLANNED` + a `planned` task_logs row +
/// the Planned column; `PLANNED` stays put; running/terminal → 400), and a
/// backlog drop returns it to `OPEN` on the backlog date (`unplanned` row +
/// the Open column; an already-`OPEN` task writes nothing). Same date is a
/// 200 no-op. Session-only: no Google write of any kind.
pub async fn reschedule_agenda_item(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    let input: RescheduleAgendaItemInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::reschedule_agenda_item(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &task_logs_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &calendars_d1(&ctx)?,
        now_unix,
        &user.id,
        id,
        input.date.as_deref(),
        input.sort_order,
        input.backlog,
        focused_task_id.as_deref(),
    )
    .await
    {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/agenda/days/:date/move-to-today` → 200 `{"items":[...]}`.
///
/// Moves a **past** day's still-pending items (living `OPEN | PLANNED |
/// IN_PROGRESS` tasks and `pending` occurrences of living routines) onto
/// civil today in one call, appended to today's pile in their original
/// order. `done`/`skipped`/`in_progress` occurrences, terminal or
/// soft-deleted tasks, soft-deleted-routine leftovers and orphans stay where
/// they are. Today or a future date → 400. No body, session-only — no Google
/// write of any kind.
pub async fn move_day_to_today(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(date) = ctx.param("date") else {
        return json_error(&ctx, 404, "not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::move_day_to_today(
        &calendars_d1(&ctx)?,
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &user.id,
        date,
        now_unix,
        focused_task_id.as_deref(),
    )
    .await
    {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `PATCH /api/occurrences/:id` → 200 `{"occurrence":{...}}`. Body:
/// `{title?}`. A present title writes the override; `""`/whitespace clears
/// it back to inheritance. Empty body → 400.
///
/// Google gate (slice 6): only when the occurrence **already has a chip**
/// (`google_event_id` set — load first, like the focused delete) does the
/// PATCH also write Google (the chip's `summary` follows the resolved
/// title), so only then is a refreshable token required (401 otherwise). A
/// chip-less occurrence is session-only.
pub async fn patch_occurrence(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    let updates: UpdateOccurrence = match req.json().await {
        Ok(updates) => updates,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let occurrences = occurrences_d1(&ctx)?;
    // The gate: a missing/other-user occurrence is a plain 404 (checked
    // before the Google gate, exactly like the move endpoint's task load).
    let occurrence = match occurrences.get_by_id(id).await {
        Ok(Some(occurrence)) => occurrence,
        Ok(None) => return json_error(&ctx, 404, "not found"),
        Err(err) => return map_error(&ctx, AgendaError::Repo(err)),
    };
    if occurrence.user_id != user.id {
        return json_error(&ctx, 404, "not found");
    }
    let needs_google = occurrence.google_event_id.is_some() && occurrence.calendar_id.is_some();
    let (http, access) = if needs_google {
        let Some(oauth) = google_access(&ctx) else {
            return unauthorized(&ctx);
        };
        let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
        let Some(access) = refresh_access(&tokens_d1(&ctx)?, oauth, &user.id, now_unix).await else {
            return unauthorized(&ctx);
        };
        (
            Some(&crate::http::WorkerHttp as &dyn api_core::HttpClient),
            Some(access),
        )
    } else {
        (None, None)
    };

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let calendars = calendars_d1(&ctx)?;
    let events = events_d1(&ctx)?;
    let operations = operations_d1(&ctx)?;
    let notify = crate::user_hub::UserHubNotify::new(&ctx.env);
    let result = api_core::patch_occurrence(
        http,
        needs_google.then(|| &calendars as &dyn api_core::CalendarRepo),
        needs_google.then(|| &events as &dyn api_core::CalendarEventRepo),
        needs_google.then(|| &operations as &dyn api_core::CalendarEventOperationRepo),
        access.as_ref(),
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences,
        &user.id,
        id,
        &updates,
        now_unix,
        &notify,
    )
    .await;
    match result {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/occurrences/:id/start` → 200 `{"occurrence":{...},"event":{...}}`.
///
/// Creates the one-shot Google log (summary = the resolved title, carriers
/// `sanctuary_routine_id`/`sanctuary_occurrence_id`, never an RRULE), stores
/// the ids on the occurrence, and flips it to `in_progress`. `pending`-only
/// (repeating on `in_progress` is a 200 no-op; `done`/`skipped` → 400), and
/// the occurrence must be **scheduled on civil today** — an agenda item for
/// it sits on today's date (a rescheduled occurrence starts where its item
/// moved, not on its rule date); otherwise 400. Always needs Google —
/// session + refreshable token (401 otherwise), same gate as the task timer.
pub async fn start_occurrence(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(oauth) = google_access(&ctx) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let tokens = tokens_d1(&ctx)?;
    let Some(access) = refresh_access(&tokens, oauth, &user.id, now_unix).await else {
        return unauthorized(&ctx);
    };

    let calendars = calendars_d1(&ctx)?;
    let events = events_d1(&ctx)?;
    let operations = operations_d1(&ctx)?;
    let notify = crate::user_hub::UserHubNotify::new(&ctx.env);
    let result = api_core::start_occurrence(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &operations,
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &access,
        &user.id,
        id,
        now_unix,
        &notify,
    )
    .await;
    match result {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// Shared body of the pause/complete/skip handlers: session-only unless the
/// occurrence is `in_progress` **with** stored ids — then the running chip
/// gets closed and a refreshable token is required (401 otherwise). The
/// occurrence is loaded before the gate (like the focused delete / move), so
/// a missing/other-user occurrence is a plain 404, never a 401.
macro_rules! occurrence_exit {
    ($name:ident, $service:path, $doc:literal) => {
        #[doc = $doc]
        pub async fn $name(
            req: Request,
            ctx: RouteContext<Option<api_core::Config>>,
        ) -> Result<Response> {
            let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
                return unauthorized(&ctx);
            };
            let Some(id) = ctx.param("id") else {
                return json_error(&ctx, 404, "not found");
            };

            let occurrences = occurrences_d1(&ctx)?;
            let occurrence = match occurrences.get_by_id(id).await {
                Ok(Some(occurrence)) => occurrence,
                Ok(None) => return json_error(&ctx, 404, "not found"),
                Err(err) => return map_error(&ctx, AgendaError::Repo(err)),
            };
            // Ownership before the Google gate: another user's occurrence is
            // a plain 404, never a 401 (same rule as the move endpoint).
            if occurrence.user_id != user.id {
                return json_error(&ctx, 404, "not found");
            }
            let needs_google = occurrence.status == api_core::OCCURRENCE_STATUS_IN_PROGRESS
                && occurrence.google_event_id.is_some()
                && occurrence.calendar_id.is_some();
            let (http, access) = if needs_google {
                let Some(oauth) = google_access(&ctx) else {
                    return unauthorized(&ctx);
                };
                let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
                let Some(access) =
                    refresh_access(&tokens_d1(&ctx)?, oauth, &user.id, now_unix).await
                else {
                    return unauthorized(&ctx);
                };
                (
                    Some(&crate::http::WorkerHttp as &dyn api_core::HttpClient),
                    Some(access),
                )
            } else {
                (None, None)
            };

            let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
            let calendars = calendars_d1(&ctx)?;
            let events = events_d1(&ctx)?;
            let operations = operations_d1(&ctx)?;
            let notify = crate::user_hub::UserHubNotify::new(&ctx.env);
            let result = $service(
                http,
                needs_google.then(|| &calendars as &dyn api_core::CalendarRepo),
                needs_google.then(|| &events as &dyn api_core::CalendarEventRepo),
                needs_google.then(|| &operations as &dyn api_core::CalendarEventOperationRepo),
                access.as_ref(),
                &lists_d1(&ctx)?,
                &categories_d1(&ctx)?,
                &routines_d1(&ctx)?,
                &occurrences,
                &user.id,
                id,
                now_unix,
                &notify,
            )
            .await;
            match result {
                Ok(response) => {
                    let response = Response::from_json(&response)?;
                    Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
                }
                Err(err) => map_error(&ctx, err),
            }
        }
    };
}

occurrence_exit!(
    pause_occurrence,
    api_core::pause_occurrence,
    "POST /api/occurrences/:id/pause → 200 `{\"occurrence\":{...}}` — `in_progress` → `pending` after the chip close (Google gate when ids are stored), then ids cleared; `pending` → 200 no-op; `done`/`skipped` → 400."
);

occurrence_exit!(
    complete_occurrence,
    api_core::complete_occurrence,
    "POST /api/occurrences/:id/complete → 200 `{\"occurrence\":{...}}` — the verb matrix; from `in_progress` with stored ids the running chip's end is PATCHed closed (Google gate), otherwise session-only."
);

occurrence_exit!(
    skip_occurrence,
    api_core::skip_occurrence,
    "POST /api/occurrences/:id/skip → 200 `{\"occurrence\":{...}}` — the verb matrix; from `in_progress` with stored ids the running chip's end is PATCHed closed (Google gate), otherwise session-only."
);

/// `POST /api/occurrences/:id/reopen` → 200 `{"occurrence":{...}}`.
///
/// The dedicated un-do verb (ADR 0004 amendment): `done`/`skipped` →
/// `pending` with the stored one-shot log ids cleared, so a later start mints
/// a NEW Google event (the closed chip stays on Google as an orphaned log,
/// never PATCHed or deleted). `pending` → 200 no-op; `in_progress` → 400 (no
/// abort via reopen — pause is the Google-writing exit). Session-only —
/// never touches Google, so no token refresh here.
/// Deliberately NOT the `occurrence_exit!` macro: that one Google-gates
/// `in_progress`; reopen never writes Google.
pub async fn reopen_occurrence(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    match api_core::reopen_occurrence(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &user.id,
        id,
    )
    .await
    {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}
//! `/api/agenda/*` + `/api/occurrences/*` handlers (ADR 0004).
//!
//! Session-gated via the session cookie — like `/api/lists/*` and
//! `/api/routines`. The Google-touching verbs gate on a refreshable token:
//! `/start` always, complete/skip **when the occurrence is `in_progress`
//! with stored ids** (load first, like the focused delete), and the title
//! PATCH **when a chip exists** (`google_event_id` set). Everything else is
//! session-only. The orchestration lives in `api_core::agenda` (pure,
//! unit-tested); this file extracts the session user, wires the D1 repos,
//! refreshes tokens, and maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized (missing session, or a Google-touching verb
//! without a refreshable token), 400 invalid input (missing/invalid date,
//! malformed body, non-task kind, terminal/foreign task, DELETE on an
//! occurrence item, empty PATCH body, negative rank, start on a non-today or
//! terminal occurrence, reschedule of an in_progress/done occurrence or onto
//! a date already holding the routine, no writable calendar), 404 "not found"
//! (missing/other-user/soft-deleted task, occurrence, item, or routine —
//! existence is never leaked), 502 Google write failure, 500 logged database
//! errors.

use worker::*;

use api_core::agenda::AgendaError;
use api_core::models::{
    MoveAgendaItemInput, NewAgendaItemInput, RescheduleAgendaItemInput, UpdateOccurrence,
};
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

fn tokens_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TokenRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TokenRepo::new(db))
}

/// Session user id + OAuth config, or `None` — the Google gate's session
/// check (missing session or missing OAuth config both mean 401).
fn session_and_oauth<'a>(
    req: &Request,
    ctx: &'a RouteContext<Option<api_core::Config>>,
) -> Option<(String, &'a OAuthConfig)> {
    let user = crate::auth::session_user(req, ctx.data.as_ref())?;
    let config = ctx.data.as_ref()?;
    let oauth = config.oauth.as_ref()?;
    Some((user.id, oauth))
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

/// `GET /api/agenda?date=YYYY-MM-DD` → 200 `{"items":[...]}`.
///
/// Seeds on read (living routines whose rule covers the date get their
/// occurrence + membership ensured), then returns the mixed pile with
/// embedded task views / occurrence views.
pub async fn get_agenda(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let date = {
        let url = req.url()?;
        url.query_pairs()
            .find(|(key, _)| key == "date")
            .map(|(_, value)| value.into_owned())
            .unwrap_or_default()
    };
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::get_agenda(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &user.id,
        &date,
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

/// `POST /api/agenda/items` → 200 `{"item":{...}}`. Body:
/// `{kind, ref_id, sort_order?, date}` — the `date` is REQUIRED (the ADR
/// table omits it because GET is date-scoped, but the POST must name it).
/// Tasks only in v1; duplicate adds are idempotent 200s with the existing
/// item.
pub async fn add_agenda_item(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let input: NewAgendaItemInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::add_agenda_item(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &user.id,
        &input,
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
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
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

/// `DELETE /api/agenda/items/:id` → 200 `{"success":true}`. Unpins a task
/// item (hard delete; the task stays on the Board). Occurrence-kind items
/// are 400 — skip is the decline.
pub async fn delete_agenda_item(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    match api_core::delete_agenda_item(&agenda_d1(&ctx)?, &user.id, id).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/agenda/items/:id/reschedule` → 200 `{"item":{...}}`. Body:
/// `{date}` (`YYYY-MM-DD`, required). Relocates the slot to that day —
/// occurrences exdate their source date on the routine and move
/// (`pending | skipped` only), tasks move the membership slot only (task
/// status unchanged, no `task_logs` row). Session-only: no Google write of
/// any kind.
pub async fn reschedule_agenda_item(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    let input: RescheduleAgendaItemInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    let focused_task_id = focused_task_id(&ctx, &user.id).await;

    match api_core::reschedule_agenda_item(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &tasks_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &user.id,
        id,
        &input.date,
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
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
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
        let Some((user_id, oauth)) = session_and_oauth(&req, &ctx) else {
            return unauthorized(&ctx);
        };
        let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
        let Some(access) = refresh_access(&tokens_d1(&ctx)?, oauth, &user_id, now_unix).await else {
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
    let result = api_core::patch_occurrence(
        http,
        needs_google.then(|| &calendars as &dyn api_core::CalendarRepo),
        needs_google.then(|| &events as &dyn api_core::CalendarEventRepo),
        access.as_ref(),
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences,
        &user.id,
        id,
        &updates,
        now_unix,
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
    let Some((user_id, oauth)) = session_and_oauth(&req, &ctx) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let tokens = tokens_d1(&ctx)?;
    let Some(access) = refresh_access(&tokens, oauth, &user_id, now_unix).await else {
        return unauthorized(&ctx);
    };

    let calendars = calendars_d1(&ctx)?;
    let events = events_d1(&ctx)?;
    let result = api_core::start_occurrence(
        &crate::http::WorkerHttp,
        &calendars,
        &events,
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &agenda_d1(&ctx)?,
        &access,
        &user_id,
        id,
        now_unix,
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

/// Shared body of the complete/skip handlers: session-only unless the
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
            let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
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
                let Some((user_id, oauth)) = session_and_oauth(&req, &ctx) else {
                    return unauthorized(&ctx);
                };
                let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
                let Some(access) =
                    refresh_access(&tokens_d1(&ctx)?, oauth, &user_id, now_unix).await
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
            let result = $service(
                http,
                needs_google.then(|| &calendars as &dyn api_core::CalendarRepo),
                needs_google.then(|| &events as &dyn api_core::CalendarEventRepo),
                access.as_ref(),
                &lists_d1(&ctx)?,
                &categories_d1(&ctx)?,
                &routines_d1(&ctx)?,
                &occurrences,
                &user.id,
                id,
                now_unix,
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
    complete_occurrence,
    api_core::complete_occurrence,
    "POST /api/occurrences/:id/complete → 200 `{\"occurrence\":{...}}` — the verb matrix; from `in_progress` with stored ids the running chip's end is PATCHed closed (Google gate), otherwise session-only."
);

occurrence_exit!(
    skip_occurrence,
    api_core::skip_occurrence,
    "POST /api/occurrences/:id/skip → 200 `{\"occurrence\":{...}}` — the verb matrix; from `in_progress` with stored ids the running chip's end is PATCHed closed (Google gate), otherwise session-only."
);
//! `/api/tasks/*` handlers: task CRUD (classified by title regex) plus the
//! timer: start/stop/pause/complete/discard backed by Google Calendar events.
//!
//! CRUD is session-gated via the session cookie only — like `/api/lists/*`
//! and `/api/categories/*`. The timer actions additionally refresh the Google
//! access token (like `/api/calendar/*`): a user whose token cannot be
//! refreshed gets `401 {"error":"unauthorized"}`.
//!
//! The orchestration lives in `api_core::tasks` (pure, unit-tested); this
//! file extracts the session user, wires the D1 repos, refreshes the OAuth
//! token for the timer, and maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid input (title/category rules, bad
//! duration or priority, empty PATCH body, terminal-task transitions, no
//! writable calendar), 404 not found (missing/soft-deleted/other-user task),
//! 409 one-running-task conflict, 502 Google API failures, 500 logged
//! database errors.

use worker::*;

use api_core::models::{NewTaskInput, UpdateTask};
use api_core::tasks::TasksError;

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

/// Maps a service error to its HTTP response.
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: TasksError) -> Result<Response> {
    match err {
        TasksError::Invalid(message) => json_error(ctx, 400, &message),
        TasksError::NotFound => json_error(ctx, 404, "task not found"),
        TasksError::Conflict => json_error(ctx, 409, "a task is already running"),
        TasksError::GoogleApi(message) => json_error(ctx, 502, &message),
        TasksError::Repo(err) => {
            console_log!("tasks: database error: {err}");
            json_error(ctx, 500, "failed to load tasks")
        }
        TasksError::Calendar(api_core::CalendarError::GoogleApi(message)) => {
            json_error(ctx, 502, &message)
        }
        TasksError::Calendar(err) => {
            console_log!("tasks: calendar error: {err}");
            json_error(ctx, 500, "failed to update task")
        }
    }
}

/// Session user id + OAuth config, or a `None` 401 signal — the timer's
/// session gate (same shape as `crate::calendar::session_and_oauth`).
fn session_and_oauth<'a>(
    req: &Request,
    ctx: &'a RouteContext<Option<api_core::Config>>,
) -> Result<Option<(String, &'a api_core::OAuthConfig)>> {
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

/// Refreshes the user's Google access token. `Ok(None)` when the stored token
/// cannot be refreshed (missing/expired refresh token, Google 400) — callers
/// respond `401 {"error":"unauthorized"}` exactly like `/api/calendar/*`.
async fn refresh_access(
    tokens: &crate::db::D1TokenRepo,
    oauth: &api_core::OAuthConfig,
    user_id: &str,
    now_unix: i64,
) -> Option<api_core::GoogleAccess> {
    match api_core::refresh_if_needed(&crate::http::WorkerHttp, tokens, oauth, user_id, now_unix)
        .await
    {
        Ok(access) => Some(access),
        Err(err) => {
            console_log!("tasks: token refresh failed: {err}");
            None
        }
    }
}

fn d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskRepo> {
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

/// `GET /api/tasks` → 200 `{"tasks":[...]}`; each task carries its computed
/// `category` (the client never reimplements the matcher). Seeds the taxonomy
/// (count-gated) so a tasks-only first visitor still has categories.
pub async fn list_tasks(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    match api_core::list_tasks(&lists_d1(&ctx)?, &categories_d1(&ctx)?, &d1(&ctx)?, &user.id).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/tasks` → 200 `{"task":{...}}`. Body: `{title, description?,
/// duration_minutes?, priority?, difficulty?}`. The title must uniquely match
/// a non-untracked category (400 otherwise, with the reason in `error`).
pub async fn create_task(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let input: NewTaskInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    match api_core::create_task(&lists_d1(&ctx)?, &categories_d1(&ctx)?, &d1(&ctx)?, &user.id, &input).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `PATCH /api/tasks/:id` → 200 `{"task":{...}}`. Body: `UpdateTask` — every
/// field optional; a present `title` must uniquely match a non-untracked
/// category (400 otherwise). Status is never updatable this slice.
pub async fn update_task(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "task not found");
    };
    let updates: UpdateTask = match req.json().await {
        Ok(updates) => updates,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    match api_core::update_task(&categories_d1(&ctx)?, &d1(&ctx)?, &user.id, id, &updates).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `DELETE /api/tasks/:id` → 200 `{"success":true}` (soft delete; 404 for
/// missing/other-user tasks).
pub async fn delete_task(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "task not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    match api_core::delete_task(&d1(&ctx)?, &user.id, id, &now_rfc3339).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

// ──────────────────────────────────────────
// Timer actions (Google OAuth-gated)
// ──────────────────────────────────────────

/// D1 handles for one timer action. Every repo is constructed from a fresh
/// `ctx.d1("DB")` wrapper (D1Database is not Clone in worker 0.8.5).
struct TimerRepos {
    tasks: crate::db::D1TaskRepo,
    categories: crate::db::D1TaskCategoryRepo,
    lists: crate::db::D1TaskListRepo,
    calendars: crate::db::D1CalendarRepo,
    events: crate::db::D1CalendarEventRepo,
    logs: crate::db::D1TaskLogRepo,
    tokens: crate::db::D1TokenRepo,
}

fn timer_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<TimerRepos> {
    let db = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    Ok(TimerRepos {
        tasks: crate::db::D1TaskRepo::new(db()?),
        categories: crate::db::D1TaskCategoryRepo::new(db()?),
        lists: crate::db::D1TaskListRepo::new(db()?),
        calendars: crate::db::D1CalendarRepo::new(db()?),
        events: crate::db::D1CalendarEventRepo::new(db()?),
        logs: crate::db::D1TaskLogRepo::new(db()?),
        tokens: crate::db::D1TokenRepo::new(db()?),
    })
}

/// Shared session + token-refresh gate for the five timer actions. The inner
/// `Err` carries the already-built `401` response (missing session, missing
/// OAuth config, or an unrefreshable token).
async fn timer_access(
    req: &Request,
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<Result<(String, api_core::GoogleAccess), Response>> {
    let Some((user_id, oauth)) = session_and_oauth(req, ctx)? else {
        return Ok(Err(unauthorized(ctx)?));
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let repos = timer_d1(ctx)?;
    let Some(access) = refresh_access(&repos.tokens, oauth, &user_id, now_unix).await else {
        return Ok(Err(unauthorized(ctx)?));
    };
    Ok(Ok((user_id, access)))
}

/// POST /api/tasks/:id/start → 200 `{"task":{...},"event":{...}}`.
///
/// Opens a Google Calendar event now → now + duration (summary = task title,
/// `extendedProperties.shared.sanctuary_task_id` = task UUID) and marks the
/// task IN_PROGRESS. 409 when another task is already running; 400 on
/// COMPLETED/DISCARDED tasks or a missing writable calendar.
pub async fn start_task(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let (user_id, access) = match timer_access(&req, &ctx).await? {
        Ok(gated) => gated,
        Err(response) => return Ok(response),
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "task not found");
    };
    let repos = timer_d1(&ctx)?;
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let result = api_core::start_task(
        &crate::http::WorkerHttp,
        &repos.calendars,
        &repos.events,
        &repos.lists,
        &repos.categories,
        &repos.tasks,
        &repos.logs,
        &access,
        &user_id,
        id,
        now_unix,
    )
    .await;
    respond_action(&ctx, result)
}

/// Runs one stop/pause/complete/discard handler body (all five share the
/// session gate, the id param, and the response serialization; `start` is
/// spelled out above because it takes the extra list repo).
macro_rules! timer_action {
    ($name:ident, $path:path, $doc:literal) => {
        #[doc = $doc]
        pub async fn $name(
            req: Request,
            ctx: RouteContext<Option<api_core::Config>>,
        ) -> Result<Response> {
            let (user_id, access) = match timer_access(&req, &ctx).await? {
                Ok(gated) => gated,
                Err(response) => return Ok(response),
            };
            let Some(id) = ctx.param("id") else {
                return json_error(&ctx, 404, "task not found");
            };
            let repos = timer_d1(&ctx)?;
            let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
            let result = $path(
                &crate::http::WorkerHttp,
                &repos.calendars,
                &repos.events,
                &repos.categories,
                &repos.tasks,
                &repos.logs,
                &access,
                &user_id,
                id,
                now_unix,
            )
            .await;
            respond_action(&ctx, result)
        }
    };
}

timer_action!(
    stop_task,
    api_core::stop_task,
    "POST /api/tasks/:id/stop → 200 `{\"task\":{...},\"event\":{...}|null}`. Patches the running event's end to now and flips the task back to OPEN (idempotent when the event is already closed in Google)."
);

timer_action!(
    pause_task,
    api_core::pause_task,
    "POST /api/tasks/:id/pause → 200 `{\"task\":{...},\"event\":{...}|null}`. Same event close as stop; the log row says `paused`."
);

timer_action!(
    complete_task,
    api_core::complete_task,
    "POST /api/tasks/:id/complete → 200 `{\"task\":{...},\"event\":{...}|null}`. Auto-stops a running event first (`stopped` then `completed` logs); idempotent 200 for already COMPLETED tasks."
);

timer_action!(
    discard_task,
    api_core::discard_task,
    "POST /api/tasks/:id/discard → 200 `{\"task\":{...},\"event\":{...}|null}`. Auto-stops a running event first (`stopped` then `discarded` logs); idempotent 200 for already DISCARDED tasks."
);

/// Serializes a `TaskActionResponse` (or maps the error).
fn respond_action(
    ctx: &RouteContext<Option<api_core::Config>>,
    result: Result<api_core::TaskActionResponse, TasksError>,
) -> Result<Response> {
    match result {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(ctx))?))
        }
        Err(err) => map_error(ctx, err),
    }
}

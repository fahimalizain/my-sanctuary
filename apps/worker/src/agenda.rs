//! `/api/agenda/*` + `/api/occurrences/*` handlers (ADR 0004, slice 4).
//!
//! Session-gated via the session cookie only — like `/api/lists/*` and
//! `/api/routines`; **no Google this slice** (no token refresh, no event
//! writes — occurrence start and the title-PATCH Google write land in slice
//! 6). The orchestration lives in `api_core::agenda` (pure, unit-tested);
//! this file extracts the session user, wires the D1 repos, and maps errors
//! to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid input (missing/invalid date,
//! malformed body, non-task kind, terminal/foreign task, DELETE on an
//! occurrence item, empty PATCH body, negative rank), 404 "not found"
//! (missing/other-user/soft-deleted task, occurrence, item, or routine —
//! existence is never leaked), 500 logged database errors. No 502 this
//! slice: nothing here writes Google.

use worker::*;

use api_core::agenda::AgendaError;
use api_core::models::{MoveAgendaItemInput, NewAgendaItemInput, UpdateOccurrence};
use api_core::UserRepo;

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

/// `PATCH /api/occurrences/:id` → 200 `{"occurrence":{...}}`. Body:
/// `{title?}`. A present title writes the override; `""`/whitespace clears
/// it back to inheritance. Empty body → 400. No Google write this slice.
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

    match api_core::patch_occurrence(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &occurrences_d1(&ctx)?,
        &user.id,
        id,
        &updates,
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

/// `POST /api/occurrences/:id/complete` → 200 `{"occurrence":{...}}` — the
/// verb matrix without Google (done is a 200 no-op; slice 6 closes a running
/// chip).
pub async fn complete_occurrence(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    match api_core::complete_occurrence(
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

/// `POST /api/occurrences/:id/skip` → 200 `{"occurrence":{...}}` — the verb
/// matrix without Google (skipped is a 200 no-op).
pub async fn skip_occurrence(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "not found");
    };

    match api_core::skip_occurrence(
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
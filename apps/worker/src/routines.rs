//! `/api/routines/*` handlers: standing-routine CRUD (ADR 0004).
//!
//! Session-gated via the session cookie only — like `/api/lists/*` and
//! `/api/tasks` CRUD; routine endpoints never touch Google (no token refresh,
//! no RRULE ever leaves Sanctuary). The orchestration lives in
//! `api_core::routines` (pure, unit-tested); this file extracts the session
//! user, wires the D1 repos, and maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid input (title/category rules,
//! `estimated_minutes < 1`, invalid rrule blob, empty PATCH body),
//! 404 "routine not found" (missing/soft-deleted/other-user), 500 logged
//! database errors.

use worker::*;

use api_core::models::{NewRoutineInput, UpdateRoutine};
use api_core::routines::RoutinesError;

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
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: RoutinesError) -> Result<Response> {
    match err {
        RoutinesError::Invalid(message) => json_error(ctx, 400, &message),
        RoutinesError::NotFound => json_error(ctx, 404, "routine not found"),
        RoutinesError::Repo(err) => {
            console_log!("routines: database error: {err}");
            json_error(ctx, 500, "failed to load routines")
        }
    }
}

fn routines_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1RoutineRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1RoutineRepo::new(db))
}

fn lists_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskListRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskListRepo::new(db))
}

fn categories_d1(
    ctx: &RouteContext<Option<api_core::Config>>,
) -> Result<crate::db::D1TaskCategoryRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1TaskCategoryRepo::new(db))
}

/// `GET /api/routines` → 200 `{"routines":[...]}`; living routines in standing
/// order, each with its computed category. Seeds the taxonomy like the tasks
/// endpoints so a routines-only first visitor still has a matcher.
pub async fn list_routines(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    match api_core::list_routines(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &user.id,
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

/// `POST /api/routines` → 200 `{"routine":{...}}`. Body:
/// `{title, estimated_minutes?, rrule}` — `rrule` is the two-line recurrence
/// blob (`DTSTART:` line + `RRULE:` line). Invalid rrule or a title that does
/// not uniquely classify is 400; nothing is persisted then.
pub async fn create_routine(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let input: NewRoutineInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    match api_core::create_routine(
        &lists_d1(&ctx)?,
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
        &user.id,
        &input,
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

/// `PATCH /api/routines/:id` → 200 `{"routine":{...}}`. Body:
/// `{title?, estimated_minutes?, rrule?, sort_order?}` — at least one field
/// required (400 otherwise). Rule changes never touch materialized
/// occurrences.
pub async fn update_routine(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "routine not found");
    };

    let updates: UpdateRoutine = match req.json().await {
        Ok(updates) => updates,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    match api_core::update_routine(
        &categories_d1(&ctx)?,
        &routines_d1(&ctx)?,
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

/// `DELETE /api/routines/:id` → 200 `{"success":true}` (soft delete;
/// materialized occurrences are untouched).
pub async fn delete_routine(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "routine not found");
    };

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);

    match api_core::delete_routine(&routines_d1(&ctx)?, &user.id, id, &now_rfc3339).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

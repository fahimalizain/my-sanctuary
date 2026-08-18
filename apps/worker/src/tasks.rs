//! `/api/tasks/*` handlers: task CRUD, classified by title regex.
//!
//! Session-gated via the session cookie only — like `/api/lists/*` and
//! `/api/categories/*`. The orchestration lives in `api_core::tasks` (pure,
//! unit-tested); this file extracts the session user, wires the D1 repos, and
//! maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid input (title does not match a
//! category / matches several / matches untracked, bad duration or priority,
//! empty PATCH body), 404 not found (missing/soft-deleted/other-user task),
//! 500 logged database errors.
//!
//! `GET /api/tasks` and `POST /api/tasks` run `ensure_taxonomy` (count-gated)
//! so a tasks-only client still gets a matcher; the first Lists visit keeps
//! its order because the seed only fires when lists/categories are empty.

use worker::*;

use api_core::models::{NewTaskInput, UpdateTask};
use api_core::tasks::TasksError;

/// 401 body for missing/invalid sessions.
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
        TasksError::Repo(err) => {
            console_log!("tasks: database error: {err}");
            json_error(ctx, 500, "failed to load tasks")
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
/// duration_minutes?, priority?}`. The title must uniquely match a
/// non-untracked category (400 otherwise, with the reason in `error`).
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

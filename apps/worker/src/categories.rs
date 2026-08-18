//! `/api/categories/*` handlers: category CRUD for the one-level taxonomy.
//!
//! Session-gated via the session cookie only — like `/api/lists/*`. The
//! orchestration lives in `api_core::categories` (pure, unit-tested); this
//! file extracts the session user, wires the D1 repo, and maps errors to HTTP
//! responses.
//!
//! Status map: 401 unauthorized, 400 invalid input (including bad pattern
//! regexes), 404 not found, 409 conflict (living children, the undeletable
//! `untracked` sink, slug collisions), 500 logged database errors.
//!
//! `GET /api/categories` intentionally does NOT seed: the taxonomy is seeded
//! once by `GET /api/lists` so parallel first-visit fetches can never
//! double-seed.

use worker::*;

use api_core::categories::CategoriesError;
use api_core::models::{NewTaskCategoryInput, UpdateTaskCategory};

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
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: CategoriesError) -> Result<Response> {
    match err {
        CategoriesError::Invalid(message) => json_error(ctx, 400, &message),
        CategoriesError::NotFound => json_error(ctx, 404, "category not found"),
        CategoriesError::Conflict(message) => json_error(ctx, 409, &message),
        CategoriesError::Repo(err) => {
            console_log!("categories: database error: {err}");
            json_error(ctx, 500, "failed to load categories")
        }
    }
}

fn d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1TaskCategoryRepo> {
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

/// `GET /api/categories` → 200 `{"categories":[...]}` (the taxonomy is seeded
/// by `GET /api/lists`, so this is a pure read).
pub async fn list_categories(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    match api_core::categories::list_categories(&d1(&ctx)?, &user.id).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/categories` → 200 `{"category":{...}}`. Body: category fields +
/// `patterns` (see `NewTaskCategoryInput`). Roots must reference a list owned
/// by the session user (404 otherwise).
pub async fn create_category(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let input: NewTaskCategoryInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    match api_core::categories::create_category(&d1(&ctx)?, &lists_d1(&ctx)?, &user.id, &input).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `PATCH /api/categories/:id` → 200 `{"category":{...}}`. Body:
/// `UpdateTaskCategory` — every field optional, `patterns` replaces the whole
/// set when present. A root's new `list_id` must reference a list owned by
/// the session user (404 otherwise).
pub async fn update_category(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "category not found");
    };
    let updates: UpdateTaskCategory = match req.json().await {
        Ok(updates) => updates,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    match api_core::categories::update_category(&d1(&ctx)?, &lists_d1(&ctx)?, &user.id, id, &updates).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `DELETE /api/categories/:id` → 200 `{"success":true}` (409 when the
/// category has living children or is the undeletable `untracked` sink).
pub async fn delete_category(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "category not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    match api_core::categories::delete_category(&d1(&ctx)?, &user.id, id, &now_rfc3339).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::cors_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

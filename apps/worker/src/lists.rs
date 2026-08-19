//! `/api/lists/*` handlers: task list CRUD (the former "streams").
//!
//! Session-gated via the session cookie only — unlike the calendar handlers,
//! `task_lists` is user-scoped by the session and does NOT require a Google
//! token refresh. The orchestration lives in `api_core::lists` (pure,
//! unit-tested); this file extracts the session user, wires the D1 repo, and
//! maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid input, 404 not found,
//! 409 list in use, 500 logged database errors.

use worker::*;

use api_core::lists::ListsError;
use api_core::models::UpdateTaskList;

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
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: ListsError) -> Result<Response> {
    match err {
        ListsError::Invalid(message) => json_error(ctx, 400, &message),
        ListsError::NotFound => json_error(ctx, 404, "list not found"),
        ListsError::Conflict => json_error(ctx, 409, "list in use"),
        ListsError::Repo(err) => {
            console_log!("lists: database error: {err}");
            json_error(ctx, 500, "failed to load lists")
        }
    }
}

/// `GET /api/lists` → 200 `{"lists":[...]}`; seeds the defaults on first visit.
pub async fn list_lists(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let lists = crate::db::D1TaskListRepo::new(d1()?);
    let categories = crate::db::D1TaskCategoryRepo::new(d1()?);
    // Seeds the taxonomy (categories) on first visit too.
    match api_core::list_lists(&lists, &categories, &user.id).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/lists` → 200 `{"list":{...}}`. Body: `{name, color}`.
pub async fn create_list(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };

    let input: NewListInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let lists = crate::db::D1TaskListRepo::new(d1()?);
    match api_core::create_list(&lists, &user.id, &input.name, &input.color).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `PATCH /api/lists/:id` → 200 `{"list":{...}}`. Body: `{name?, color?,
/// sort_order?}` — at least one field required (400 otherwise).
pub async fn update_list(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "list not found");
    };

    let updates: UpdateTaskList = match req.json().await {
        Ok(updates) => updates,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let lists = crate::db::D1TaskListRepo::new(d1()?);
    match api_core::update_list(&lists, &user.id, id, &updates).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `DELETE /api/lists/:id` → 200 `{"success":true}` (409 when the list still
/// has living root categories).
pub async fn delete_list(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::session_user(&req, ctx.data.as_ref()) else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "list not found");
    };

    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);

    let d1 = || ctx.d1("DB").map_err(|_| Error::RustError("d1 binding not configured".to_string()));
    let lists = crate::db::D1TaskListRepo::new(d1()?);
    match api_core::delete_list(&lists, &user.id, id, &now_rfc3339).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(&ctx))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// Request body for `POST /api/lists`.
#[derive(Debug, serde::Deserialize)]
struct NewListInput {
    pub name: String,
    pub color: String,
}

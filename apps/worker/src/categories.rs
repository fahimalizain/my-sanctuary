//! `/api/categories/*` handlers: category CRUD for the one-level taxonomy.
//!
//! Session-gated via the session cookie or an API key bearer — like
//! `/api/lists/*`. The
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
//!
//! **Reclassify trigger (reports slice 2).** Every *successful* create,
//! update or delete restarts the digest's `reclassify` job (status `running`,
//! cursor `''`) because the already-stamped `calendar_events.category_id`
//! values only change when the classifier re-runs. The rebuild itself never
//! happens inside the request: a best-effort queue message pulls a tick
//! forward, and the cron picks up a `running` job regardless. A queue failure
//! must not fail the HTTP response. `is_productive`-only edits restart the
//! scan too — wasteful but correct and simple.

use worker::*;

use api_core::categories::CategoriesError;
use api_core::models::{NewTaskCategoryInput, UpdateTaskCategory};
use api_core::DigestRepo;

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

/// Restarts the digest reclassify job after a taxonomy change, then pings the
/// queue so a tick starts immediately.
///
/// Both steps are best-effort: the cron sees a `running` job on the next tick
/// even when the queue is unavailable, and a failure here must never turn a
/// successful category edit into a 500.
async fn trigger_reclassify(ctx: &RouteContext<Option<api_core::Config>>, user_id: &str) {
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    let repo = match ctx.d1("DB") {
        Ok(db) => crate::db::D1DigestRepo::new(db),
        Err(err) => {
            console_log!("categories: DB binding missing for reclassify: {err}");
            return;
        }
    };
    if let Err(err) = repo
        .upsert_job_status(user_id, "reclassify", "running", "", &now_rfc3339)
        .await
    {
        console_log!("categories: reclassify job reset failed: {err}");
        return;
    }
    match ctx.env.queue("CALENDAR_SYNC") {
        Ok(queue) => {
            let message = api_core::DigestQueueMessage {
                kind: api_core::DigestJobKind::Reclassify,
                user_id: user_id.to_string(),
            };
            if let Err(err) = queue.send(message).await {
                console_log!("categories: reclassify enqueue failed: {err}");
            }
        }
        Err(err) => {
            console_log!("categories: CALENDAR_SYNC binding missing: {err}");
        }
    }
}

/// `GET /api/categories` → 200 `{"categories":[...]}` (the taxonomy is seeded
/// by `GET /api/lists`, so this is a pure read).
pub async fn list_categories(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    match api_core::categories::list_categories(&d1(&ctx)?, &user.id).await {
        Ok(response) => {
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
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
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let input: NewTaskCategoryInput = match req.json().await {
        Ok(input) => input,
        Err(_) => return json_error(&ctx, 400, "invalid body"),
    };
    match api_core::categories::create_category(&d1(&ctx)?, &lists_d1(&ctx)?, &user.id, &input).await {
        Ok(response) => {
            // The stored category stamps are now stale; a tick will re-run the
            // classifier over the cached events. Never blocks the response.
            trigger_reclassify(&ctx, &user.id).await;
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
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
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
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
            // The stored category stamps are now stale; a tick will re-run the
            // classifier over the cached events. Never blocks the response.
            trigger_reclassify(&ctx, &user.id).await;
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
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
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id") else {
        return json_error(&ctx, 404, "category not found");
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    match api_core::categories::delete_category(&d1(&ctx)?, &user.id, id, &now_rfc3339).await {
        Ok(response) => {
            // The stored category stamps are now stale; a tick will re-run the
            // classifier over the cached events. Never blocks the response.
            trigger_reclassify(&ctx, &user.id).await;
            let response = Response::from_json(&response)?;
            Ok(response.with_headers(crate::auth::json_headers(crate::auth::frontend_url(
                &ctx,
            ))?))
        }
        Err(err) => map_error(&ctx, err),
    }
}

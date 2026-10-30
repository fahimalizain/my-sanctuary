//! `/api/rules/*` and `/api/pauses/*` handlers (reports slice 3).
//!
//! Session-gated like `/api/categories/*` (session cookie or API key bearer).
//! The model lives in
//! `api_core::rules` (pure, unit-tested); this file extracts the session user,
//! wires the D1 repos, and maps errors to HTTP responses.
//!
//! Status map: 401 unauthorized, 400 invalid body / invalid params / unknown
//! category, 404 unknown rule or pause, 409 a transition the rule's phase does
//! not allow, 500 corrupt stored data and logged database errors.
//!
//! `GET /api/rules/scoreboard` seeds the eight enforcement rules when the user
//! has none, and asks the digest to start a backfill — it never waits for the
//! rebuild. Every mutating rule endpoint answers with the same `RuleScore`
//! card the scoreboard uses, so the client never has to re-fetch.

use worker::*;

use api_core::rules::{RulesError, UpdateRuleInput};

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
    Ok(Response::from_json(&serde_json::json!({ "error": message }))?
        .with_status(status)
        .with_headers(headers))
}

/// Maps a service error to its HTTP response.
///
/// Only a request-body problem is a 400: corrupt stored data and a failed
/// repository are 500s, logged for the operator.
fn map_error(ctx: &RouteContext<Option<api_core::Config>>, err: RulesError) -> Result<Response> {
    match err {
        RulesError::Invalid(message) => json_error(ctx, 400, &message),
        RulesError::Corrupt(message) => {
            console_log!("rules: corrupt stored data: {message}");
            json_error(ctx, 500, "failed to load rules")
        }
        RulesError::RuleNotFound => json_error(ctx, 404, "rule not found"),
        RulesError::PauseNotFound => json_error(ctx, 404, "pause not found"),
        RulesError::Conflict(message) => json_error(ctx, 409, &message),
        RulesError::Repo(err) => {
            console_log!("rules: database error: {err}");
            json_error(ctx, 500, "failed to load rules")
        }
    }
}

/// Logs the operator-facing notes a scoring pass collected (a skipped seed, an
/// unreadable rule row). They never reach the client.
fn log_warnings(warnings: &[String]) {
    for warning in warnings {
        console_log!("rules: {warning}");
    }
}

fn rules_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1RuleRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1RuleRepo::new(db))
}

fn digest_d1(ctx: &RouteContext<Option<api_core::Config>>) -> Result<crate::db::D1DigestRepo> {
    let db = ctx
        .d1("DB")
        .map_err(|_| Error::RustError("d1 binding not configured".to_string()))?;
    Ok(crate::db::D1DigestRepo::new(db))
}

/// Renders a value as a JSON response with the shared CORS headers.
fn json_response<T: serde::Serialize>(
    ctx: &RouteContext<Option<api_core::Config>>,
    value: &T,
) -> Result<Response> {
    Ok(Response::from_json(value)?
        .with_headers(crate::auth::json_headers(crate::auth::frontend_url(ctx))?))
}

/// The request body as JSON, or `None` when it is missing or malformed.
async fn json_body<T: serde::de::DeserializeOwned>(req: &mut Request) -> Option<T> {
    let text = req.text().await.ok()?;
    if text.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&text).ok()
}

/// The request body as JSON, falling back to `T::default()` for an empty body
/// (the pause endpoint takes an optional `{from, to}`).
async fn json_body_or_default<T: serde::de::DeserializeOwned + Default>(
    req: &mut Request,
) -> Option<T> {
    let text = req.text().await.ok()?;
    if text.trim().is_empty() {
        return Some(T::default());
    }
    serde_json::from_str(&text).ok()
}

/// "Today" in the user's primary calendar zone, plus the RFC 3339 now stamp
/// the repos write.
async fn now_and_as_of(
    ctx: &RouteContext<Option<api_core::Config>>,
    user_id: &str,
) -> Result<(String, String)> {
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    let now_rfc3339 = api_core::unix_secs_to_rfc3339(now_unix);
    let (as_of, _time_zone) = api_core::today(&digest_d1(ctx)?, user_id, now_unix)
        .await
        .map_err(|err| Error::RustError(format!("rules: today failed: {err}")))?;
    Ok((as_of, now_rfc3339))
}

/// `GET /api/rules/scoreboard` → 200 `Scoreboard`.
pub async fn scoreboard(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    match api_core::scoreboard(&rules_d1(&ctx)?, &digest_d1(&ctx)?, &user.id, now_unix).await {
        Ok(board) => {
            log_warnings(&board.warnings);
            json_response(&ctx, &board)
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `GET /api/rules/archive` → 200 `{"rules":[RuleScore]}`.
pub async fn archive(req: Request, ctx: RouteContext<Option<api_core::Config>>) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    match api_core::archive_rules(&rules_d1(&ctx)?, &digest_d1(&ctx)?, &user.id, now_unix).await {
        Ok(response) => {
            log_warnings(&response.warnings);
            json_response(&ctx, &response)
        }
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/rules` → 200 the created `RuleScore`.
pub async fn create_rule(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(input) = json_body::<api_core::NewRuleInput>(&mut req).await else {
        return json_error(&ctx, 400, "invalid body");
    };
    let (as_of, now_rfc3339) = now_and_as_of(&ctx, &user.id).await?;
    match api_core::create_rule(&rules_d1(&ctx)?, &user.id, &input, &as_of, &now_rfc3339).await {
        Ok(score) => json_response(&ctx, &score),
        Err(err) => map_error(&ctx, err),
    }
}

/// `PATCH /api/rules/:id` → 200 the updated `RuleScore`.
pub async fn update_rule(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").cloned() else {
        return json_error(&ctx, 404, "rule not found");
    };
    let Some(input) = json_body::<UpdateRuleInput>(&mut req).await else {
        return json_error(&ctx, 400, "invalid body");
    };
    let (as_of, now_rfc3339) = now_and_as_of(&ctx, &user.id).await?;
    match api_core::update_rule(&rules_d1(&ctx)?, &user.id, &id, &input, &as_of, &now_rfc3339).await
    {
        Ok(score) => json_response(&ctx, &score),
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/rules/:id/pause` → 200 the updated `RuleScore`.
///
/// No body (or an empty one) pauses open-endedly; `{from, to}` schedules a
/// closed per-rule period without touching the status.
pub async fn pause_rule(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").cloned() else {
        return json_error(&ctx, 404, "rule not found");
    };
    let Some(input) =
        json_body_or_default::<api_core::PauseRuleInput>(&mut req).await
    else {
        return json_error(&ctx, 400, "invalid body");
    };
    let (as_of, now_rfc3339) = now_and_as_of(&ctx, &user.id).await?;
    match api_core::pause_rule(&rules_d1(&ctx)?, &user.id, &id, &input, &as_of, &now_rfc3339).await {
        Ok(score) => json_response(&ctx, &score),
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/rules/:id/resume` → 200 the updated `RuleScore`.
pub async fn resume_rule(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").cloned() else {
        return json_error(&ctx, 404, "rule not found");
    };
    let (as_of, now_rfc3339) = now_and_as_of(&ctx, &user.id).await?;
    match api_core::resume_rule(&rules_d1(&ctx)?, &user.id, &id, &as_of, &now_rfc3339).await {
        Ok(score) => json_response(&ctx, &score),
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/rules/:id/archive` → 200 the updated `RuleScore`. Idempotent.
pub async fn archive_rule(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    set_archived(req, ctx, true).await
}

/// `POST /api/rules/:id/restore` → 200 the updated `RuleScore`.
pub async fn restore_rule(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    set_archived(req, ctx, false).await
}

async fn set_archived(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
    archived: bool,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").cloned() else {
        return json_error(&ctx, 404, "rule not found");
    };
    let (as_of, now_rfc3339) = now_and_as_of(&ctx, &user.id).await?;
    match api_core::set_rule_archived(
        &rules_d1(&ctx)?,
        &user.id,
        &id,
        archived,
        &as_of,
        &now_rfc3339,
    )
    .await
    {
        Ok(score) => json_response(&ctx, &score),
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/rules/reorder` → 200 `{"success":true}`. The body must list every
/// rule of the user exactly once.
pub async fn reorder_rules(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(input) = json_body::<api_core::ReorderInput>(&mut req).await else {
        return json_error(&ctx, 400, "invalid body");
    };
    let now_rfc3339 = api_core::unix_secs_to_rfc3339((worker::Date::now().as_millis() / 1000) as i64);
    match api_core::reorder_rules(&rules_d1(&ctx)?, &user.id, &input, &now_rfc3339).await {
        Ok(()) => json_response(&ctx, &serde_json::json!({ "success": true })),
        Err(err) => map_error(&ctx, err),
    }
}

/// `GET /api/pauses` → 200 `{"pauses":[...]}`.
pub async fn list_pauses(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    match api_core::list_pauses(&rules_d1(&ctx)?, &user.id).await {
        Ok(response) => json_response(&ctx, &response),
        Err(err) => map_error(&ctx, err),
    }
}

/// `POST /api/pauses` → 200 `{"pause":{...}}` (a global period).
pub async fn create_pause(
    mut req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(input) = json_body::<api_core::NewPauseInput>(&mut req).await else {
        return json_error(&ctx, 400, "invalid body");
    };
    let now_rfc3339 = api_core::unix_secs_to_rfc3339((worker::Date::now().as_millis() / 1000) as i64);
    match api_core::create_global_pause(&rules_d1(&ctx)?, &user.id, &input, &now_rfc3339).await {
        Ok(response) => json_response(&ctx, &response),
        Err(err) => map_error(&ctx, err),
    }
}

/// `DELETE /api/pauses/:id` → 200 `{"success":true}`.
pub async fn delete_pause(
    req: Request,
    ctx: RouteContext<Option<api_core::Config>>,
) -> Result<Response> {
    let Some(user) = crate::auth::authenticate(&req, &ctx).await else {
        return unauthorized(&ctx);
    };
    let Some(id) = ctx.param("id").cloned() else {
        return json_error(&ctx, 404, "pause not found");
    };
    match api_core::delete_pause(&rules_d1(&ctx)?, &user.id, &id).await {
        Ok(()) => json_response(&ctx, &serde_json::json!({ "success": true })),
        Err(err) => map_error(&ctx, err),
    }
}

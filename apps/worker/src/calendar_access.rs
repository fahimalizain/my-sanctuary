//! Per-request calendar access selection (ADR 0012).
//!
//! Every handler that used to demand "a refreshable Google token" now asks for
//! a [`CalendarSession`]: the [`api_core::HttpClient`] to call plus the
//! [`api_core::GoogleAccess`] to pass down. That keeps the Google and
//! standalone paths behind one gate instead of sprinkling
//! `integration_type` checks through the calendar/tasks/agenda handlers.
//!
//! - **Google**: refresh the stored token through [`crate::http::WorkerHttp`]
//!   and hand back that client.
//! - **Standalone**: fail closed unless
//!   [`Config::standalone_enabled_for`](api_core::Config::standalone_enabled_for)
//!   approves this request host (a public host, `SECURE_COOKIE=true`, or the
//!   flag being off all reach this branch as `Unauthorized`), then hand back
//!   [`api_core::StandaloneCalendarHttp`] over fresh D1 handles and a dummy
//!   bearer. No Google call is possible on this path.
//!
//! The Google failure is carried as [`CalendarAccessError::GoogleRefresh`] —
//! not flattened into 401 — so GET list handlers can keep their existing
//! cache-fallback contract ([`api_core::classify_refresh_failure`]); write
//! handlers map it to 401 exactly as they did before.
//!
//! Building a session is cheap by design (one token read for Google, no I/O
//! for standalone), so [`list_events`](crate::calendar::list_events) calls it
//! inside its existing `futures::join!` and pays no extra round trip.

use std::sync::Arc;

use api_core::models::IntegrationType;
use api_core::{GoogleAccess, TokenError};
use worker::{Request, RouteContext};

/// The calendar client and bearer for one authenticated request.
pub struct CalendarSession {
    /// Google API client (Google users) or the local cache-backed stand-in
    /// (standalone users).
    pub http: Box<dyn api_core::HttpClient>,
    /// Bearer handed to `api_core`; standalone gets a fixed dummy value so the
    /// stand-in never depends on it.
    pub access: GoogleAccess,
}

/// Why [`calendar_access`] could not build a session.
///
/// `Unauthorized` is the credential/feature-gate answer (401); `GoogleRefresh`
/// keeps the token error intact so list paths can classify it.
pub enum CalendarAccessError {
    /// No usable calendar backend for this user (unknown integration type,
    /// standalone gate closed, missing config/D1 binding on the standalone
    /// path).
    Unauthorized,
    /// The Google token could not be refreshed (or no token/oauth is
    /// configured). List paths classify it, write paths 401.
    GoogleRefresh(TokenError),
}

/// The request's host: parsed from the URL, falling back to the `Host`
/// header (the Worker's `Request::url()` is always present in practice, but
/// the header keeps the standalone gate honest behind a rewrite).
pub(crate) fn request_host(req: &Request) -> Option<String> {
    req.url()
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .or_else(|| req.headers().get("Host").ok().flatten())
}

/// Resolves the calendar backend for `user` (see the module docs).
pub async fn calendar_access(
    ctx: &RouteContext<Option<api_core::Config>>,
    req: &Request,
    user: &api_core::SessionUser,
    now_unix: i64,
) -> Result<CalendarSession, CalendarAccessError> {
    match user.integration_type {
        IntegrationType::Google => google_session(ctx, user, now_unix).await,
        IntegrationType::Standalone => standalone_session(ctx, req, user, now_unix),
    }
}

async fn google_session(
    ctx: &RouteContext<Option<api_core::Config>>,
    user: &api_core::SessionUser,
    now_unix: i64,
) -> Result<CalendarSession, CalendarAccessError> {
    // A missing config/oauth/D1 binding is "no refreshable grant": the list
    // paths fall back to the cache (MissingGrant), the write paths 401. That
    // is what the old `session_and_oauth` gate's callers effectively did.
    let Some(config) = ctx.data.as_ref() else {
        return Err(CalendarAccessError::GoogleRefresh(TokenError::NoToken));
    };
    let Some(oauth) = config.oauth.as_ref() else {
        return Err(CalendarAccessError::GoogleRefresh(TokenError::NoToken));
    };
    let Ok(db) = ctx.d1("DB") else {
        return Err(CalendarAccessError::GoogleRefresh(TokenError::NoToken));
    };

    let tokens = crate::db::D1TokenRepo::new(db);
    let access =
        api_core::refresh_if_needed(&crate::http::WorkerHttp, &tokens, oauth, &user.id, now_unix)
            .await
            .map_err(CalendarAccessError::GoogleRefresh)?;
    Ok(CalendarSession {
        http: Box::new(crate::http::WorkerHttp),
        access,
    })
}

fn standalone_session(
    ctx: &RouteContext<Option<api_core::Config>>,
    req: &Request,
    user: &api_core::SessionUser,
    now_unix: i64,
) -> Result<CalendarSession, CalendarAccessError> {
    // Fail closed: the same server-side gate the auth endpoints use. A sealed
    // standalone cookie is not enough on a public host.
    let Some(config) = ctx.data.as_ref() else {
        return Err(CalendarAccessError::Unauthorized);
    };
    if !config.standalone_enabled_for(request_host(req).as_deref()) {
        return Err(CalendarAccessError::Unauthorized);
    }

    let d1 = || ctx.d1("DB").map_err(|_| CalendarAccessError::Unauthorized);
    let calendars = crate::db::D1CalendarRepo::new(d1()?);
    let events = crate::db::D1CalendarEventRepo::new(d1()?);
    Ok(CalendarSession {
        http: Box::new(api_core::StandaloneCalendarHttp::new(
            Arc::new(calendars),
            Arc::new(events),
            user.id.clone(),
            now_unix,
        )),
        // Never used by the stand-in; keeps the `GoogleAccess` plumbing (and
        // the journal's call sites) unchanged.
        access: GoogleAccess {
            access_token: "standalone".to_string(),
            token_type: "Bearer".to_string(),
        },
    })
}

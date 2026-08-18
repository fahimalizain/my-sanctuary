//! Shared API types for the sanctuary Worker.
//!
//! Pure Rust — no `worker` dependency — so it can be unit-tested natively
//! (`cargo test -p api-core`) while still compiling for `wasm32-unknown-unknown`
//! inside `apps/worker`.

pub mod calendar;
pub mod models;
pub mod oauth;
pub mod repo;
pub mod time;
pub mod token;

mod config;
mod health;
mod session;

pub use calendar::{
    create_event, decide_webhook, ensure_watch, is_public_https_callback, list_events,
    parse_event_time_range, stop_watches_for_calendar, sync_calendar, tokens_match,
    CalendarError, CalendarEventsResponse, CreateEventOutput, CreateEventResponse,
    CalendarListOutput, WebhookDecision, GOOGLE_CALENDAR_LIST_URL, GOOGLE_CHANNELS_STOP_URL,
    GOOGLE_EVENTS_BASE_URL, SYNC_STALE_THRESHOLD_SECS, WATCH_DEFAULT_TTL_SECS,
};
pub use config::{
    Config, ConfigError, OAuthConfig, DEFAULT_FRONTEND_URL, MIN_SESSION_SECRET_LEN,
};
pub use health::{HealthResponse, VersionResponse};
pub use oauth::{
    authorization_url, exchange_and_login, generate_state, HttpClient, HttpError, OAuthError,
    GOOGLE_AUTH_URL, GOOGLE_TOKEN_URL, GOOGLE_USERINFO_URL, OAUTH_SCOPES,
};
pub use repo::{
    build_event_upsert_sql, CalendarEventRepo, CalendarRepo, RepoError, TokenRepo, UserRepo,
    WatchChannelRepo, EVENT_UPSERT_CHUNK_SIZE, EVENT_UPSERT_COL_COUNT,
};
pub use session::{
    clear_session_cookie_header, cookie_value_from_header, seal, session_cookie_header, unseal,
    LogoutResponse, MeResponse, SessionError, SessionUser, SESSION_COOKIE_NAME,
    SESSION_DURATION_SECS,
};
pub use time::{rfc3339_to_unix_secs, unix_secs_to_rfc3339};
pub use token::{refresh_if_needed, GoogleAccess, TokenError, REFRESH_SKEW_SECS};

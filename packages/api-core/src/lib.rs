//! Shared API types for the sanctuary Worker.
//!
//! Pure Rust — no `worker` dependency — so it can be unit-tested natively
//! (`cargo test -p api-core`) while still compiling for `wasm32-unknown-unknown`
//! inside `apps/worker`.

pub mod models;
pub mod oauth;
pub mod repo;
pub mod time;

mod config;
mod health;
mod session;

pub use config::{
    Config, ConfigError, OAuthConfig, DEFAULT_FRONTEND_URL, MIN_SESSION_SECRET_LEN,
};
pub use health::{HealthResponse, VersionResponse};
pub use oauth::{
    authorization_url, exchange_and_login, generate_state, HttpClient, HttpError, OAuthError,
    GOOGLE_AUTH_URL, GOOGLE_TOKEN_URL, GOOGLE_USERINFO_URL, OAUTH_SCOPES,
};
pub use repo::{RepoError, TokenRepo, UserRepo};
pub use session::{
    clear_session_cookie_header, cookie_value_from_header, seal, session_cookie_header, unseal,
    LogoutResponse, MeResponse, SessionError, SessionUser, SESSION_COOKIE_NAME,
    SESSION_DURATION_SECS,
};
pub use time::unix_secs_to_rfc3339;

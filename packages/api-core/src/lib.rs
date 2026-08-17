//! Shared API types for the sanctuary Worker.
//!
//! Pure Rust — no `worker` dependency — so it can be unit-tested natively
//! (`cargo test -p api-core`) while still compiling for `wasm32-unknown-unknown`
//! inside `apps/worker`.

mod config;
mod health;
mod session;

pub use config::{Config, ConfigError, DEFAULT_FRONTEND_URL, MIN_SESSION_SECRET_LEN};
pub use health::{HealthResponse, VersionResponse};
pub use session::{
    clear_session_cookie_header, cookie_value_from_header, seal, session_cookie_header, unseal,
    LogoutResponse, MeResponse, SessionError, SessionUser, SESSION_COOKIE_NAME,
    SESSION_DURATION_SECS,
};

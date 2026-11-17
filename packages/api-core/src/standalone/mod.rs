//! Standalone mode (ADR 0012): username/password identity with zero Google
//! dependency.
//!
//! Two pieces live here, both pure Rust and unit-testable with repo fakes:
//!
//! - [`auth`] — `register` / `login` orchestration: validation, PBKDF2 hashing
//!   ([`crate::password`]), the `users` + `local_credentials` writes, and the
//!   calendar self-heal that follows them.
//! - [`bootstrap`] — [`ensure_standalone_calendar`], the idempotent
//!   `google_calendars` seed for a standalone user's one local calendar.
//!
//! This module deliberately does **not** apply the server-side
//! `ALLOW_STANDALONE_AUTH` / request-host gate: the Worker owns that check on
//! both auth endpoints. Everything here assumes standalone mode is enabled.
//!
//! `login` is generic over the repo traits and `now_unix` is supplied by the
//! caller (`worker::Date` / a frozen test clock) — nothing here reads
//! `SystemTime`, which is unreliable on `wasm32-unknown-unknown`.

pub mod auth;
pub mod bootstrap;

pub use auth::{
    login, normalize_username, register, LoginInput, RegisterInput, StandaloneAuthConfig,
    StandaloneAuthError, FALLBACK_TIME_ZONE, MAX_NAME_LEN, MAX_PASSWORD_LEN, MAX_USERNAME_LEN,
    MIN_PASSWORD_LEN, MIN_USERNAME_LEN,
};
pub use bootstrap::{
    ensure_standalone_calendar, standalone_calendar_local_id, standalone_google_calendar_id,
    STANDALONE_CALENDAR_SUMMARY,
};

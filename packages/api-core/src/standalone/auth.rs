//! Standalone register/login orchestration (ADR 0012).
//!
//! `register` validates the form, mints a random local user id, then writes
//! the `users` row (`integration_type = 'standalone'`, synthetic
//! `google_id = standalone:<id>`, `<username>@standalone.invalid` email) and
//! the `local_credentials` row (PBKDF2 hash — the plaintext never reaches the
//! database) **in one atomic repo call**
//! ([`UserRepo::create_standalone_account`]), and finally seeds the user's
//! local calendar.
//!
//! `login` normalizes the username, verifies the stored PBKDF2 hash with a
//! constant-time compare, refuses anything that is not a standalone user
//! (`InvalidCredentials` for every miss, so nothing leaks), and self-heals a
//! missing calendar.
//!
//! Both functions are generic over the repo traits and take `now_unix` — the
//! Worker supplies `Date.now()`, tests use a frozen clock. Nothing here reads
//! `SystemTime` (unreliable on wasm) and nothing here talks to Google.
//!
//! The `ALLOW_STANDALONE_AUTH` / non-public-host gate is deliberately **not**
//! applied in this module: the Worker checks it on both auth endpoints before
//! calling in.

use getrandom::getrandom;
use thiserror::Error;

use crate::models::{IntegrationType, NewLocalCredential, NewStandaloneUser};
use crate::password::{hash_password_with_iterations, verify_password, PasswordError};
use crate::repo::{CalendarRepo, LocalCredentialRepo, RepoError, UserRepo};
use crate::session::SessionUser;
use crate::time::{parse_iana_tz, unix_secs_to_rfc3339};

use super::bootstrap::ensure_standalone_calendar;

/// Shortest accepted username (normalized length).
pub const MIN_USERNAME_LEN: usize = 3;
/// Longest accepted username (normalized length).
pub const MAX_USERNAME_LEN: usize = 32;
/// Shortest accepted password (`chars().count()`).
pub const MIN_PASSWORD_LEN: usize = 8;
/// Longest accepted password (`chars().count()`).
pub const MAX_PASSWORD_LEN: usize = 128;
/// Longest accepted display name.
pub const MAX_NAME_LEN: usize = 80;

/// Zone used when neither the browser nor the configured default yields a
/// valid IANA name.
pub const FALLBACK_TIME_ZONE: &str = "UTC";

/// Body of `POST /auth/standalone/register`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterInput {
    pub username: String,
    pub password: String,
    pub name: Option<String>,
    /// IANA zone captured from the browser
    /// (`Intl.DateTimeFormat().resolvedOptions().timeZone`). `None`/empty →
    /// [`StandaloneAuthConfig::default_time_zone`].
    pub time_zone: Option<String>,
}

/// Body of `POST /auth/standalone/login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginInput {
    pub username: String,
    pub password: String,
}

/// Deployment knobs for standalone auth. The Worker builds this from `Config`
/// (`STANDALONE_TIME_ZONE` and the PBKDF2 work factor — 210k in production,
/// ~1k in tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneAuthConfig {
    /// IANA zone for a registration that carries no browser zone. An invalid
    /// value falls back to `UTC` (never an error: the config is deployment
    /// input, not user input).
    pub default_time_zone: String,
    /// PBKDF2 iteration count for newly stored hashes.
    pub password_iterations: u32,
}

/// Errors surfaced by [`register`] / [`login`].
///
/// The Worker maps these to HTTP: `InvalidInput` → 400, `UsernameTaken` → 409,
/// `InvalidCredentials` → 401, `Repo`/`Password`/`Entropy` → 500.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StandaloneAuthError {
    /// Caller-supplied form value failed validation (400).
    #[error("{0}")]
    InvalidInput(String),
    /// The normalized username is already registered (409).
    #[error("username is already taken")]
    UsernameTaken,
    /// Unknown username, wrong password, or a non-standalone user (401).
    /// Deliberately one error for every miss so nothing leaks.
    #[error("invalid username or password")]
    InvalidCredentials,
    /// Database failure (500).
    #[error(transparent)]
    Repo(#[from] RepoError),
    /// Password hashing failure (500).
    #[error(transparent)]
    Password(#[from] PasswordError),
    /// Randomness source failed while minting the user id (500).
    #[error("failed to obtain randomness for the standalone user id")]
    Entropy,
}

/// Trims and ASCII-lowercases `username`, then enforces `[a-z0-9._-]{3,32}`.
///
/// ASCII-only lowercasing is deliberate: a username is matched against a
/// `UNIQUE` SQLite column, where case-insensitive matching is ASCII-only too.
pub fn normalize_username(username: &str) -> Result<String, StandaloneAuthError> {
    let normalized = username.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(StandaloneAuthError::InvalidInput(
            "username is required".to_string(),
        ));
    }
    if !(MIN_USERNAME_LEN..=MAX_USERNAME_LEN).contains(&normalized.chars().count()) {
        return Err(StandaloneAuthError::InvalidInput(format!(
            "username must be between {MIN_USERNAME_LEN} and {MAX_USERNAME_LEN} characters"
        )));
    }
    if !normalized
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
    {
        return Err(StandaloneAuthError::InvalidInput(
            "username may only contain a-z, 0-9, '.', '_' and '-'".to_string(),
        ));
    }
    Ok(normalized)
}

/// 16 random bytes as 32 lowercase hex chars. `uuid` is deliberately not a
/// dependency (a second `getrandom` version for no benefit — same reasoning as
/// [`crate::password`]).
fn mint_user_id() -> Result<String, StandaloneAuthError> {
    let mut bytes = [0u8; 16];
    getrandom(&mut bytes).map_err(|_| StandaloneAuthError::Entropy)?;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        id.push_str(&format!("{byte:02x}"));
    }
    Ok(id)
}

fn validate_password(password: &str) -> Result<(), StandaloneAuthError> {
    let length = password.chars().count();
    if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&length) {
        return Err(StandaloneAuthError::InvalidInput(format!(
            "password must be between {MIN_PASSWORD_LEN} and {MAX_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

/// Trimmed name, defaulting to the normalized username when absent/empty.
fn resolve_name(name: Option<&str>, username: &str) -> Result<String, StandaloneAuthError> {
    match name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(name) => {
            if name.chars().count() > MAX_NAME_LEN {
                return Err(StandaloneAuthError::InvalidInput(format!(
                    "name must be at most {MAX_NAME_LEN} characters"
                )));
            }
            Ok(name.to_string())
        }
        None => Ok(username.to_string()),
    }
}

/// The configured default zone when it is a real IANA name, else `UTC`.
fn default_time_zone(config: &StandaloneAuthConfig) -> String {
    let configured = config.default_time_zone.trim();
    if parse_iana_tz(configured).is_some() {
        configured.to_string()
    } else {
        FALLBACK_TIME_ZONE.to_string()
    }
}

/// Browser-supplied zone when present and valid, else the config default.
///
/// A caller-supplied zone that is *present but invalid* is a 400 — silently
/// replacing a browser's zone with UTC would shift the user's whole agenda.
fn resolve_time_zone(
    time_zone: Option<&str>,
    config: &StandaloneAuthConfig,
) -> Result<String, StandaloneAuthError> {
    match time_zone.map(str::trim).filter(|zone| !zone.is_empty()) {
        Some(zone) => {
            if parse_iana_tz(zone).is_none() {
                return Err(StandaloneAuthError::InvalidInput(
                    "time zone is not a valid IANA zone".to_string(),
                ));
            }
            Ok(zone.to_string())
        }
        None => Ok(default_time_zone(config)),
    }
}

fn session_user(user: crate::models::User) -> SessionUser {
    SessionUser {
        id: user.id,
        email: user.email,
        name: user.name,
        picture: user.picture.unwrap_or_default(),
        integration_type: IntegrationType::Standalone,
    }
}

fn taken_or_repo(err: RepoError) -> StandaloneAuthError {
    match err {
        RepoError::Conflict => StandaloneAuthError::UsernameTaken,
        other => StandaloneAuthError::Repo(other),
    }
}

/// Registers a standalone user: validate → mint id → `users` row **and**
/// `local_credentials` row in one atomic write → calendar seed.
///
/// Returns the session identity for the new account. The user and credential
/// are written together ([`UserRepo::create_standalone_account`]): a duplicate
/// username loses the race at the credential insert and surfaces as
/// [`StandaloneAuthError::UsernameTaken`] with nothing written, so a failed
/// attempt never leaves an orphan user whose UNIQUE email blocks a retry (the
/// earlier `get_by_username` check only makes the common case cheap).
pub async fn register(
    users: &dyn UserRepo,
    credentials: &dyn LocalCredentialRepo,
    calendars: &dyn CalendarRepo,
    input: RegisterInput,
    config: &StandaloneAuthConfig,
    now_unix: i64,
) -> Result<SessionUser, StandaloneAuthError> {
    let username = normalize_username(&input.username)?;
    validate_password(&input.password)?;
    let name = resolve_name(input.name.as_deref(), &username)?;
    let time_zone = resolve_time_zone(input.time_zone.as_deref(), config)?;

    if credentials.get_by_username(&username).await?.is_some() {
        return Err(StandaloneAuthError::UsernameTaken);
    }

    // Hash before any write: an out-of-range iteration count or an entropy
    // failure must not leave a user row behind.
    let password_hash = hash_password_with_iterations(&input.password, config.password_iterations)?;

    let id = mint_user_id()?;
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let user = users
        .create_standalone_account(
            NewStandaloneUser {
                id: id.clone(),
                google_id: format!("standalone:{id}"),
                email: format!("{username}@standalone.invalid"),
                name: name.clone(),
                created_at: now_rfc3339.clone(),
                updated_at: now_rfc3339.clone(),
            },
            NewLocalCredential {
                user_id: id,
                username,
                password_hash,
                created_at: now_rfc3339.clone(),
                updated_at: now_rfc3339,
            },
        )
        .await
        .map_err(taken_or_repo)?;

    ensure_standalone_calendar(calendars, &user.id, &time_zone, now_unix).await?;

    Ok(session_user(user))
}

/// Logs a standalone user in: normalize → credential lookup → PBKDF2 verify →
/// load the standalone user row → self-heal the calendar.
///
/// Every miss (unknown username, wrong password, soft-deleted row, a Google
/// user that somehow owns this username) is [`StandaloneAuthError::InvalidCredentials`].
pub async fn login(
    users: &dyn UserRepo,
    credentials: &dyn LocalCredentialRepo,
    calendars: &dyn CalendarRepo,
    input: LoginInput,
    config: &StandaloneAuthConfig,
    now_unix: i64,
) -> Result<SessionUser, StandaloneAuthError> {
    // A malformed username cannot be registered, so it is just another 401.
    let username =
        normalize_username(&input.username).map_err(|_| StandaloneAuthError::InvalidCredentials)?;

    let Some(credential) = credentials.get_by_username(&username).await? else {
        return Err(StandaloneAuthError::InvalidCredentials);
    };
    if !verify_password(&credential.password_hash, &input.password) {
        return Err(StandaloneAuthError::InvalidCredentials);
    }

    let Some(user) = users.get_by_id(&credential.user_id).await? else {
        return Err(StandaloneAuthError::InvalidCredentials);
    };
    if user.integration_type != IntegrationType::Standalone {
        return Err(StandaloneAuthError::InvalidCredentials);
    }

    // Idempotent: `DO NOTHING` keeps an existing row (and its stored zone).
    ensure_standalone_calendar(calendars, &user.id, &default_time_zone(config), now_unix).await?;

    Ok(session_user(user))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::calendar::FakeCalendarRepo;
    use crate::models::{LocalCredential, User};
    use crate::repo::UserRepo;
    use crate::time::rfc3339_to_unix_secs;

    const NOW: i64 = 1_700_000_000;

    fn config() -> StandaloneAuthConfig {
        StandaloneAuthConfig {
            default_time_zone: "UTC".to_string(),
            password_iterations: 1_000,
        }
    }

    fn register_input(username: &str, password: &str) -> RegisterInput {
        RegisterInput {
            username: username.to_string(),
            password: password.to_string(),
            name: None,
            time_zone: None,
        }
    }

    /// Converts an insert input into the row the fake stores.
    fn user_row(user: NewStandaloneUser) -> User {
        User {
            id: user.id,
            google_id: user.google_id,
            email: user.email,
            name: user.name,
            picture: None,
            created_at: user.created_at,
            updated_at: user.updated_at,
            deleted_at: None,
            focused_task_id: None,
            integration_type: IntegrationType::Standalone,
        }
    }

    /// In-memory `UserRepo`: stores rows, honours the standalone insert guard,
    /// never rewrites an existing id. Its credential store is shared with the
    /// paired [`FakeCredentialRepo`], so `create_standalone_account` writes
    /// both rows the way the production repos do — the auth tests exercise the
    /// real register path instead of a stub.
    #[derive(Default)]
    struct FakeUserRepo {
        stored: Mutex<Vec<User>>,
        credentials: Arc<Mutex<Vec<LocalCredential>>>,
    }

    impl FakeUserRepo {
        fn insert_raw(&self, user: User) {
            self.stored.lock().unwrap().push(user);
        }

        fn rows(&self) -> Vec<User> {
            self.stored.lock().unwrap().clone()
        }
    }

    #[async_trait(?Send)]
    impl UserRepo for FakeUserRepo {
        async fn get_by_id(&self, id: &str) -> Result<Option<User>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|user| user.id == id && user.deleted_at.is_none())
                .cloned())
        }

        async fn get_by_google_id(&self, google_id: &str) -> Result<Option<User>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|user| user.google_id == google_id && user.deleted_at.is_none())
                .cloned())
        }

        async fn upsert_by_google_id(
            &self,
            _user: crate::models::NewUser,
        ) -> Result<User, RepoError> {
            Err(RepoError::Backend("not used by standalone tests".into()))
        }

        async fn create_standalone(&self, user: NewStandaloneUser) -> Result<User, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if stored
                .iter()
                .any(|row| row.google_id == user.google_id || row.email == user.email)
            {
                return Err(RepoError::Conflict);
            }
            let row = user_row(user);
            stored.push(row.clone());
            Ok(row)
        }

        async fn create_standalone_account(
            &self,
            user: NewStandaloneUser,
            credential: NewLocalCredential,
        ) -> Result<User, RepoError> {
            // Both checks happen before either row is pushed: a conflict
            // leaves the store untouched, like the SQL transaction.
            let mut stored = self.stored.lock().unwrap();
            let mut credentials = self.credentials.lock().unwrap();
            if credentials
                .iter()
                .any(|row| row.username == credential.username)
            {
                return Err(RepoError::Conflict);
            }
            if stored
                .iter()
                .any(|row| row.google_id == user.google_id || row.email == user.email)
            {
                return Err(RepoError::Conflict);
            }
            let row = user_row(user);
            credentials.push(credential.into_row());
            stored.push(row.clone());
            Ok(row)
        }

        async fn set_focused_task_id(
            &self,
            _user_id: &str,
            _task_id: Option<&str>,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// In-memory `LocalCredentialRepo` with the username UNIQUE guard.
    #[derive(Default)]
    struct FakeCredentialRepo {
        stored: Arc<Mutex<Vec<LocalCredential>>>,
    }

    impl FakeCredentialRepo {
        /// A credential repo over `users`' own store — the pair a register
        /// writes through and login reads back.
        fn sharing(users: &FakeUserRepo) -> Self {
            Self {
                stored: Arc::clone(&users.credentials),
            }
        }
    }

    /// A `UserRepo` + `LocalCredentialRepo` pair over one shared store.
    fn repos() -> (FakeUserRepo, FakeCredentialRepo) {
        let users = FakeUserRepo::default();
        let credentials = FakeCredentialRepo::sharing(&users);
        (users, credentials)
    }

    #[async_trait(?Send)]
    impl LocalCredentialRepo for FakeCredentialRepo {
        async fn get_by_username(
            &self,
            username: &str,
        ) -> Result<Option<LocalCredential>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.username == username)
                .cloned())
        }

        async fn insert(
            &self,
            credential: NewLocalCredential,
        ) -> Result<LocalCredential, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            if stored.iter().any(|row| row.username == credential.username) {
                return Err(RepoError::Conflict);
            }
            let row = credential.into_row();
            stored.push(row.clone());
            Ok(row)
        }

        async fn update_password(
            &self,
            _user_id: &str,
            _password_hash: &str,
            _now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            Ok(())
        }
    }

    fn google_user(id: &str) -> User {
        User {
            id: id.to_string(),
            google_id: format!("google-{id}"),
            email: "ada@example.com".to_string(),
            name: "Ada".to_string(),
            picture: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
            focused_task_id: None,
            integration_type: IntegrationType::Google,
        }
    }

    #[test]
    fn normalize_username_trims_lowercases_and_validates() {
        assert_eq!(
            normalize_username("  Ada.Lovelace_1 ").unwrap(),
            "ada.lovelace_1"
        );
        assert_eq!(normalize_username("abc").unwrap(), "abc");
        assert_eq!(normalize_username(&"a".repeat(32)).unwrap(), "a".repeat(32));

        for bad in [
            "",
            "  ",
            "ab",
            "a b",
            "ada@example.com",
            "ada/",
            "ñandú",
            &"a".repeat(33),
        ] {
            assert!(
                matches!(
                    normalize_username(bad),
                    Err(StandaloneAuthError::InvalidInput(_))
                ),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn register_login_roundtrip_seeds_the_browser_zone() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        let session = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            RegisterInput {
                username: "  Ada.Lovelace ".to_string(),
                password: "correct horse".to_string(),
                name: Some("  Ada Lovelace  ".to_string()),
                time_zone: Some("Asia/Kolkata".to_string()),
            },
            &config(),
            NOW,
        ))
        .expect("register");

        assert_eq!(session.integration_type, IntegrationType::Standalone);
        assert_eq!(session.email, "ada.lovelace@standalone.invalid");
        assert_eq!(session.name, "Ada Lovelace");
        assert_eq!(session.picture, "");
        assert_eq!(session.id.len(), 32, "16 random bytes as hex");
        assert!(session
            .id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));

        let stored = users.rows();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, session.id);
        assert_eq!(stored[0].google_id, format!("standalone:{}", session.id));
        assert_eq!(stored[0].email, "ada.lovelace@standalone.invalid");
        assert_eq!(stored[0].name, "Ada Lovelace");
        assert_eq!(stored[0].picture, None);
        assert_eq!(stored[0].created_at, "2023-11-14T22:13:20Z");
        assert_eq!(stored[0].integration_type, IntegrationType::Standalone);

        let credential = pollster::block_on(credentials.get_by_username("ada.lovelace"))
            .expect("query")
            .expect("credential");
        assert_eq!(credential.user_id, session.id);
        assert!(credential.password_hash.starts_with("pbkdf2-sha256$1000$"));
        assert!(verify_password(&credential.password_hash, "correct horse"));

        let seeded = calendars.stored.lock().unwrap().clone();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, format!("standalone-cal-{}", session.id));
        assert_eq!(
            seeded[0].google_calendar_id,
            format!("standalone:{}", session.id)
        );
        assert_eq!(seeded[0].time_zone, "Asia/Kolkata", "browser zone wins");
        assert!(seeded[0].sync_enabled);
        assert_eq!(seeded[0].sync_status, "disabled");

        let session = pollster::block_on(login(
            &users,
            &credentials,
            &calendars,
            LoginInput {
                username: "ADA.LOVELACE".to_string(),
                password: "correct horse".to_string(),
            },
            &config(),
            NOW + 60,
        ))
        .expect("login");
        assert_eq!(session.id, stored[0].id);
        assert_eq!(session.integration_type, IntegrationType::Standalone);
        assert_eq!(
            calendars.stored.lock().unwrap().len(),
            1,
            "login must not duplicate the calendar"
        );
    }

    #[test]
    fn duplicate_username_is_rejected_before_any_write() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", "password-1"),
            &config(),
            NOW,
        ))
        .expect("first register");

        let err = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("  ADA ", "password-2"),
            &config(),
            NOW,
        ))
        .unwrap_err();
        assert_eq!(err, StandaloneAuthError::UsernameTaken);
        assert_eq!(users.rows().len(), 1, "no second user row");
        assert_eq!(calendars.stored.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_orphan_user_row_makes_register_username_taken_not_a_500() {
        // A `users` row with the standalone email but no credential: the cheap
        // pre-check misses it, so the conflict only surfaces at the atomic
        // account write. It must be 409 (`UsernameTaken`), never a backend 500.
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());
        users.insert_raw(User {
            id: "orphan".to_string(),
            google_id: "standalone:orphan".to_string(),
            email: "ada@standalone.invalid".to_string(),
            name: "Ada".to_string(),
            picture: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            deleted_at: None,
            focused_task_id: None,
            integration_type: IntegrationType::Standalone,
        });

        let err = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", "password-1"),
            &config(),
            NOW,
        ))
        .unwrap_err();
        assert_eq!(err, StandaloneAuthError::UsernameTaken);
        assert_eq!(users.rows().len(), 1, "the orphan row is untouched");
        assert!(
            credentials.stored.lock().unwrap().is_empty(),
            "the failed attempt writes no credential"
        );
        assert!(calendars.stored.lock().unwrap().is_empty());
    }

    #[test]
    fn wrong_password_and_unknown_user_are_invalid_credentials() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", "password-1"),
            &config(),
            NOW,
        ))
        .expect("register");

        for (username, password) in [
            ("ada", "wrong-password"),
            ("ada", ""),
            ("nobody", "password-1"),
            ("ab", "password-1"),
            ("ada!", "password-1"),
        ] {
            let err = pollster::block_on(login(
                &users,
                &credentials,
                &calendars,
                LoginInput {
                    username: username.to_string(),
                    password: password.to_string(),
                },
                &config(),
                NOW,
            ))
            .unwrap_err();
            assert_eq!(
                err,
                StandaloneAuthError::InvalidCredentials,
                "{username:?}/{password:?}"
            );
        }
    }

    #[test]
    fn a_google_user_behind_a_credential_is_rejected() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        users.insert_raw(google_user("g-1"));
        pollster::block_on(credentials.insert(NewLocalCredential {
            user_id: "g-1".to_string(),
            username: "ada".to_string(),
            password_hash: hash_password_with_iterations("password-1", 1_000).unwrap(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }))
        .expect("credential");

        let err = pollster::block_on(login(
            &users,
            &credentials,
            &calendars,
            LoginInput {
                username: "ada".to_string(),
                password: "password-1".to_string(),
            },
            &config(),
            NOW,
        ))
        .unwrap_err();
        assert_eq!(err, StandaloneAuthError::InvalidCredentials);
        assert!(
            calendars.stored.lock().unwrap().is_empty(),
            "no calendar is seeded for a rejected login"
        );
    }

    #[test]
    fn invalid_browser_time_zone_is_a_400() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        let err = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            RegisterInput {
                username: "ada".to_string(),
                password: "password-1".to_string(),
                name: None,
                time_zone: Some("Mars/Olympus_Mons".to_string()),
            },
            &config(),
            NOW,
        ))
        .unwrap_err();
        assert!(
            matches!(err, StandaloneAuthError::InvalidInput(_)),
            "{err:?}"
        );
        assert!(users.rows().is_empty());
        assert!(calendars.stored.lock().unwrap().is_empty());
    }

    #[test]
    fn absent_time_zone_uses_the_config_default_and_falls_back_to_utc() {
        for (configured, expected) in [
            ("America/New_York", "America/New_York"),
            ("  Asia/Tokyo  ", "Asia/Tokyo"),
            ("bogus/zone", "UTC"),
            ("", "UTC"),
        ] {
            let (users, credentials) = repos();
            let calendars = FakeCalendarRepo::with(Vec::new());
            let config = StandaloneAuthConfig {
                default_time_zone: configured.to_string(),
                password_iterations: 1_000,
            };

            pollster::block_on(register(
                &users,
                &credentials,
                &calendars,
                RegisterInput {
                    username: "ada".to_string(),
                    password: "password-1".to_string(),
                    name: None,
                    time_zone: Some("   ".to_string()),
                },
                &config,
                NOW,
            ))
            .expect("register");

            let seeded = calendars.stored.lock().unwrap().clone();
            assert_eq!(seeded[0].time_zone, expected, "configured {configured:?}");
        }
    }

    #[test]
    fn name_defaults_to_the_username_and_is_bounded() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        let session = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            RegisterInput {
                username: "ada".to_string(),
                password: "password-1".to_string(),
                name: Some("   ".to_string()),
                time_zone: None,
            },
            &config(),
            NOW,
        ))
        .expect("register");
        assert_eq!(session.name, "ada");

        let err = pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            RegisterInput {
                username: "grace".to_string(),
                password: "password-1".to_string(),
                name: Some("x".repeat(MAX_NAME_LEN + 1)),
                time_zone: None,
            },
            &config(),
            NOW,
        ))
        .unwrap_err();
        assert!(
            matches!(err, StandaloneAuthError::InvalidInput(_)),
            "{err:?}"
        );
    }

    #[test]
    fn password_length_is_enforced() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        for password in ["", "short", &"x".repeat(7), &"x".repeat(129)] {
            let err = pollster::block_on(register(
                &users,
                &credentials,
                &calendars,
                register_input("ada", password),
                &config(),
                NOW,
            ))
            .unwrap_err();
            assert!(
                matches!(err, StandaloneAuthError::InvalidInput(_)),
                "{password:?}"
            );
        }

        // Exactly 128 characters is accepted.
        pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", &"x".repeat(MAX_PASSWORD_LEN)),
            &config(),
            NOW,
        ))
        .expect("128-character password");
    }

    #[test]
    fn login_self_heals_a_missing_calendar() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", "password-1"),
            &config(),
            NOW,
        ))
        .expect("register");
        assert_eq!(calendars.stored.lock().unwrap().len(), 1);

        // Simulate a lost calendar row (an older account, or a failed seed).
        calendars.stored.lock().unwrap().clear();

        pollster::block_on(login(
            &users,
            &credentials,
            &calendars,
            LoginInput {
                username: "ada".to_string(),
                password: "password-1".to_string(),
            },
            &StandaloneAuthConfig {
                default_time_zone: "Europe/Berlin".to_string(),
                password_iterations: 1_000,
            },
            NOW + 60,
        ))
        .expect("login");

        let seeded = calendars.stored.lock().unwrap().clone();
        assert_eq!(seeded.len(), 1, "login re-seeds the calendar");
        assert_eq!(seeded[0].time_zone, "Europe/Berlin");
        let id = users.rows()[0].id.clone();
        assert_eq!(seeded[0].id, format!("standalone-cal-{id}"));
    }

    #[test]
    fn registration_timestamps_come_from_now_unix() {
        let (users, credentials) = repos();
        let calendars = FakeCalendarRepo::with(Vec::new());

        pollster::block_on(register(
            &users,
            &credentials,
            &calendars,
            register_input("ada", "password-1"),
            &config(),
            NOW,
        ))
        .expect("register");

        assert_eq!(rfc3339_to_unix_secs(&users.rows()[0].created_at), Some(NOW));
        let seeded = calendars.stored.lock().unwrap().clone();
        assert_eq!(seeded[0].created_at, "2023-11-14T22:13:20Z");
        assert_eq!(
            seeded[0].last_success_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }
}

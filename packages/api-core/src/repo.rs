//! Repository traits, errors, and the D1 SQL statements.
//!
//! The traits live in api-core (pure Rust, unit-testable with fakes); the D1
//! implementations live in `apps/worker/src/db.rs`. SQL is a single source of
//! truth here so it can be reviewed and asserted in tests.
//!
//! Soft-delete rules:
//! - Reads filter `deleted_at IS NULL`.
//! - `TokenRepo::delete` is a soft delete (`UPDATE … SET deleted_at = ?`),
//!   fixing the old Go D1 bug that hard-deleted token rows.
//! - Both upserts set `deleted_at = NULL` on conflict, so a subsequent login
//!   resurrects a soft-deleted user/token row instead of tripping the UNIQUE
//!   constraint.

use async_trait::async_trait;
use thiserror::Error;

use crate::models::{GoogleOAuthToken, NewToken, NewUser, User};

/// Errors surfaced by repository operations.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RepoError {
    #[error("record not found")]
    NotFound,
    #[error("record conflicts with an existing one")]
    Conflict,
    #[error("database error: {0}")]
    Backend(String),
}

/// Identity persistence. No token methods here — tokens are [`TokenRepo`]'s job.
///
/// `#[async_trait(?Send)]`: the returned futures must not require `Send`
/// because the Worker's D1 (`js_sys` promises) and `worker::Fetch` futures are
/// `!Send` on wasm. The `Send + Sync` supertraits still hold — the D1 repos
/// wrap a `D1Database` (unsafe `Send + Sync` in the worker crate).
#[async_trait(?Send)]
pub trait UserRepo: Send + Sync {
    /// Returns the user with `id`, or `None` when absent or soft-deleted.
    async fn get_by_id(&self, id: &str) -> Result<Option<User>, RepoError>;
    /// Returns the user with `google_id`, or `None` when absent or soft-deleted.
    async fn get_by_google_id(&self, google_id: &str) -> Result<Option<User>, RepoError>;
    /// Inserts a new user or updates the existing one (keyed on `google_id`).
    /// Returns the stored row, including the DB-generated `id`.
    async fn upsert_by_google_id(&self, user: NewUser) -> Result<User, RepoError>;
}

/// Google OAuth token persistence.
#[async_trait(?Send)]
pub trait TokenRepo: Send + Sync {
    /// Returns the active token row for `user_id`, or `None` when absent or
    /// soft-deleted. Slice 4's token refresher will consume this.
    async fn get_by_user_id(&self, user_id: &str) -> Result<Option<GoogleOAuthToken>, RepoError>;
    /// Inserts or replaces the token row for `user_id`. A missing/empty
    /// `refresh_token` never blanks a stored one (see `TOKEN_UPSERT_SQL`).
    async fn upsert(&self, token: NewToken) -> Result<(), RepoError>;
    /// SOFT delete: stamps `deleted_at = now_rfc3339` on the active row.
    async fn delete(&self, user_id: &str, now_rfc3339: &str) -> Result<(), RepoError>;
}

pub const USER_GET_BY_ID_SQL: &str = "SELECT * FROM users WHERE id = ? AND deleted_at IS NULL";

pub const USER_GET_BY_GOOGLE_ID_SQL: &str =
    "SELECT * FROM users WHERE google_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `google_id`; `deleted_at = NULL` on conflict resurrects a
/// soft-deleted row so the user can log in again.
pub const USER_UPSERT_SQL: &str = "
    INSERT INTO users (id, google_id, email, name, picture, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(google_id) DO UPDATE SET
        email = excluded.email,
        name = excluded.name,
        picture = excluded.picture,
        updated_at = excluded.updated_at,
        deleted_at = NULL
    RETURNING *
";

/// Fallback for D1 deployments where `RETURNING` misbehaves: update the row
/// found by `get_by_google_id` instead. Mirrors the old Go d1_repo.go fallback.
pub const USER_UPDATE_BY_ID_SQL: &str =
    "UPDATE users SET email = ?, name = ?, picture = ?, updated_at = ? WHERE id = ?";

pub const TOKEN_GET_BY_USER_ID_SQL: &str =
    "SELECT * FROM google_oauth_tokens WHERE user_id = ? AND deleted_at IS NULL";

/// Upsert keyed on `user_id`; `refresh_token` is preserved when the incoming
/// value is NULL or an empty string, and `deleted_at = NULL` on conflict
/// resurrects a soft-deleted row.
pub const TOKEN_UPSERT_SQL: &str = "
    INSERT INTO google_oauth_tokens
        (id, user_id, access_token, refresh_token, expiry, token_type, scope, created_at, updated_at)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
    ON CONFLICT(user_id) DO UPDATE SET
        access_token = excluded.access_token,
        refresh_token = COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token),
        expiry = excluded.expiry,
        scope = excluded.scope,
        token_type = excluded.token_type,
        updated_at = excluded.updated_at,
        deleted_at = NULL
";

/// SOFT delete: the row keeps its UNIQUE `user_id` slot but becomes invisible
/// to `deleted_at IS NULL` reads.
pub const TOKEN_DELETE_SQL: &str =
    "UPDATE google_oauth_tokens SET deleted_at = ?, updated_at = ? WHERE user_id = ? AND deleted_at IS NULL";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_reads_filter_soft_deleted_rows() {
        assert!(USER_GET_BY_ID_SQL.contains("deleted_at IS NULL"), "{USER_GET_BY_ID_SQL}");
        assert!(USER_GET_BY_GOOGLE_ID_SQL.contains("deleted_at IS NULL"));
        assert!(TOKEN_GET_BY_USER_ID_SQL.contains("deleted_at IS NULL"));
    }

    #[test]
    fn token_delete_is_soft_not_hard() {
        assert!(TOKEN_DELETE_SQL.starts_with("UPDATE"), "{TOKEN_DELETE_SQL}");
        assert!(TOKEN_DELETE_SQL.contains("SET deleted_at = ?"), "{TOKEN_DELETE_SQL}");
        assert!(!TOKEN_DELETE_SQL.contains("DELETE FROM"), "{TOKEN_DELETE_SQL}");
    }

    #[test]
    fn token_upsert_preserves_existing_refresh_token() {
        assert!(
            TOKEN_UPSERT_SQL.contains(
                "COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token)"
            ),
            "{TOKEN_UPSERT_SQL}"
        );
    }

    #[test]
    fn upserts_resurrect_soft_deleted_rows() {
        assert!(USER_UPSERT_SQL.contains("deleted_at = NULL"), "{USER_UPSERT_SQL}");
        assert!(TOKEN_UPSERT_SQL.contains("deleted_at = NULL"), "{TOKEN_UPSERT_SQL}");
    }
}

//! Database models.
//!
//! Timestamps are RFC 3339 UTC strings (`TEXT` columns in D1). Nullable
//! columns are `Option<T>`. The `User`/`GoogleOAuthToken` types double as D1
//! row projections: they derive `Deserialize` so `D1PreparedStatement::first`
//! can map rows straight onto them (field names match the schema).

use serde::Deserialize;

/// A user identity as stored in the `users` table.
///
/// Identity only — Google OAuth tokens live in [`GoogleOAuthToken`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub id: String,
    pub google_id: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::UserRepo::upsert_by_google_id`].
///
/// The D1 implementation generates the UUID `id` and the timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    pub google_id: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
}

/// A Google OAuth token as stored in the `google_oauth_tokens` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GoogleOAuthToken {
    pub id: String,
    pub user_id: String,
    pub access_token: String,
    /// `None` when Google only issued an access token (e.g. a later login);
    /// the upsert keeps any previously stored refresh token in that case.
    pub refresh_token: Option<String>,
    /// RFC 3339 UTC instant when the access token expires.
    pub expiry: String,
    pub token_type: String,
    pub scope: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Soft-delete marker; reads filter on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

/// Insert/update input for [`crate::repo::TokenRepo::upsert`].
///
/// The D1 implementation generates the UUID `id` and the timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToken {
    pub user_id: String,
    pub access_token: String,
    /// `None` (or empty) when Google omitted `refresh_token`; the SQL
    /// `COALESCE(NULLIF(...))` keeps the stored value in that case.
    pub refresh_token: Option<String>,
    /// RFC 3339 UTC instant when the access token expires.
    pub expiry: String,
    pub token_type: String,
    pub scope: Option<String>,
}

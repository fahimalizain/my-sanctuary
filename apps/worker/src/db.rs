//! D1-backed implementations of the api-core repository traits.
//!
//! IDs are UUIDv4 strings generated here; timestamps are RFC 3339 UTC strings
//! sourced from `worker::Date::now()` (JS `Date.now()` — never `SystemTime`)
//! and formatted by the shared `api_core::unix_secs_to_rfc3339` helper.
//!
//! Row mapping uses `D1PreparedStatement::first` with serde: `api_core::User`
//! and `api_core::GoogleOAuthToken` derive `Deserialize` with field names that
//! match the D1 schema, so rows deserialize straight onto the models.
//!
//! Binding uses the worker 0.8 `bind_refs` API with `D1Type` values
//! (`Text`/`Null`); nullable columns bind `D1Type::Null`.

use api_core::models::{GoogleOAuthToken, NewToken, NewUser, User};
use api_core::repo::{
    RepoError, TOKEN_DELETE_SQL, TOKEN_GET_BY_USER_ID_SQL, TOKEN_UPSERT_SQL,
    USER_GET_BY_GOOGLE_ID_SQL, USER_GET_BY_ID_SQL, USER_UPDATE_BY_ID_SQL, USER_UPSERT_SQL,
};
use api_core::{TokenRepo, UserRepo};
use worker::{D1Database, D1Type};

/// Current time as an RFC 3339 UTC string, from the JS clock.
fn now_rfc3339() -> String {
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    api_core::unix_secs_to_rfc3339(now_unix)
}

fn backend(err: worker::Error) -> RepoError {
    RepoError::Backend(err.to_string())
}

/// `users` table persistence.
pub struct D1UserRepo {
    db: D1Database,
}

impl D1UserRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `google_oauth_tokens` table persistence.
pub struct D1TokenRepo {
    db: D1Database,
}

impl D1TokenRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl UserRepo for D1UserRepo {
    async fn get_by_id(&self, id: &str) -> Result<Option<User>, RepoError> {
        let stmt = self
            .db
            .prepare(USER_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<User>(None).await.map_err(backend)
    }

    async fn get_by_google_id(&self, google_id: &str) -> Result<Option<User>, RepoError> {
        let stmt = self
            .db
            .prepare(USER_GET_BY_GOOGLE_ID_SQL)
            .bind_refs(&[D1Type::Text(google_id)])
            .map_err(backend)?;
        stmt.first::<User>(None).await.map_err(backend)
    }

    async fn upsert_by_google_id(&self, user: NewUser) -> Result<User, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(USER_UPSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&user.google_id),
                D1Type::Text(&user.email),
                D1Type::Text(&user.name),
                D1Type::Text(user.picture.as_deref().unwrap_or("")),
                D1Type::Text(&now),
                D1Type::Text(&now),
            ])
            .map_err(backend)?;

        match stmt.first::<User>(None).await {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(RepoError::Backend(
                "user upsert returned no rows".to_string(),
            )),
            // D1 `RETURNING` fallback (mirrors the old Go d1_repo.go): read the
            // existing row and update it in place. If the read also fails,
            // surface the original upsert error.
            Err(err) => {
                let existing = self
                    .get_by_google_id(&user.google_id)
                    .await
                    .map_err(|_| RepoError::Backend(err.to_string()))?;
                let existing = existing.ok_or_else(|| {
                    RepoError::Backend("user upsert failed and no existing row found".to_string())
                })?;
                let now = now_rfc3339();
                let stmt = self
                    .db
                    .prepare(USER_UPDATE_BY_ID_SQL)
                    .bind_refs(&[
                        D1Type::Text(&user.email),
                        D1Type::Text(&user.name),
                        D1Type::Text(user.picture.as_deref().unwrap_or("")),
                        D1Type::Text(&now),
                        D1Type::Text(&existing.id),
                    ])
                    .map_err(backend)?;
                stmt.run().await.map_err(backend)?;
                Ok(User {
                    id: existing.id,
                    google_id: user.google_id,
                    email: user.email,
                    name: user.name,
                    picture: user.picture,
                    created_at: existing.created_at,
                    updated_at: now,
                    deleted_at: existing.deleted_at,
                })
            }
        }
    }
}

#[async_trait::async_trait(?Send)]
impl TokenRepo for D1TokenRepo {
    async fn get_by_user_id(&self, user_id: &str) -> Result<Option<GoogleOAuthToken>, RepoError> {
        let stmt = self
            .db
            .prepare(TOKEN_GET_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        stmt.first::<GoogleOAuthToken>(None).await.map_err(backend)
    }

    async fn upsert(&self, token: NewToken) -> Result<(), RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let refresh_token = match token.refresh_token.as_deref() {
            Some(value) => D1Type::Text(value),
            // NULL flows through COALESCE(NULLIF(excluded.refresh_token, ''), …)
            // and keeps any previously stored refresh token.
            None => D1Type::Null,
        };
        let scope = match token.scope.as_deref() {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        };
        let stmt = self
            .db
            .prepare(TOKEN_UPSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&token.user_id),
                D1Type::Text(&token.access_token),
                refresh_token,
                D1Type::Text(&token.expiry),
                D1Type::Text(&token.token_type),
                scope,
                D1Type::Text(&now),
                D1Type::Text(&now),
            ])
            .map_err(backend)?;
        let result = stmt.run().await.map_err(backend)?;
        if !result.success() {
            return Err(RepoError::Backend(result.error().unwrap_or_default()));
        }
        Ok(())
    }

    async fn delete(&self, user_id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TOKEN_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(user_id),
            ])
            .map_err(backend)?;
        let result = stmt.run().await.map_err(backend)?;
        if !result.success() {
            return Err(RepoError::Backend(result.error().unwrap_or_default()));
        }
        Ok(())
    }
}

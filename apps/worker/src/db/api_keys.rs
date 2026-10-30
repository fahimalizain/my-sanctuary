//! D1-backed API key repository.
//!
//! Bind order mirrors `packages/d1-sync/src/api_keys.rs` and the SQL constants
//! live in `api_core::api_key`, so a statement that works locally against the
//! real migrations is the one that runs here. Nothing in this file mints ids or
//! timestamps: the caller stamps `created_at`/`updated_at` from
//! `worker::Date::now()` and passes the frozen RFC 3339 strings in.

use api_core::api_key::{
    ActiveKeyCountRow, ApiKey, ApiKeyOwner, ApiKeyOwnerRow, ApiKeyRepo, NewApiKey,
    API_KEY_AUTHENTICATE_SQL, API_KEY_COUNT_ACTIVE_SQL, API_KEY_INSERT_SQL,
    API_KEY_LIST_BY_USER_ID_SQL, API_KEY_MARK_USED_SQL, API_KEY_SOFT_DELETE_SQL,
};
use api_core::repo::RepoError;
use worker::{D1Database, D1Type};

use super::{backend, query_vec, run_stmt, run_stmt_changes};

/// `api_keys` table persistence.
pub struct D1ApiKeyRepo {
    db: D1Database,
}

impl D1ApiKeyRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// Binds an optional string as `D1Type::Text` or `D1Type::Null`.
fn optional_text(value: Option<&str>) -> D1Type<'_> {
    match value {
        Some(value) => D1Type::Text(value),
        None => D1Type::Null,
    }
}

#[async_trait::async_trait(?Send)]
impl ApiKeyRepo for D1ApiKeyRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<ApiKey>, RepoError> {
        let stmt = self
            .db
            .prepare(API_KEY_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn count_active(&self, user_id: &str) -> Result<i64, RepoError> {
        let stmt = self
            .db
            .prepare(API_KEY_COUNT_ACTIVE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row = stmt
            .first::<ActiveKeyCountRow>(None)
            .await
            .map_err(backend)?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }

    async fn insert(&self, key: NewApiKey) -> Result<ApiKey, RepoError> {
        let expires_on = optional_text(key.expires_on.as_deref());
        let expires_at = optional_text(key.expires_at.as_deref());
        let stmt = self
            .db
            .prepare(API_KEY_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&key.id),
                D1Type::Text(&key.user_id),
                D1Type::Text(&key.name),
                D1Type::Text(&key.key_hash),
                D1Type::Text(&key.key_prefix),
                expires_on,
                expires_at,
                D1Type::Text(&key.created_at),
                D1Type::Text(&key.updated_at),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        // No `RETURNING`: the row is the input plus the two unset columns.
        Ok(ApiKey {
            id: key.id,
            user_id: key.user_id,
            name: key.name,
            key_hash: key.key_hash,
            key_prefix: key.key_prefix,
            expires_on: key.expires_on,
            expires_at: key.expires_at,
            last_used_at: None,
            created_at: key.created_at,
            updated_at: key.updated_at,
            deleted_at: None,
        })
    }

    async fn soft_delete(
        &self,
        id: &str,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<bool, RepoError> {
        let stmt = self
            .db
            .prepare(API_KEY_SOFT_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(user_id),
            ])
            .map_err(backend)?;
        // Zero changes = unknown id, wrong owner, or already revoked.
        Ok(run_stmt_changes(stmt).await? > 0)
    }

    async fn authenticate(
        &self,
        key_hash: &str,
        now_rfc3339: &str,
    ) -> Result<Option<ApiKeyOwner>, RepoError> {
        let stmt = self
            .db
            .prepare(API_KEY_AUTHENTICATE_SQL)
            .bind_refs(&[D1Type::Text(key_hash), D1Type::Text(now_rfc3339)])
            .map_err(backend)?;
        let row = stmt.first::<ApiKeyOwnerRow>(None).await.map_err(backend)?;
        Ok(row.map(ApiKeyOwner::from))
    }

    async fn mark_used(
        &self,
        id: &str,
        cutoff_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(API_KEY_MARK_USED_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
                D1Type::Text(cutoff_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

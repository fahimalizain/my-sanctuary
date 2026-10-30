//! rusqlite implementation of the API-key repo trait.
//!
//! Bind order mirrors `apps/worker/src/db/api_keys.rs`; SQL constants come from
//! `api_core::api_key`. The point of this impl is that `cargo test -p
//! d1-sync --test api_keys_d1` applies the real Worker migrations and
//! round-trips a key through the real schema (the UNIQUE `key_hash`, the
//! partial living-user index, the join's column aliases) without D1.

use api_core::api_key::{
    ActiveKeyCountRow, ApiKey, ApiKeyOwner, ApiKeyOwnerRow, ApiKeyRepo, NewApiKey,
    API_KEY_AUTHENTICATE_SQL, API_KEY_COUNT_ACTIVE_SQL, API_KEY_INSERT_SQL,
    API_KEY_LIST_BY_USER_ID_SQL, API_KEY_MARK_USED_SQL, API_KEY_SOFT_DELETE_SQL,
};
use api_core::repo::RepoError;

use crate::repos::lock;
use crate::{exec, exec_changes, query_one, query_vec, Db};

/// `api_keys` table persistence over one rusqlite connection.
pub struct SqliteApiKeyRepo {
    db: Db,
}

impl SqliteApiKeyRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl ApiKeyRepo for SqliteApiKeyRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<ApiKey>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, API_KEY_LIST_BY_USER_ID_SQL, &[&user_id])
    }

    async fn count_active(&self, user_id: &str) -> Result<i64, RepoError> {
        let conn = lock(&self.db)?;
        let row: Option<ActiveKeyCountRow> =
            query_one(&conn, API_KEY_COUNT_ACTIVE_SQL, &[&user_id])?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }

    async fn insert(&self, key: NewApiKey) -> Result<ApiKey, RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            API_KEY_INSERT_SQL,
            &[
                &key.id,
                &key.user_id,
                &key.name,
                &key.key_hash,
                &key.key_prefix,
                &key.expires_on,
                &key.expires_at,
                &key.created_at,
                &key.updated_at,
            ],
        )?;
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
        let conn = lock(&self.db)?;
        let changes = exec_changes(
            &conn,
            API_KEY_SOFT_DELETE_SQL,
            &[&now_rfc3339, &now_rfc3339, &id, &user_id],
        )?;
        Ok(changes > 0)
    }

    async fn authenticate(
        &self,
        key_hash: &str,
        now_rfc3339: &str,
    ) -> Result<Option<ApiKeyOwner>, RepoError> {
        let conn = lock(&self.db)?;
        let row: Option<ApiKeyOwnerRow> =
            query_one(&conn, API_KEY_AUTHENTICATE_SQL, &[&key_hash, &now_rfc3339])?;
        Ok(row.map(ApiKeyOwner::from))
    }

    async fn mark_used(
        &self,
        id: &str,
        cutoff_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            API_KEY_MARK_USED_SQL,
            &[&now_rfc3339, &id, &cutoff_rfc3339],
        )
    }
}

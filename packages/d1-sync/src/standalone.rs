//! rusqlite implementations of the standalone identity repos (ADR 0012).
//!
//! Bind order mirrors `apps/worker/src/db/mod.rs` (`D1UserRepo`) and the
//! (Phase 3) `D1LocalCredentialRepo`; SQL constants come from
//! `api_core::repo`. The point of these impls is that `cargo test -p d1-sync
//! --test standalone_d1` applies the real Worker migrations and drives
//! [`api_core::standalone::register`] / [`api_core::standalone::login`] over
//! the real `users.integration_type` column, the synthetic `google_id`, and the
//! `local_credentials` UNIQUE `username` — without D1.

use api_core::models::{LocalCredential, NewLocalCredential, NewStandaloneUser, NewUser, User};
use api_core::repo::{
    LocalCredentialRepo, RepoError, UserRepo, LOCAL_CREDENTIAL_GET_BY_USERNAME_SQL,
    LOCAL_CREDENTIAL_INSERT_SQL, LOCAL_CREDENTIAL_UPDATE_PASSWORD_SQL, USER_GET_BY_GOOGLE_ID_SQL,
    USER_GET_BY_ID_SQL, USER_INSERT_STANDALONE_SQL, USER_SET_FOCUSED_TASK_ID_SQL, USER_UPSERT_SQL,
};

use rusqlite::types::ToSql;
use rusqlite::Connection;
use serde::de::DeserializeOwned;

use crate::repos::{lock, now_rfc3339_wall};
use crate::{exec, query_one, row_to_value, Db};

/// `users` table persistence over one rusqlite connection.
pub struct SqliteUserRepo {
    db: Db,
}

impl SqliteUserRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// Maps only a UNIQUE-index violation (extended code 2067 / primary key 1555)
/// to [`RepoError::Conflict`]; every other failure, including NOT NULL and FK
/// violations, stays a [`RepoError::Backend`].
///
/// `create_standalone_account` relies on this: the registration guard is the
/// UNIQUE `local_credentials.username` index, and everything else is a real
/// database error (500), matching the D1 matcher's message check.
fn map_unique(err: rusqlite::Error) -> RepoError {
    if let rusqlite::Error::SqliteFailure(code, _) = &err {
        if code.code == rusqlite::ErrorCode::ConstraintViolation
            && matches!(
                code.extended_code,
                rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                    | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
            )
        {
            return RepoError::Conflict;
        }
    }
    RepoError::Backend(err.to_string())
}

/// Maps a UNIQUE / NOT NULL / FK violation to [`RepoError::Conflict`].
///
/// `create_standalone` and the credential insert rely on the schema as their
/// only race guard (there is no upsert), so the caller must be able to tell a
/// duplicate from a broken database.
fn map_constraint(err: rusqlite::Error) -> RepoError {
    if let rusqlite::Error::SqliteFailure(code, _) = &err {
        if code.code == rusqlite::ErrorCode::ConstraintViolation {
            return RepoError::Conflict;
        }
    }
    RepoError::Backend(err.to_string())
}

/// A `RETURNING *` query whose errors keep the [`RepoError::Conflict`]
/// classification (`query_one` collapses every rusqlite error to `Backend`).
fn insert_returning<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
) -> Result<Option<T>, RepoError> {
    let mut stmt = conn.prepare(sql).map_err(map_constraint)?;
    let mut rows = stmt.query(params).map_err(map_constraint)?;
    match rows.next() {
        Ok(Some(row)) => {
            let value = row_to_value(row).map_err(|err| RepoError::Backend(err.to_string()))?;
            let parsed =
                serde_json::from_value(value).map_err(|err| RepoError::Backend(err.to_string()))?;
            Ok(Some(parsed))
        }
        Ok(None) => Ok(None),
        Err(err) => Err(map_constraint(err)),
    }
}

#[async_trait::async_trait(?Send)]
impl UserRepo for SqliteUserRepo {
    async fn get_by_id(&self, id: &str) -> Result<Option<User>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, USER_GET_BY_ID_SQL, &[&id])
    }

    async fn get_by_google_id(&self, google_id: &str) -> Result<Option<User>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, USER_GET_BY_GOOGLE_ID_SQL, &[&google_id])
    }

    async fn upsert_by_google_id(&self, user: NewUser) -> Result<User, RepoError> {
        let conn = lock(&self.db)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339_wall();
        // Same binds as `D1UserRepo`: the picture falls back to an empty
        // string (never NULL) and `RETURNING *` hands back the stored row.
        let picture = user.picture.as_deref().unwrap_or("");
        let row: Option<User> = insert_returning(
            &conn,
            USER_UPSERT_SQL,
            &[
                &id,
                &user.google_id,
                &user.email,
                &user.name,
                &picture,
                &now,
                &now,
            ],
        )?;
        row.ok_or_else(|| RepoError::Backend("user upsert returned no rows".to_string()))
    }

    async fn create_standalone(&self, user: NewStandaloneUser) -> Result<User, RepoError> {
        let conn = lock(&self.db)?;
        // One plain INSERT (no upsert): a duplicate synthetic id/email is a
        // `Conflict`, never a silent rewrite of an existing account.
        let row: Option<User> = insert_returning(
            &conn,
            USER_INSERT_STANDALONE_SQL,
            &[
                &user.id,
                &user.google_id,
                &user.email,
                &user.name,
                &user.created_at,
                &user.updated_at,
            ],
        )?;
        row.ok_or_else(|| RepoError::Backend("standalone user insert returned no rows".to_string()))
    }

    async fn create_standalone_account(
        &self,
        user: NewStandaloneUser,
        credential: NewLocalCredential,
    ) -> Result<User, RepoError> {
        let mut conn = lock(&self.db)?;
        // Registration is one transaction: a credential insert that fails (a
        // duplicate username, the registration race loser) rolls the `users`
        // row back too. An orphan user has no login handle and its UNIQUE
        // email would block every retry (ADR 0012).
        let tx = conn.transaction().map_err(map_unique)?;
        let row: Option<User> = insert_returning(
            &tx,
            USER_INSERT_STANDALONE_SQL,
            &[
                &user.id,
                &user.google_id,
                &user.email,
                &user.name,
                &user.created_at,
                &user.updated_at,
            ],
        )?;
        let row = row.ok_or_else(|| {
            RepoError::Backend("standalone user insert returned no rows".to_string())
        })?;
        tx.execute(
            LOCAL_CREDENTIAL_INSERT_SQL,
            rusqlite::params![
                credential.user_id,
                credential.username,
                credential.password_hash,
                credential.created_at,
                credential.updated_at,
            ],
        )
        .map_err(map_unique)?;
        tx.commit().map_err(map_unique)?;
        Ok(row)
    }

    async fn set_focused_task_id(
        &self,
        user_id: &str,
        task_id: Option<&str>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            USER_SET_FOCUSED_TASK_ID_SQL,
            &[&task_id, &now_rfc3339, &user_id],
        )
    }
}

/// `local_credentials` table persistence over one rusqlite connection.
pub struct SqliteLocalCredentialRepo {
    db: Db,
}

impl SqliteLocalCredentialRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl LocalCredentialRepo for SqliteLocalCredentialRepo {
    async fn get_by_username(&self, username: &str) -> Result<Option<LocalCredential>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, LOCAL_CREDENTIAL_GET_BY_USERNAME_SQL, &[&username])
    }

    async fn insert(&self, credential: NewLocalCredential) -> Result<LocalCredential, RepoError> {
        let conn = lock(&self.db)?;
        // The UNIQUE `username` is the registration guard: a duplicate is a
        // `Conflict`, never a silent overwrite of another user's hash.
        conn.execute(
            LOCAL_CREDENTIAL_INSERT_SQL,
            rusqlite::params![
                credential.user_id,
                credential.username,
                credential.password_hash,
                credential.created_at,
                credential.updated_at,
            ],
        )
        .map_err(map_constraint)?;
        Ok(credential.into_row())
    }

    async fn update_password(
        &self,
        user_id: &str,
        password_hash: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            LOCAL_CREDENTIAL_UPDATE_PASSWORD_SQL,
            &[&password_hash, &now_rfc3339, &user_id],
        )
    }
}

//! D1-backed `local_credentials` repository (ADR 0012).
//!
//! One row per standalone user; the UNIQUE `username` column is the
//! registration guard. The SQL constants live in `api_core::repo`, so the
//! statements run here are the ones the api-core fake and the d1-sync tests
//! exercise. Nothing in this file hashes a password or mints a timestamp: the
//! caller (`api_core::standalone`) does both and passes the frozen RFC 3339
//! strings in.
//!
//! A duplicate username must surface as [`RepoError::Conflict`] so
//! `api_core::standalone::register` can answer 409 for the loser of a
//! concurrent registration race, not 500.

use api_core::models::{LocalCredential, NewLocalCredential};
use api_core::repo::{
    LocalCredentialRepo, RepoError, LOCAL_CREDENTIAL_GET_BY_USERNAME_SQL,
    LOCAL_CREDENTIAL_INSERT_SQL, LOCAL_CREDENTIAL_UPDATE_PASSWORD_SQL,
};
use worker::{D1Database, D1Type};

use super::{backend, run_stmt};

/// `local_credentials` table persistence.
pub struct D1LocalCredentialRepo {
    db: D1Database,
}

impl D1LocalCredentialRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// Maps a D1 insert failure to [`RepoError::Conflict`] when the UNIQUE
/// `username` index rejected the row (a concurrent registration), and to
/// [`RepoError::Backend`] otherwise.
///
/// `D1PreparedStatement::run` either throws (the JS promise rejects) or
/// resolves with `success: false`; both carry the SQLite message, so both
/// paths go through this matcher.
fn credential_error(message: String) -> RepoError {
    if message.contains("UNIQUE constraint failed") {
        RepoError::Conflict
    } else {
        RepoError::Backend(message)
    }
}

#[async_trait::async_trait(?Send)]
impl LocalCredentialRepo for D1LocalCredentialRepo {
    async fn get_by_username(&self, username: &str) -> Result<Option<LocalCredential>, RepoError> {
        let stmt = self
            .db
            .prepare(LOCAL_CREDENTIAL_GET_BY_USERNAME_SQL)
            .bind_refs(&[D1Type::Text(username)])
            .map_err(backend)?;
        stmt.first::<LocalCredential>(None).await.map_err(backend)
    }

    async fn insert(&self, credential: NewLocalCredential) -> Result<LocalCredential, RepoError> {
        let stmt = self
            .db
            .prepare(LOCAL_CREDENTIAL_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&credential.user_id),
                D1Type::Text(&credential.username),
                D1Type::Text(&credential.password_hash),
                D1Type::Text(&credential.created_at),
                D1Type::Text(&credential.updated_at),
            ])
            .map_err(backend)?;
        let result = stmt
            .run()
            .await
            .map_err(|err| credential_error(err.to_string()))?;
        if !result.success() {
            return Err(credential_error(
                result
                    .error()
                    .unwrap_or_else(|| "local credential insert failed".to_string()),
            ));
        }
        // No `RETURNING`: the row is exactly the values bound above.
        Ok(credential.into_row())
    }

    async fn update_password(
        &self,
        user_id: &str,
        password_hash: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(LOCAL_CREDENTIAL_UPDATE_PASSWORD_SQL)
            .bind_refs(&[
                D1Type::Text(password_hash),
                D1Type::Text(now_rfc3339),
                D1Type::Text(user_id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

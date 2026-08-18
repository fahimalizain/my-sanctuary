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

use api_core::models::{
    CalendarEvent, GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewToken,
    NewUser, NewWatchChannel, User, WatchChannel,
};
use api_core::repo::{
    build_event_upsert_sql, CalendarEventRepo, CalendarRepo, RepoError, TokenRepo, UserRepo,
    WatchChannelRepo, CALENDAR_DELETE_SQL, CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL,
    CALENDAR_GET_BY_ID_SQL, CALENDAR_LIST_BY_USER_ID_SQL, CALENDAR_LIST_SYNC_ENABLED_SQL,
    CALENDAR_SET_SYNC_ENABLED_SQL, CALENDAR_UPDATE_SYNC_STATE_SQL, CALENDAR_UPSERT_SQL,
    EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL, EVENT_DELETE_SQL, EVENT_DELETE_STALE_SQL,
    EVENT_GET_BY_ID_SQL, EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL, EVENT_UPSERT_CHUNK_SIZE,
    TOKEN_DELETE_SQL, TOKEN_GET_BY_USER_ID_SQL, TOKEN_UPSERT_SQL, USER_GET_BY_GOOGLE_ID_SQL,
    USER_GET_BY_ID_SQL, USER_UPDATE_BY_ID_SQL, USER_UPSERT_SQL, WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL,
    WATCH_CHANNEL_DELETE_BY_ID_SQL, WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL, WATCH_CHANNEL_INSERT_SQL,
    WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
};
use worker::{D1Database, D1PreparedStatement, D1Type};

/// Current time as an RFC 3339 UTC string, from the JS clock.
fn now_rfc3339() -> String {
    let now_unix = (worker::Date::now().as_millis() / 1000) as i64;
    api_core::unix_secs_to_rfc3339(now_unix)
}

fn backend(err: worker::Error) -> RepoError {
    RepoError::Backend(err.to_string())
}

/// Runs a statement, surfacing D1's `success`/`error` metadata.
async fn run_stmt(stmt: D1PreparedStatement) -> Result<(), RepoError> {
    let result = stmt.run().await.map_err(backend)?;
    if !result.success() {
        return Err(RepoError::Backend(result.error().unwrap_or_default()));
    }
    Ok(())
}

/// Executes a select and maps every row through serde (`D1Result::results`).
async fn query_vec<T>(stmt: D1PreparedStatement) -> Result<Vec<T>, RepoError>
where
    T: for<'a> serde::Deserialize<'a>,
{
    let result = stmt.all().await.map_err(backend)?;
    result.results::<T>().map_err(backend)
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

/// `google_calendars` table persistence.
pub struct D1CalendarRepo {
    db: D1Database,
}

impl D1CalendarRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `calendar_events` table persistence.
pub struct D1CalendarEventRepo {
    db: D1Database,
}

impl D1CalendarEventRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// `google_calendars_watch_channels` table persistence.
pub struct D1WatchChannelRepo {
    db: D1Database,
}

impl D1WatchChannelRepo {
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
        run_stmt(stmt).await
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarRepo for D1CalendarRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_sync_enabled(&self) -> Result<Vec<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_LIST_SYNC_ENABLED_SQL)
            .bind(&[])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<GoogleCalendar>(None).await.map_err(backend)
    }

    async fn get_by_google_cal_id(
        &self,
        user_id: &str,
        google_cal_id: &str,
    ) -> Result<Option<GoogleCalendar>, RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(google_cal_id)])
            .map_err(backend)?;
        stmt.first::<GoogleCalendar>(None).await.map_err(backend)
    }

    async fn upsert(&self, calendar: NewCalendar) -> Result<(), RepoError> {
        self.upsert_batch(vec![calendar]).await
    }

    async fn upsert_batch(&self, calendars: Vec<NewCalendar>) -> Result<(), RepoError> {
        for calendar in calendars {
            let id = uuid::Uuid::new_v4().to_string();
            let now = now_rfc3339();
            // `is_primary`/`sync_enabled` are INTEGER 0/1 columns in D1.
            let last_synced_at = match calendar.last_synced_at.as_deref() {
                Some(value) => D1Type::Text(value),
                // NULL flows through COALESCE(NULLIF(..., ''), …) and keeps any
                // previously stored value.
                None => D1Type::Null,
            };
            let stmt = self
                .db
                .prepare(CALENDAR_UPSERT_SQL)
                .bind_refs(&[
                    D1Type::Text(&id),
                    D1Type::Text(&calendar.user_id),
                    D1Type::Text(&calendar.google_calendar_id),
                    D1Type::Text(&calendar.summary),
                    D1Type::Text(&calendar.time_zone),
                    D1Type::Integer(i32::from(calendar.is_primary)),
                    D1Type::Text(&calendar.access_role),
                    D1Type::Integer(i32::from(calendar.sync_enabled)),
                    D1Type::Text(&calendar.sync_token),
                    last_synced_at,
                    D1Type::Text(&now),
                    D1Type::Text(&now),
                ])
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }
        Ok(())
    }

    async fn update_sync_state(
        &self,
        id: &str,
        sync_token: &str,
        last_synced_at_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(CALENDAR_UPDATE_SYNC_STATE_SQL)
            .bind_refs(&[
                D1Type::Text(sync_token),
                D1Type::Text(last_synced_at_rfc3339),
                D1Type::Text(&now),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn set_sync_enabled(
        &self,
        id: &str,
        enabled: bool,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_SET_SYNC_ENABLED_SQL)
            .bind_refs(&[
                D1Type::Integer(i32::from(enabled)),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(CALENDAR_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

#[async_trait::async_trait(?Send)]
impl CalendarEventRepo for D1CalendarEventRepo {
    async fn upsert(&self, event: NewCalendarEvent, now_rfc3339: &str) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (sql, args) = build_event_upsert_sql(&[event], now_rfc3339, vec![id.clone()]);
        self.run_upsert(&sql, &args).await?;
        Ok(id)
    }

    async fn upsert_batch(
        &self,
        events: Vec<NewCalendarEvent>,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        // Chunk to stay under D1's 100 bound-parameter limit (7 rows of 13
        // columns per statement); each chunk is one D1 subrequest.
        for chunk in events.chunks(EVENT_UPSERT_CHUNK_SIZE) {
            let ids: Vec<String> = chunk.iter().map(|_| uuid::Uuid::new_v4().to_string()).collect();
            let (sql, args) = build_event_upsert_sql(chunk, now_rfc3339, ids);
            self.run_upsert(&sql, &args).await?;
        }
        Ok(())
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<CalendarEvent>(None).await.map_err(backend)
    }

    async fn list_by_user_id_and_time_range(
        &self,
        user_id: &str,
        start_rfc3339: &str,
        end_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(end_rfc3339),
                D1Type::Text(start_rfc3339),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_by_google_event_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(google_event_id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_stale(
        &self,
        calendar_id: &str,
        older_than_rfc3339: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_DELETE_STALE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(calendar_id),
                D1Type::Text(older_than_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

impl D1CalendarEventRepo {
    /// Binds all-string args as `D1Type::Text` and runs the statement.
    async fn run_upsert(&self, sql: &str, args: &[String]) -> Result<(), RepoError> {
        let refs: Vec<D1Type> = args.iter().map(|arg| D1Type::Text(arg)).collect();
        let stmt = self.db.prepare(sql).bind_refs(&refs).map_err(backend)?;
        run_stmt(stmt).await
    }
}

#[async_trait::async_trait(?Send)]
impl WatchChannelRepo for D1WatchChannelRepo {
    async fn insert(
        &self,
        channel: NewWatchChannel,
        now_rfc3339: &str,
    ) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&channel.calendar_id),
                D1Type::Text(&channel.channel_id),
                D1Type::Text(&channel.resource_id),
                D1Type::Text(&channel.token),
                D1Type::Text(&channel.expiration),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(id)
    }

    async fn get_by_channel_id(&self, channel_id: &str) -> Result<Option<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL)
            .bind_refs(&[D1Type::Text(channel_id)])
            .map_err(backend)?;
        stmt.first::<WatchChannel>(None).await.map_err(backend)
    }

    async fn list_by_calendar_id(&self, calendar_id: &str) -> Result<Vec<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_unexpired_by_calendar_id(
        &self,
        calendar_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<WatchChannel>, RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id), D1Type::Text(now_rfc3339)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn delete_by_id(&self, id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_DELETE_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn delete_by_calendar_id(&self, calendar_id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

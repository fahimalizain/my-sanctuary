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
    CalendarEvent, GoogleCalendar, GoogleOAuthToken, NewCalendar, NewCalendarEvent, NewTask,
    NewTaskCategory, NewTaskCategoryPattern, NewTaskList, NewTaskLog, NewToken, NewUser,
    NewWatchChannel, Task, TaskCategory, TaskCategoryPattern, TaskList, TaskLog, UpdateTask,
    UpdateTaskCategory, UpdateTaskList, User, WatchChannel,
};
use api_core::repo::{
    build_event_upsert_sql, CalendarEventRepo, CalendarRepo, RepoError, TaskCategoryRepo,
    TaskListRepo, TaskLogRepo, TaskRepo, TokenRepo, UserRepo, WatchChannelRepo,
    CALENDAR_DELETE_SQL, CALENDAR_GET_BY_GOOGLE_CAL_ID_SQL, CALENDAR_GET_BY_ID_SQL,
    CALENDAR_LIST_BY_USER_ID_SQL, CALENDAR_LIST_SYNC_ENABLED_SQL,
    CALENDAR_SET_SYNC_ENABLED_SQL, CALENDAR_UPDATE_SYNC_STATE_SQL, CALENDAR_UPSERT_SQL,
    EVENT_DELETE_BY_GOOGLE_EVENT_ID_SQL, EVENT_DELETE_SQL, EVENT_DELETE_STALE_SQL,
    EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL, EVENT_GET_BY_ID_SQL,
    EVENT_LIST_BY_USER_ID_AND_TIME_RANGE_SQL,
    EVENT_LIST_RUNNING_BY_USER_ID_SQL, EVENT_UPSERT_CHUNK_SIZE, TASK_CATEGORY_COUNT_BY_USER_ID_SQL,
    TASK_CATEGORY_COUNT_CHILDREN_SQL, TASK_CATEGORY_DELETE_SQL, TASK_CATEGORY_GET_BY_ID_SQL,
    TASK_CATEGORY_GET_UNTRACKED_SQL, TASK_CATEGORY_INSERT_SQL, TASK_CATEGORY_LIST_BY_USER_ID_SQL,
    TASK_CATEGORY_PATTERNS_DELETE_SQL, TASK_CATEGORY_PATTERNS_INSERT_SQL,
    TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL, TASK_CATEGORY_PATTERNS_LIST_SQL,
    TASK_CATEGORY_UPDATE_SQL, TASK_DELETE_SQL,
    TASK_GET_BY_ID_SQL, TASK_INSERT_SQL, TASK_LIST_BY_USER_ID_SQL, TASK_LIST_IN_PROGRESS_SQL,
    TASK_LIST_COUNT_BY_USER_ID_SQL,
    TASK_LIST_COUNT_ROOT_CATEGORIES_SQL, TASK_LIST_DELETE_SQL, TASK_LIST_GET_BY_ID_SQL,
    TASK_LIST_INSERT_SQL, TASK_LIST_LIST_BY_USER_ID_SQL, TASK_LIST_UPDATE_SQL,
    TASK_MAX_SORT_ORDER_SQL, TASK_SET_SORT_ORDER_SQL, TASK_SHIFT_SORT_ORDER_RANGE_SQL,
    TASK_SHIFT_SORT_ORDER_SQL,
    TASK_UPDATE_SQL, TASK_LOG_INSERT_SQL, TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL,
    TASK_SET_STATUS_SQL,
    TOKEN_DELETE_SQL, TOKEN_GET_BY_USER_ID_SQL,
    TOKEN_UPSERT_SQL, USER_GET_BY_GOOGLE_ID_SQL, USER_GET_BY_ID_SQL, USER_UPDATE_BY_ID_SQL,
    USER_UPSERT_SQL, WATCH_CHANNEL_DELETE_BY_CALENDAR_ID_SQL, WATCH_CHANNEL_DELETE_BY_ID_SQL,
    WATCH_CHANNEL_GET_BY_CHANNEL_ID_SQL, WATCH_CHANNEL_INSERT_SQL, WATCH_CHANNEL_LIST_BY_CALENDAR_ID_SQL,
    WATCH_CHANNEL_LIST_UNEXPIRED_BY_CALENDAR_ID_SQL,
};
use serde::Deserialize;
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
        // Chunk to stay under D1's 100 bound-parameter limit (7 rows of 14
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

    async fn get_by_calendar_and_google_id(
        &self,
        calendar_id: &str,
        google_event_id: &str,
    ) -> Result<Option<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL)
            .bind_refs(&[D1Type::Text(calendar_id), D1Type::Text(google_event_id)])
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

    async fn list_running_by_user_id(
        &self,
        user_id: &str,
        now_rfc3339: &str,
    ) -> Result<Vec<CalendarEvent>, RepoError> {
        let stmt = self
            .db
            .prepare(EVENT_LIST_RUNNING_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(now_rfc3339), D1Type::Text(now_rfc3339)])
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

/// Row projection for `SELECT COUNT(*) AS count` statements.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct CountRow {
    pub count: i64,
}

/// Row projection for the `SELECT sort_order … LIMIT 1` statements (one
/// column only — no point deserializing a full `Task`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct SortOrderRow {
    pub sort_order: i64,
}

/// `task_lists` table persistence.
pub struct D1TaskListRepo {
    db: D1Database,
}

impl D1TaskListRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl TaskListRepo for D1TaskListRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskList>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<TaskList>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<TaskList>(None).await.map_err(backend)
    }

    async fn insert(&self, list: NewTaskList) -> Result<TaskList, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(TASK_LIST_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&list.user_id),
                D1Type::Text(&list.name),
                D1Type::Text(&list.color),
                D1Type::Integer(list.sort_order as i32),
                D1Type::Text(&now),
                D1Type::Text(&now),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(TaskList {
            id: id.clone(),
            user_id: list.user_id,
            name: list.name,
            color: list.color,
            sort_order: list.sort_order,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        })
    }

    async fn update(
        &self,
        id: &str,
        updates: &UpdateTaskList,
    ) -> Result<Option<TaskList>, RepoError> {
        // NULL binds flow through COALESCE and leave the column unchanged.
        let name = match updates.name.as_deref() {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        };
        let color = match updates.color.as_deref() {
            Some(value) => D1Type::Text(value),
            None => D1Type::Null,
        };
        let sort_order = match updates.sort_order {
            Some(value) => D1Type::Integer(value as i32),
            None => D1Type::Null,
        };
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(TASK_LIST_UPDATE_SQL)
            .bind_refs(&[
                name,
                color,
                sort_order,
                D1Type::Text(&now),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        self.get_by_id(id).await
    }

    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_COUNT_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }

    async fn count_root_categories_for_list(&self, list_id: &str) -> Result<i64, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_COUNT_ROOT_CATEGORIES_SQL)
            .bind_refs(&[D1Type::Text(list_id)])
            .map_err(backend)?;
        let row = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }
}

/// `task_categories` + `task_category_patterns` persistence.
pub struct D1TaskCategoryRepo {
    db: D1Database,
}

impl D1TaskCategoryRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl TaskCategoryRepo for D1TaskCategoryRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<TaskCategory>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<TaskCategory>(None).await.map_err(backend)
    }

    async fn insert(&self, category: NewTaskCategory) -> Result<TaskCategory, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        // Nullable columns bind NULL; `is_productive`/`is_untracked` are
        // INTEGER 0/1 columns in D1.
        let list_id = optional_text(category.list_id.as_deref());
        let parent_id = optional_text(category.parent_id.as_deref());
        let google_calendar_id = optional_text(category.google_calendar_id.as_deref());
        let google_color_id = optional_text(category.google_color_id.as_deref());
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&category.user_id),
                list_id,
                parent_id,
                D1Type::Text(&category.title),
                D1Type::Text(&category.slug),
                D1Type::Text(&category.color),
                D1Type::Integer(i32::from(category.is_productive)),
                google_calendar_id,
                google_color_id,
                D1Type::Integer(category.sort_order as i32),
                D1Type::Integer(i32::from(category.is_untracked)),
                D1Type::Text(&now),
                D1Type::Text(&now),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(TaskCategory {
            id: id.clone(),
            user_id: category.user_id,
            list_id: category.list_id,
            parent_id: category.parent_id,
            title: category.title,
            slug: category.slug,
            color: category.color,
            is_productive: category.is_productive,
            google_calendar_id: category.google_calendar_id,
            google_color_id: category.google_color_id,
            sort_order: category.sort_order,
            is_untracked: category.is_untracked,
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        })
    }

    async fn update(
        &self,
        id: &str,
        updates: &UpdateTaskCategory,
    ) -> Result<Option<TaskCategory>, RepoError> {
        // NULL binds flow through COALESCE and leave the column unchanged.
        // `parent_id` is bound twice: once for the COALESCE and once for the
        // CASE that NULLs `list_id` when a new parent is bound (children never
        // store a list_id — they inherit on read).
        let title = optional_text(updates.title.as_deref());
        let slug = optional_text(updates.slug.as_deref());
        let color = optional_text(updates.color.as_deref());
        let is_productive = match updates.is_productive {
            Some(value) => D1Type::Integer(i32::from(value)),
            None => D1Type::Null,
        };
        let google_calendar_id = optional_text(updates.google_calendar_id.as_deref());
        let google_color_id = optional_text(updates.google_color_id.as_deref());
        let sort_order = match updates.sort_order {
            Some(value) => D1Type::Integer(value as i32),
            None => D1Type::Null,
        };
        let parent_id = updates.parent_id.as_deref();
        let list_id = optional_text(updates.list_id.as_deref());
        let now = now_rfc3339();
        // A Vec (not an array) so the `parent_id` binding can be repeated
        // (once for the COALESCE, once for the CASE WHEN ? IS NOT NULL).
        let refs: Vec<D1Type> = vec![
            title,
            slug,
            color,
            is_productive,
            google_calendar_id,
            google_color_id,
            sort_order,
            optional_text(parent_id),
            optional_text(parent_id),
            list_id,
            D1Type::Text(&now),
            D1Type::Text(id),
        ];
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_UPDATE_SQL)
            .bind_refs(&refs)
            .map_err(backend)?;
        run_stmt(stmt).await?;
        self.get_by_id(id).await
    }

    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_COUNT_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }

    async fn count_children(&self, category_id: &str) -> Result<i64, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_COUNT_CHILDREN_SQL)
            .bind_refs(&[D1Type::Text(category_id)])
            .map_err(backend)?;
        let row = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.count).unwrap_or(0))
    }

    async fn get_untracked(&self, user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_GET_UNTRACKED_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        stmt.first::<TaskCategory>(None).await.map_err(backend)
    }

    async fn list_patterns_by_category_id(
        &self,
        category_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_PATTERNS_LIST_SQL)
            .bind_refs(&[D1Type::Text(category_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_patterns_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_PATTERNS_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn replace_patterns(
        &self,
        category_id: &str,
        patterns: Vec<NewTaskCategoryPattern>,
    ) -> Result<(), RepoError> {
        let delete = self
            .db
            .prepare(TASK_CATEGORY_PATTERNS_DELETE_SQL)
            .bind_refs(&[D1Type::Text(category_id)])
            .map_err(backend)?;
        run_stmt(delete).await?;
        let now = now_rfc3339();
        for (sort_order, pattern) in patterns.into_iter().enumerate() {
            let id = uuid::Uuid::new_v4().to_string();
            let stmt = self
                .db
                .prepare(TASK_CATEGORY_PATTERNS_INSERT_SQL)
                .bind_refs(&[
                    D1Type::Text(&id),
                    D1Type::Text(category_id),
                    D1Type::Text(&pattern.regex),
                    optional_text(pattern.google_calendar_id.as_deref()),
                    D1Type::Integer(sort_order as i32),
                    D1Type::Text(&now),
                    D1Type::Text(&now),
                ])
                .map_err(backend)?;
            run_stmt(stmt).await?;
        }
        Ok(())
    }

    async fn delete_patterns_by_category_id(&self, category_id: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TASK_CATEGORY_PATTERNS_DELETE_SQL)
            .bind_refs(&[D1Type::Text(category_id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

/// Binds an optional string as `D1Type::Text` or `D1Type::Null`.
fn optional_text(value: Option<&str>) -> D1Type<'_> {
    match value {
        Some(value) => D1Type::Text(value),
        None => D1Type::Null,
    }
}

/// `tasks` table persistence.
pub struct D1TaskRepo {
    db: D1Database,
}

impl D1TaskRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl TaskRepo for D1TaskRepo {
    async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<Task>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LIST_BY_USER_ID_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn list_in_progress(&self) -> Result<Vec<Task>, RepoError> {
        // The elongate cron's work list: every living IN_PROGRESS row, all
        // users (status is the one-running lock). No binds — `prepare`
        // returns the statement directly when there is nothing to bind.
        query_vec(self.db.prepare(TASK_LIST_IN_PROGRESS_SQL)).await
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<Task>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_GET_BY_ID_SQL)
            .bind_refs(&[D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<Task>(None).await.map_err(backend)
    }

    async fn insert(&self, task: NewTask) -> Result<Task, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_rfc3339();
        // This slice creates tasks in exactly one status; the literal is bound
        // here (the schema's DEFAULT 'OPEN' is a backstop only). The caller
        // (create_task) never shifts peers: `sort_order` is the append rank —
        // 0 on an empty Backlog, otherwise max(sort_order)+1.
        let stmt = self
            .db
            .prepare(TASK_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&task.user_id),
                D1Type::Text(&task.title),
                D1Type::Text(&task.description),
                D1Type::Integer(task.duration_minutes as i32),
                D1Type::Text(&task.priority),
                D1Type::Text(&task.difficulty),
                D1Type::Text(api_core::TASK_STATUS_OPEN),
                D1Type::Integer(task.sort_order as i32),
                D1Type::Text(&now),
                D1Type::Text(&now),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        Ok(Task {
            id: id.clone(),
            user_id: task.user_id,
            title: task.title,
            description: task.description,
            duration_minutes: task.duration_minutes,
            priority: task.priority,
            difficulty: task.difficulty,
            sort_order: task.sort_order,
            status: api_core::TASK_STATUS_OPEN.to_string(),
            created_at: now.clone(),
            updated_at: now,
            deleted_at: None,
        })
    }

    async fn shift_sort_order(
        &self,
        user_id: &str,
        status: &str,
        from_inclusive: i64,
    ) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TASK_SHIFT_SORT_ORDER_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(status),
                D1Type::Integer(from_inclusive as i32),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn shift_sort_order_by(
        &self,
        user_id: &str,
        status: &str,
        from_inclusive: i64,
        to_inclusive: i64,
        delta: i64,
    ) -> Result<(), RepoError> {
        // Signed peer shift over `[from_inclusive, to_inclusive]` — bound
        // both ends so the moving card never shifts itself, regardless of
        // the delta's sign.
        let stmt = self
            .db
            .prepare(TASK_SHIFT_SORT_ORDER_RANGE_SQL)
            .bind_refs(&[
                D1Type::Integer(delta as i32),
                D1Type::Text(user_id),
                D1Type::Text(status),
                D1Type::Integer(from_inclusive as i32),
                D1Type::Integer(to_inclusive as i32),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn update(&self, id: &str, updates: &UpdateTask) -> Result<Option<Task>, RepoError> {
        // NULL binds flow through COALESCE and leave the column unchanged;
        // status is deliberately not bound (this slice has no transitions).
        let title = optional_text(updates.title.as_deref());
        let description = optional_text(updates.description.as_deref());
        let duration_minutes = match updates.duration_minutes {
            Some(value) => D1Type::Integer(value as i32),
            None => D1Type::Null,
        };
        let priority = optional_text(updates.priority.as_deref());
        let difficulty = optional_text(updates.difficulty.as_deref());
        let now = now_rfc3339();
        let stmt = self
            .db
            .prepare(TASK_UPDATE_SQL)
            .bind_refs(&[
                title,
                description,
                duration_minutes,
                priority,
                difficulty,
                D1Type::Text(&now),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        self.get_by_id(id).await
    }

    async fn set_status(
        &self,
        id: &str,
        status: &str,
        now_rfc3339: &str,
    ) -> Result<Option<Task>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_SET_STATUS_SQL)
            .bind_refs(&[D1Type::Text(status), D1Type::Text(now_rfc3339), D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        self.get_by_id(id).await
    }

    async fn set_sort_order(&self, id: &str, sort_order: i64) -> Result<Option<Task>, RepoError> {
        // Placement only: no status, no `updated_at` (re-ranking is not a
        // content change). The fresh row is re-read like `set_status` does.
        let stmt = self
            .db
            .prepare(TASK_SET_SORT_ORDER_SQL)
            .bind_refs(&[D1Type::Integer(sort_order as i32), D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await?;
        self.get_by_id(id).await
    }

    async fn max_sort_order(
        &self,
        user_id: &str,
        status: &str,
        exclude_id: Option<&str>,
    ) -> Result<Option<i64>, RepoError> {
        // The append target: highest living `sort_order` of the pile, `None`
        // when empty (`LIMIT 1` + `first`). `exclude_id` keeps the mover's
        // leftover rank out of its own append target; `unwrap_or("")` binds
        // an empty string for `create_task`, which never matches a UUID.
        // Mirrors the get-by-id reads.
        let stmt = self
            .db
            .prepare(TASK_MAX_SORT_ORDER_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(status),
                D1Type::Text(exclude_id.unwrap_or("")),
            ])
            .map_err(backend)?;
        let row = stmt.first::<SortOrderRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.sort_order))
    }

    async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
        let stmt = self
            .db
            .prepare(TASK_DELETE_SQL)
            .bind_refs(&[
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }
}

/// `task_logs` table persistence (append-only audit trail).
pub struct D1TaskLogRepo {
    db: D1Database,
}

impl D1TaskLogRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl TaskLogRepo for D1TaskLogRepo {
    async fn insert(&self, log: NewTaskLog, now_rfc3339: &str) -> Result<String, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let stmt = self
            .db
            .prepare(TASK_LOG_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&log.task_id),
                D1Type::Text(&log.user_id),
                D1Type::Text(&log.r#type),
                D1Type::Text(&log.at),
                optional_text(log.calendar_id.as_deref()),
                optional_text(log.google_event_id.as_deref()),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        let result = stmt.run().await.map_err(backend)?;
        if !result.success() {
            return Err(RepoError::Backend(result.error().unwrap_or_default()));
        }
        Ok(id)
    }

    async fn latest_started_by_task_id(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskLog>, RepoError> {
        let stmt = self
            .db
            .prepare(TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL)
            .bind_refs(&[D1Type::Text(task_id)])
            .map_err(backend)?;
        stmt.first::<TaskLog>(None).await.map_err(backend)
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

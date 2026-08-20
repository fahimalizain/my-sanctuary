//! Shared API types for the sanctuary Worker.
//!
//! Pure Rust — no `worker` dependency — so it can be unit-tested natively
//! (`cargo test -p api-core`) while still compiling for `wasm32-unknown-unknown`
//! inside `apps/worker`.

pub mod calendar;
pub mod categories;
pub mod google_color;
pub mod lists;
pub mod models;
pub mod oauth;
pub mod repo;
pub mod tasks;
pub mod time;
pub mod token;

mod config;
mod health;
mod session;

pub use calendar::{
    create_event, decide_webhook, ensure_watch, is_public_https_callback, list_calendars,
    list_events, parse_event_time_range, patch_event, renew_watch_if_needed, run_fallback_cron,
    stop_watches_for_calendar, sync_calendar, tokens_match, CalendarError, CalendarEventsResponse,
    CalendarView, CalendarsResponse, CreateEventOutput, CreateEventResponse, CalendarListOutput,
    CronReport, WebhookDecision, CRON_SYNC_STALE_SECS, GOOGLE_CALENDAR_LIST_URL,
    GOOGLE_CHANNELS_STOP_URL, GOOGLE_EVENTS_BASE_URL, SYNC_STALE_THRESHOLD_SECS,
    WATCH_DEFAULT_TTL_SECS, WATCH_RENEW_HORIZON_SECS,
};
pub use config::{
    Config, ConfigError, OAuthConfig, DEFAULT_FRONTEND_URL, MIN_SESSION_SECRET_LEN,
};
pub use google_color::{closest_google_color_id, parse_hex_rgb, GOOGLE_EVENT_COLORS, HexColorError};
pub use health::{HealthResponse, VersionResponse};
pub use lists::{
    create_list, delete_list, list_lists, update_list, DeleteListResponse, ListsError,
    TaskListResponse, TaskListsResponse, SEED_LISTS,
};
pub use categories::{
    classify, create_category, delete_category, ensure_taxonomy, list_categories, slugify,
    update_category, CalendarScope, CategoriesError, CategoriesResponse, CategoryResponse,
    CategoryView, CategoryWithPatterns, ClassifyOutcome, DeleteCategoryResponse, MAX_PATTERN_LEN,
};
pub use oauth::{
    authorization_url, exchange_and_login, generate_state, HttpClient, HttpError, OAuthError,
    GOOGLE_AUTH_URL, GOOGLE_TOKEN_URL, GOOGLE_USERINFO_URL, OAUTH_SCOPES,
};
pub use repo::{
    build_event_upsert_sql, CalendarEventRepo, CalendarRepo, RepoError, TaskCategoryRepo,
    TaskListRepo, TaskLogRepo, TaskRepo, TokenRepo, UserRepo, WatchChannelRepo,
    CALENDAR_LIST_SYNC_ENABLED_SQL, EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL,
    EVENT_UPSERT_CHUNK_SIZE, EVENT_UPSERT_COL_COUNT,
    TASK_CATEGORY_COUNT_BY_USER_ID_SQL, TASK_CATEGORY_COUNT_CHILDREN_SQL,
    TASK_CATEGORY_DELETE_SQL, TASK_CATEGORY_GET_BY_ID_SQL, TASK_CATEGORY_GET_UNTRACKED_SQL,
    TASK_CATEGORY_INSERT_SQL, TASK_CATEGORY_LIST_BY_USER_ID_SQL,
    TASK_CATEGORY_PATTERNS_DELETE_SQL, TASK_CATEGORY_PATTERNS_INSERT_SQL,
    TASK_CATEGORY_PATTERNS_LIST_SQL, TASK_CATEGORY_UPDATE_SQL, TASK_DELETE_SQL,
    TASK_GET_BY_ID_SQL, TASK_INSERT_SQL, TASK_LIST_BY_USER_ID_SQL, TASK_LIST_COUNT_BY_USER_ID_SQL,
    TASK_LIST_COUNT_ROOT_CATEGORIES_SQL, TASK_LIST_DELETE_SQL, TASK_LIST_GET_BY_ID_SQL,
    TASK_LIST_INSERT_SQL, TASK_LIST_LIST_BY_USER_ID_SQL, TASK_LIST_UPDATE_SQL,
    TASK_MAX_SORT_ORDER_SQL, TASK_SET_SORT_ORDER_SQL, TASK_SHIFT_SORT_ORDER_RANGE_SQL,
    TASK_SHIFT_SORT_ORDER_SQL, TASK_UPDATE_SQL, TASK_LIST_IN_PROGRESS_SQL,
    TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL,
};
pub use tasks::{
    classify_title, complete_task, create_task, delete_task, discard_task, list_tasks, move_task,
    pause_task, run_elongate_cron, start_task, stop_task, update_task, ClassifyResponse,
    DeleteTaskResponse, DisplaceInput, ElongateReport, MoveTaskInput, MoveTaskResponse,
    TaskActionResponse, TaskCategorySummary, TaskResponse, TasksError, TasksResponse, TaskView,
    DEFAULT_DURATION_MINUTES, MIN_DURATION_MINUTES, TASK_LOG_COMPLETED, TASK_LOG_DISCARDED,
    TASK_LOG_PAUSED, TASK_LOG_PLANNED, TASK_LOG_REOPENED, TASK_LOG_STARTED, TASK_LOG_STOPPED,
    TASK_LOG_UNPLANNED, TASK_STATUS_COMPLETED, TASK_STATUS_DISCARDED, TASK_STATUS_IN_PROGRESS,
    TASK_STATUS_OPEN, TASK_STATUS_PLANNED,
};
pub use session::{
    clear_session_cookie_header, cookie_value_from_header, seal, session_cookie_header, unseal,
    LogoutResponse, MeResponse, SessionError, SessionUser, SESSION_COOKIE_NAME,
    SESSION_DURATION_SECS,
};
pub use time::{
    ceil_5min_unix_in_zone, nearest_minute_unix, rfc3339_to_unix_secs, unix_secs_to_rfc3339,
};
pub use token::{refresh_if_needed, GoogleAccess, TokenError, REFRESH_SKEW_SECS};

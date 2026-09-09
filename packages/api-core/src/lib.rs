//! Shared API types for the sanctuary Worker.
//!
//! Pure Rust — no `worker` dependency — so it can be unit-tested natively
//! (`cargo test -p api-core`) while still compiling for `wasm32-unknown-unknown`
//! inside `apps/worker`.

pub mod agenda;
pub mod calendar;
pub mod calendar_apply;
pub mod calendar_color;
pub mod calendar_replica;
pub mod calendar_sync;
pub mod categories;
pub mod google_color;
pub mod lists;
pub mod models;
pub mod oauth;
pub mod pattern_gen;
pub mod realtime;
pub mod repo;
pub mod routines;
pub mod tasks;
pub mod time;
pub mod token;

mod config;
mod health;
mod session;

pub use agenda::{
    add_agenda_item, complete_occurrence, delete_agenda_item, get_agenda, move_agenda_item,
    patch_occurrence, pause_occurrence, reopen_occurrence, reschedule_agenda_item,
    run_elongate_occurrences, skip_occurrence, start_occurrence, AgendaError, AgendaItemResponse,
    AgendaItemView,
    AgendaResponse, DeleteAgendaItemResponse, OccurrenceActionResponse, OccurrenceResponse,
    OccurrenceView, AGENDA_KIND_OCCURRENCE, AGENDA_KIND_TASK, OCCURRENCE_STATUS_DONE,
    OCCURRENCE_STATUS_IN_PROGRESS, OCCURRENCE_STATUS_PENDING, OCCURRENCE_STATUS_SKIPPED,
};
pub use calendar::{
    create_event, decide_webhook, delete_event, delete_event_for_user, ensure_watch,
    is_public_https_callback, list_calendars, list_events, parse_event_time_range, patch_event,
    patch_event_fields, renew_watch_if_needed, run_fallback_cron, stop_watches_for_calendar,
    sync_calendar, tokens_match, update_event_for_user, CalendarError, CalendarEventsResponse,
    CalendarListOutput, CalendarView, CalendarsResponse, CreateEventOutput, CreateEventResponse,
    CronReport, DeleteEventResponse, WebhookDecision, CRON_SYNC_STALE_SECS, GOOGLE_CALENDAR_LIST_URL,
    GOOGLE_CHANNELS_STOP_URL, GOOGLE_EVENTS_BASE_URL, SYNC_STALE_THRESHOLD_SECS,
    WATCH_DEFAULT_TTL_SECS, WATCH_RENEW_HORIZON_SECS,
};
pub use calendar_sync::{
    aggregate_sync_status, calendar_sync_view, classify_sync_error, events_sync_envelope,
    next_retry_rfc3339, next_retry_unix, replica_query_fingerprint, replica_state_for_error,
    CalendarReplicaState, CalendarSyncView, EventsSyncEnvelope, SyncAggregateStatus, SyncErrorCode,
    REPLICA_PROJECTION, SYNC_HEALTH_STALE_SECS,
};
pub use calendar_color::{
    calendar_fallback_color, color_for_event_title, paint_events, paint_events_default,
    paint_events_for_user, CalendarEventView,
};
pub use config::{
    Config, ConfigError, OAuthConfig, DEFAULT_FRONTEND_URL, MIN_SESSION_SECRET_LEN,
};
pub use google_color::{
    canonicalize_hex, is_event_label_hex, parse_hex_rgb, snap_to_event_label_hex,
    DEFAULT_EVENT_LABEL_COLOR, GOOGLE_EVENT_LABEL_COLORS, GOOGLE_EVENT_LABEL_NEUTRALS,
    HexColorError, NEUTRAL_CHROMA_THRESHOLD,
};
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
pub use pattern_gen::{
    emit_affixes, extract_hole, fill_regex, split_hole, ExtractError, FillError, HoleSplit,
};
pub use realtime::{RealtimeKind, RealtimeMessage};
pub use routines::{
    create_routine, delete_routine, list_routines, occurrence_dates, update_routine,
    validate_recurrence, DeleteRoutineResponse, RoutineResponse, RoutinesError, RoutinesResponse,
    RoutineView, DEFAULT_ESTIMATED_MINUTES, MIN_ESTIMATED_MINUTES,
};
pub use repo::{
    build_event_upsert_sql, build_occurrence_insert_sql, build_occurrence_list_by_ids_sql,
    build_agenda_item_insert_sql, build_agenda_item_list_by_refs_sql, AgendaItemRepo,
    CalendarEventRepo, CalendarRepo, OccurrenceRepo, RepoError, RoutineRepo, TaskCategoryRepo,
    TaskListRepo, TaskLogRepo, TaskRepo, TokenRepo, UserRepo, WatchChannelRepo,
    AGENDA_ITEM_DELETE_SQL, AGENDA_ITEM_GET_BY_ID_SQL, AGENDA_ITEM_GET_BY_KEY_SQL,
    AGENDA_ITEM_GET_BY_REF_SQL, AGENDA_ITEM_INSERT_SQL, AGENDA_ITEM_INSERT_CHUNK_SIZE,
    AGENDA_ITEM_INSERT_COL_COUNT, AGENDA_ITEM_LIST_BY_USER_AND_DATE_SQL,
    AGENDA_ITEM_LIST_BY_REFS_CHUNK_SIZE, AGENDA_ITEM_MAX_SORT_ORDER_SQL,
    AGENDA_ITEM_SET_LOCAL_DATE_SQL, AGENDA_ITEM_SET_SORT_ORDER_SQL,
    AGENDA_ITEM_SHIFT_SORT_ORDER_SQL,
    CALENDAR_LIST_SYNC_ENABLED_SQL, CALENDAR_RECORD_SYNC_ATTEMPT_SQL,
    CALENDAR_RECORD_SYNC_FAILURE_SQL, CALENDAR_RECORD_SYNC_SUCCESS_IF_OWNER_SQL,
    CALENDAR_RECORD_SYNC_SUCCESS_SQL, CALENDAR_RELEASE_LEASE_SQL,
    CALENDAR_RENEW_LEASE_SQL, CALENDAR_SET_EVENT_LABELS_SQL,
    CALENDAR_TRY_ACQUIRE_LEASE_SQL,
    EVENT_GET_BY_CALENDAR_AND_GOOGLE_ID_SQL, EVENT_GET_ID_BY_NATURAL_KEY_SQL,
    EVENT_UPSERT_CHUNK_SIZE, EVENT_UPSERT_COL_COUNT,
    OCCURRENCE_GET_BY_ID_SQL, OCCURRENCE_GET_BY_ROUTINE_AND_DATE_SQL, OCCURRENCE_INSERT_SQL,
    OCCURRENCE_INSERT_CHUNK_SIZE, OCCURRENCE_INSERT_COL_COUNT,
    OCCURRENCE_LIST_BY_IDS_CHUNK_SIZE, OCCURRENCE_LIST_BY_USER_AND_DATE_SQL,
    OCCURRENCE_LIST_IN_PROGRESS_SQL, OCCURRENCE_CLEAR_EVENT_IDS_SQL, OCCURRENCE_SET_EVENT_IDS_SQL,
    OCCURRENCE_SET_STATUS_SQL, OCCURRENCE_UPDATE_TITLE_SQL,
    ROUTINE_DELETE_SQL, ROUTINE_GET_BY_ID_SQL, ROUTINE_INSERT_SQL,
    ROUTINE_LIST_BY_USER_ID_SQL, ROUTINE_MAX_SORT_ORDER_SQL, ROUTINE_UPDATE_SQL,
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
    TASK_LOG_LATEST_STARTED_BY_TASK_ID_SQL, USER_SET_FOCUSED_TASK_ID_SQL,
};
pub use tasks::{
    classify_title, complete_task, create_task, delete_task, delete_focus, discard_task,
    focus_task, list_tasks, move_task, pause_task, run_elongate_cron, start_task, stop_task,
    update_task, ClassifyResponse, DeleteTaskResponse, ElongateReport, FocusTaskResponse,
    MoveTaskInput, MoveTaskResponse, TaskActionResponse, TaskCategorySummary, TaskResponse,
    TasksError, TasksResponse, TaskView, DEFAULT_DURATION_MINUTES, MIN_DURATION_MINUTES,
    START_EVENT_MINUTES, TASK_LOG_COMPLETED, TASK_LOG_DISCARDED, TASK_LOG_FOCUSED,
    TASK_LOG_PAUSED, TASK_LOG_PLANNED, TASK_LOG_REOPENED, TASK_LOG_STARTED, TASK_LOG_STOPPED,
    TASK_LOG_UNFOCUSED, TASK_LOG_UNPLANNED, TASK_STATUS_COMPLETED, TASK_STATUS_DISCARDED,
    TASK_STATUS_IN_PROGRESS, TASK_STATUS_OPEN, TASK_STATUS_PLANNED,
};
pub use session::{
    clear_session_cookie_header, cookie_value_from_header, seal, session_cookie_header, unseal,
    LogoutResponse, MeResponse, SessionError, SessionUser, SESSION_COOKIE_NAME,
    SESSION_DURATION_SECS,
};
pub use time::{
    ceil_5min_unix_in_zone, civil_date_in_zone, nearest_minute_unix, parse_iana_tz,
    rfc3339_to_unix_secs, unix_secs_to_rfc3339,
};
pub use token::{refresh_if_needed, GoogleAccess, TokenError, REFRESH_SKEW_SECS};

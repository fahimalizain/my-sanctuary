//! Consistency rules: revisioned scoring params, pauses, and a pure on-read
//! scorer over the digest tables (reports slice 3).
//!
//! Nothing here stores a verdict. A rule is a row in `rules`, its **scoring**
//! params live in `rule_revisions` (cosmetic edits never fork params), and
//! pauses are closed `pause_periods` rows or an open-ended
//! `status = paused` + `paused_on`. [`score::score_board`] folds the digest
//! aggregates into strip cells, streaks, compliance, evidence, and the
//! headline, entirely in memory.
//!
//! # Layout
//!
//! * [`params`] — the serde shapes stored in `params_json`, validation, and
//!   the condition / label formatting that needs no database.
//! * [`score`] — the pure scorer (and the tests that pin the model).
//! * [`repo`] — [`repo::RuleRepo`] plus the D1-ready SQL.
//! * [`service`] — CRUD, seeding, digest loading, and the HTTP-facing calls.

pub mod params;
pub mod repo;
pub mod score;
pub mod service;

pub use params::{
    Cat, CategoryCatalog, CategoryDailyAvgParams, CategoryDaysParams, CategorySelector,
    CategoryTotalParams, Comparator, ExcludeIf, RuleKind, RuleParams, RulePhase, RuleStatus,
    SleepDurationParams, SleepMetric, SleepTimingParams, ALL_PRODUCTIVE, MAX_ICON_LEN,
    MAX_NAME_LEN, MAX_REASON_LEN, MAX_WINDOW_DAYS,
};
pub use repo::{
    CategoryRow, CategorySecondsRow, NewRuleRow, PauseRow, RevisionRow, RuleRepo, RuleRow,
    SleepNightRow, PAUSES_DELETE_SQL, PAUSES_INSERT_SQL, PAUSES_LIST_SQL, RULES_COUNT_SQL,
    RULES_GET_SQL, RULES_INSERT_SQL, RULES_LIST_SQL, RULES_SET_SORT_ORDER_SQL, RULES_UPDATE_SQL,
    RULE_CATEGORIES_SQL, RULE_CATEGORY_SECONDS_SQL, RULE_REVISIONS_INSERT_SQL,
    RULE_REVISIONS_LIST_SQL, RULE_SLEEP_NIGHTS_SQL,
};
pub use score::{
    digest_load_floor, rule_phase, score_board, score_rule, ArchiveResponse, BoardInput, DayState,
    DigestFlags, DigestSlice, Evidence, EvidenceDay, EvidenceNight, EvidencePoint,
    EvidenceWeekday, Night, PausePeriod, Revision, RuleScore, RuleSpec, Scoreboard, StripCell,
    EVIDENCE_NIGHTS, EVIDENCE_WEEKDAYS, MAX_LOOKBACK_DAYS, NOT_ENOUGH_NIGHTS, STRIP_DAYS,
};
pub use service::{
    archive_rules, create_global_pause, create_rule, delete_pause, ensure_default_rules,
    list_pauses, pause_rule, reorder_rules, resume_rule, scoreboard, set_rule_archived,
    today, update_rule, NewPauseInput, NewRuleInput, PauseResponse, PauseRuleInput, PauseView,
    PausesResponse, ReorderInput, UpdateRuleInput, SEED_HISTORY_DAYS,
};

use thiserror::Error;

use crate::repo::RepoError;

/// Errors produced by the rules service.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RulesError {
    /// 400 — a bad body, an unknown category, an out-of-range param.
    #[error("{0}")]
    Invalid(String),
    /// 404 — no such rule for this user.
    #[error("rule not found")]
    RuleNotFound,
    /// 404 — no such pause period for this user.
    #[error("pause not found")]
    PauseNotFound,
    /// 409 — the rule is not in a state that allows this transition.
    #[error("{0}")]
    Conflict(String),
    /// 500 — the database failed.
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

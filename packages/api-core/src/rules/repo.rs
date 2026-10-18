//! Consistency-rule persistence: the [`RuleRepo`] trait plus D1-ready SQL.
//!
//! SQL is the single source of truth here (same pattern as
//! [`crate::digest::repo`]) so the statements can be asserted in tests, the
//! Worker's D1 repo and `packages/d1-sync`'s rusqlite repo share them, and
//! d1-sync can apply the real migrations and round-trip a rule.
//!
//! Every statement stays well under D1's 100-bound-parameter limit; these
//! queries are tiny (one user's rules, one date range).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::models::{de_d1_bool, de_empty_string};
use crate::repo::RepoError;

use super::params::{Cat, RuleKind, RuleStatus};
use super::score::RuleSpec;
use super::RulesError;

// ──────────────────────────────────────────
// SQL
// ──────────────────────────────────────────

/// Every living rule of one user, board order.
pub const RULES_LIST_SQL: &str = "
    SELECT id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
           sort_order, created_at, updated_at
    FROM rules
    WHERE user_id = ?
    ORDER BY sort_order, name
";

/// One rule by id, scoped to its owner (404 otherwise).
pub const RULES_GET_SQL: &str = "
    SELECT id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
           sort_order, created_at, updated_at
    FROM rules
    WHERE user_id = ? AND id = ?
";

/// Whether the user has any rule at all — the seed guard.
pub const RULES_COUNT_SQL: &str = "SELECT COUNT(*) AS rules FROM rules WHERE user_id = ?";

/// Binds: id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
/// sort_order, created_at, updated_at (12).
///
/// `OR IGNORE` is the seeding race guard: the eight default rules carry
/// deterministic ids (`seed:{user_id}:{slug}`), so two concurrent scoreboard
/// loads cannot write sixteen rows. A user-created rule still binds a
/// freshly minted uuid and never collides.
pub const RULES_INSERT_SQL: &str = "
    INSERT OR IGNORE INTO rules (
      id, user_id, name, icon, kind, status, starts_on, ends_on, paused_on,
      sort_order, created_at, updated_at
    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
";

/// Rewrites every mutable column. `kind` and `created_at` never change.
/// Binds: name, icon, status, starts_on, ends_on, paused_on, sort_order,
/// updated_at, user_id, id (10).
pub const RULES_UPDATE_SQL: &str = "
    UPDATE rules SET
      name = ?, icon = ?, status = ?, starts_on = ?, ends_on = ?, paused_on = ?,
      sort_order = ?, updated_at = ?
    WHERE user_id = ? AND id = ?
";

/// Binds: sort_order, updated_at, user_id, id (4).
pub const RULES_SET_SORT_ORDER_SQL: &str =
    "UPDATE rules SET sort_order = ?, updated_at = ? WHERE user_id = ? AND id = ?";

/// Every revision of every rule the user owns, oldest first — one query for
/// the whole board instead of one per rule.
///
/// Scoped through `rules.user_id`, so a foreign `rule_id` cannot be read.
pub const RULE_REVISIONS_LIST_SQL: &str = "
    SELECT id, rule_id, effective_from, params_json, created_at
    FROM rule_revisions
    WHERE rule_id IN (SELECT id FROM rules WHERE user_id = ?)
    ORDER BY effective_from, created_at, id
";

/// Binds: id, rule_id, effective_from, params_json, created_at, rule_id,
/// user_id (7).
///
/// `OR IGNORE` plus a caller-supplied id makes a seed's first revision
/// idempotent: two concurrent seed passes write the same `seed:…:v1` row
/// once. A `PATCH` revision binds a fresh uuid and never collides.
///
/// The `WHERE ? IN (SELECT id FROM rules WHERE user_id = ?)` clause is the
/// user-scope check: a revision can only be written under a rule the user
/// owns, and a foreign `rule_id` inserts nothing.
pub const RULE_REVISIONS_INSERT_SQL: &str = "
    INSERT OR IGNORE INTO rule_revisions (id, rule_id, effective_from, params_json, created_at)
    SELECT ?, ?, ?, ?, ?
    WHERE ? IN (SELECT id FROM rules WHERE user_id = ?)
";

/// Every pause period of one user (global and per-rule).
pub const PAUSES_LIST_SQL: &str = "
    SELECT id, user_id, rule_id, from_date, to_date, reason, created_at
    FROM pause_periods
    WHERE user_id = ?
    ORDER BY from_date, created_at, id
";

/// Binds: id, user_id, rule_id, from_date, to_date, reason, created_at (7).
pub const PAUSES_INSERT_SQL: &str = "
    INSERT INTO pause_periods (id, user_id, rule_id, from_date, to_date, reason, created_at)
    VALUES (?, ?, ?, ?, ?, ?, ?)
";

/// Binds: user_id, id (2).
pub const PAUSES_DELETE_SQL: &str = "DELETE FROM pause_periods WHERE user_id = ? AND id = ?";

/// The digest rows a scoreboard reads. `from_date`/`to_date` are inclusive.
pub const RULE_CATEGORY_SECONDS_SQL: &str = "
    SELECT local_date, category_id, seconds
    FROM daily_category_minutes
    WHERE user_id = ? AND local_date >= ? AND local_date <= ?
    ORDER BY local_date, category_id
";

/// Paired nights only: `est_*` rows never score (see the slice-2 brief). The
/// pure scorer filters on `source` again, so the rule survives a hand-built
/// slice.
pub const RULE_SLEEP_NIGHTS_SQL: &str = "
    SELECT wake_date, asleep_at, bed_min_past_noon, wake_min_past_midnight, seconds, source
    FROM sleep_nights
    WHERE user_id = ? AND source = 'paired' AND wake_date >= ? AND wake_date <= ?
    ORDER BY wake_date, asleep_at
";

/// The living category tree, in the order "first match wins" expects.
pub const RULE_CATEGORIES_SQL: &str = "
    SELECT id, parent_id, is_productive, slug, title
    FROM task_categories
    WHERE user_id = ? AND deleted_at IS NULL
    ORDER BY sort_order, title
";

/// The civil dates the digest has aggregated, over the inclusive range.
///
/// A window is only full when every non-paused day in it appears here: a
/// missing day means the backfill has not reached it yet, which is not the
/// same as a stored zero.
pub const RULE_AGGREGATED_DAYS_SQL: &str = "
    SELECT local_date
    FROM daily_meta
    WHERE user_id = ? AND local_date >= ? AND local_date <= ?
    ORDER BY local_date
";

// ──────────────────────────────────────────
// Rows
// ──────────────────────────────────────────

/// One `rules` row (also the D1/rusqlite row projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub icon: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub kind: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub status: String,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub paused_on: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl RuleRow {
    /// The pure scorer's view of this row.
    ///
    /// An unknown `kind` or `status` is corrupt stored data, not a bad request:
    /// the caller skips the row and the Worker answers 500 for a lone write.
    pub fn spec(&self) -> Result<RuleSpec, RulesError> {
        let kind = RuleKind::parse(&self.kind)
            .ok_or_else(|| RulesError::Corrupt(format!("rule {} has an unknown kind", self.id)))?;
        let status = RuleStatus::parse(&self.status).ok_or_else(|| {
            RulesError::Corrupt(format!("rule {} has an unknown status", self.id))
        })?;
        Ok(RuleSpec {
            id: self.id.clone(),
            name: self.name.clone(),
            icon: self.icon.clone(),
            kind,
            status,
            starts_on: self.starts_on.clone(),
            ends_on: self.ends_on.clone(),
            paused_on: self.paused_on.clone(),
            sort_order: self.sort_order,
        })
    }
}

/// The columns a new rule needs; the repo mints the id and timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRuleRow {
    /// A deterministic id (the seeds use `seed:{user_id}:{slug}`), or `None`
    /// for a freshly minted uuid. An id that already exists is ignored by
    /// `INSERT OR IGNORE`, which is what makes a double seed harmless.
    pub id: Option<String>,
    pub user_id: String,
    pub name: String,
    pub icon: String,
    pub kind: String,
    pub status: String,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub paused_on: Option<String>,
    pub sort_order: i64,
}

/// One `rule_revisions` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionRow {
    pub id: String,
    pub rule_id: String,
    pub effective_from: String,
    pub params_json: String,
    pub created_at: String,
}

/// One `pause_periods` row (`rule_id` NULL = global).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseRow {
    pub id: String,
    pub user_id: String,
    pub rule_id: Option<String>,
    pub from_date: String,
    pub to_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub reason: String,
    pub created_at: String,
}

/// One `task_categories` row, as rules see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryRow {
    pub id: String,
    pub parent_id: Option<String>,
    #[serde(default, deserialize_with = "de_d1_bool")]
    pub is_productive: bool,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub slug: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub title: String,
}

impl From<CategoryRow> for Cat {
    fn from(row: CategoryRow) -> Self {
        Cat {
            id: row.id,
            parent_id: row.parent_id,
            is_productive: row.is_productive,
            slug: row.slug,
            title: row.title,
        }
    }
}

/// One `daily_category_minutes` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategorySecondsRow {
    pub local_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub category_id: String,
    pub seconds: i64,
}

/// One `sleep_nights` row (paired only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SleepNightRow {
    pub wake_date: String,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub asleep_at: String,
    pub bed_min_past_noon: i64,
    pub wake_min_past_midnight: i64,
    pub seconds: i64,
    #[serde(default, deserialize_with = "de_empty_string")]
    pub source: String,
}

// ──────────────────────────────────────────
// The trait
// ──────────────────────────────────────────

/// Persistence for `rules` / `rule_revisions` / `pause_periods` plus the three
/// digest reads the scorer needs.
///
/// Two implementations exist: `apps/worker/src/db/rules.rs` (`D1RuleRepo`,
/// production) and `packages/d1-sync/src/rules.rs` (`SqliteRuleRepo`, which
/// applies the real migrations in `cargo test -p d1-sync`).
///
/// Ids and timestamps are minted by the implementation: `api-core` has no uuid
/// dependency (the same reason `TaskCategoryRepo::insert` takes a `NewTaskCategory`).
#[async_trait(?Send)]
pub trait RuleRepo: Send + Sync {
    /// Every rule of the user, `sort_order` then name.
    async fn list_rules(&self, user_id: &str) -> Result<Vec<RuleRow>, RepoError>;
    /// One rule, scoped to its owner.
    async fn get_rule(&self, user_id: &str, id: &str) -> Result<Option<RuleRow>, RepoError>;
    /// How many rules the user has (archived included) — the seed guard.
    async fn count_rules(&self, user_id: &str) -> Result<i64, RepoError>;
    /// Inserts a rule and returns the stored row (with its minted id).
    ///
    /// `rule.id` is honoured when set — that is how the seeds stay
    /// deterministic — and an id that already exists is **ignored**, so the
    /// same seed written twice leaves one row.
    async fn insert_rule(
        &self,
        rule: &NewRuleRow,
        now_rfc3339: &str,
    ) -> Result<RuleRow, RepoError>;
    /// Rewrites every mutable column of an existing rule.
    async fn update_rule(&self, rule: &RuleRow, now_rfc3339: &str) -> Result<(), RepoError>;
    /// Cosmetic reorder (no revision).
    async fn set_sort_order(
        &self,
        user_id: &str,
        id: &str,
        sort_order: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Every revision of every rule of the user, oldest first — one query.
    async fn list_revisions(&self, user_id: &str) -> Result<Vec<RevisionRow>, RepoError>;
    /// Appends a revision to one of the user's rules. A foreign `rule_id`
    /// writes nothing (the statement is scoped through `rules.user_id`).
    ///
    /// `id` is honoured when set — the seeds use it so a concurrent double
    /// seed writes one row — and an id that already exists is ignored.
    async fn insert_revision(
        &self,
        user_id: &str,
        rule_id: &str,
        id: Option<&str>,
        effective_from: &str,
        params_json: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError>;
    /// Every pause period of the user (global and per-rule).
    async fn list_pauses(&self, user_id: &str) -> Result<Vec<PauseRow>, RepoError>;
    /// Inserts one closed pause period.
    async fn insert_pause(
        &self,
        user_id: &str,
        rule_id: Option<&str>,
        from_date: &str,
        to_date: &str,
        reason: &str,
        now_rfc3339: &str,
    ) -> Result<PauseRow, RepoError>;
    /// Deletes one pause period owned by the user.
    async fn delete_pause(&self, user_id: &str, id: &str) -> Result<(), RepoError>;
    /// `daily_category_minutes` over the inclusive date range.
    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<CategorySecondsRow>, RepoError>;
    /// `sleep_nights` (`source = 'paired'`) over the inclusive wake-date range.
    async fn load_sleep_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<SleepNightRow>, RepoError>;
    /// The `daily_meta.local_date`s the digest has aggregated, oldest first.
    ///
    /// The scorer needs the *set*, not the rows: a day in a window that is
    /// missing here is not a zero, it is a hole the backfill has not filled.
    async fn load_aggregated_days(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<String>, RepoError>;
    /// The user's living category tree.
    async fn load_categories(&self, user_id: &str) -> Result<Vec<CategoryRow>, RepoError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// m9: a revision can only be read or written under a rule the user owns.
    /// Every revision statement has to carry `user_id` itself — the trait no
    /// longer takes a bare `rule_id`.
    #[test]
    fn revision_statements_are_scoped_by_user_id() {
        assert!(RULE_REVISIONS_LIST_SQL.contains("user_id"), "{RULE_REVISIONS_LIST_SQL}");
        assert!(
            RULE_REVISIONS_LIST_SQL.contains("rule_id IN (SELECT id FROM rules WHERE user_id = ?)"),
            "{RULE_REVISIONS_LIST_SQL}"
        );
        assert!(RULE_REVISIONS_INSERT_SQL.contains("user_id"), "{RULE_REVISIONS_INSERT_SQL}");
        assert!(
            RULE_REVISIONS_INSERT_SQL.contains("(SELECT id FROM rules WHERE user_id = ?)"),
            "{RULE_REVISIONS_INSERT_SQL}"
        );
    }

    /// The seed race guard: the insert has to tolerate an id that already
    /// exists.
    #[test]
    fn the_rule_insert_ignores_a_duplicate_id() {
        assert!(RULES_INSERT_SQL.contains("INSERT OR IGNORE"), "{RULES_INSERT_SQL}");
    }

    /// The aggregated-day read is one user-scoped range scan.
    #[test]
    fn the_aggregated_day_read_is_a_user_scoped_range() {
        assert!(RULE_AGGREGATED_DAYS_SQL.contains("daily_meta"), "{RULE_AGGREGATED_DAYS_SQL}");
        assert!(RULE_AGGREGATED_DAYS_SQL.contains("user_id = ?"), "{RULE_AGGREGATED_DAYS_SQL}");
        assert!(RULE_AGGREGATED_DAYS_SQL.contains("local_date >= ?"), "{RULE_AGGREGATED_DAYS_SQL}");
        assert!(RULE_AGGREGATED_DAYS_SQL.contains("local_date <= ?"), "{RULE_AGGREGATED_DAYS_SQL}");
    }
}

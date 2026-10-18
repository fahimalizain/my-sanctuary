//! rusqlite implementation of the rules repo trait.
//!
//! Bind order mirrors `apps/worker/src/db/rules.rs`; SQL constants come from
//! `api_core::rules::repo`. The point of this impl is that
//! `cargo test -p d1-sync` can apply the real Worker migrations and round-trip
//! a rule through the real schema (affinity, indexes, the 100-bind limit),
//! then score a scoreboard through the api-core service without D1.

use api_core::repo::RepoError;
use api_core::rules::repo::{
    CategoryRow, CategorySecondsRow, NewRuleRow, PauseRow, RevisionRow, RuleRepo, RuleRow,
    SleepNightRow, RULES_COUNT_SQL, RULES_GET_SQL, RULES_INSERT_SQL, RULES_LIST_SQL,
    RULES_SET_SORT_ORDER_SQL, RULES_UPDATE_SQL, RULE_AGGREGATED_DAYS_SQL, RULE_CATEGORIES_SQL,
    RULE_CATEGORY_SECONDS_SQL, RULE_REVISIONS_INSERT_SQL, RULE_REVISIONS_LIST_SQL,
    RULE_SLEEP_NIGHTS_SQL, PAUSES_DELETE_SQL, PAUSES_INSERT_SQL, PAUSES_LIST_SQL,
};
use rusqlite::Connection;
use serde::Deserialize;

use crate::repos::lock;
use crate::{exec, query_one, query_vec, Db};

/// `rules` / `rule_revisions` / `pause_periods` plus the digest reads the
/// scorer needs, over one rusqlite connection.
pub struct SqliteRuleRepo {
    db: Db,
}

impl SqliteRuleRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait(?Send)]
impl RuleRepo for SqliteRuleRepo {
    async fn list_rules(&self, user_id: &str) -> Result<Vec<RuleRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RULES_LIST_SQL, &[&user_id])
    }

    async fn get_rule(&self, user_id: &str, id: &str) -> Result<Option<RuleRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_one(&conn, RULES_GET_SQL, &[&user_id, &id])
    }

    async fn count_rules(&self, user_id: &str) -> Result<i64, RepoError> {
        #[derive(Deserialize)]
        struct CountRow {
            rules: i64,
        }
        let conn = lock(&self.db)?;
        let row: Option<CountRow> = query_one(&conn, RULES_COUNT_SQL, &[&user_id])?;
        Ok(row.map(|row| row.rules).unwrap_or(0))
    }

    async fn insert_rule(
        &self,
        rule: &NewRuleRow,
        now_rfc3339: &str,
    ) -> Result<RuleRow, RepoError> {
        // A seed carries its own deterministic id; a user-created rule gets a
        // fresh uuid. `INSERT OR IGNORE` drops a duplicate id silently.
        let id = rule
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let conn = lock(&self.db)?;
        exec(
            &conn,
            RULES_INSERT_SQL,
            &[
                &id,
                &rule.user_id,
                &rule.name,
                &rule.icon,
                &rule.kind,
                &rule.status,
                &rule.starts_on,
                &rule.ends_on,
                &rule.paused_on,
                &rule.sort_order,
                &now_rfc3339,
                &now_rfc3339,
            ],
        )?;
        Ok(RuleRow {
            id,
            user_id: rule.user_id.clone(),
            name: rule.name.clone(),
            icon: rule.icon.clone(),
            kind: rule.kind.clone(),
            status: rule.status.clone(),
            starts_on: rule.starts_on.clone(),
            ends_on: rule.ends_on.clone(),
            paused_on: rule.paused_on.clone(),
            sort_order: rule.sort_order,
            created_at: now_rfc3339.to_string(),
            updated_at: now_rfc3339.to_string(),
        })
    }

    async fn update_rule(&self, rule: &RuleRow, now_rfc3339: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            RULES_UPDATE_SQL,
            &[
                &rule.name,
                &rule.icon,
                &rule.status,
                &rule.starts_on,
                &rule.ends_on,
                &rule.paused_on,
                &rule.sort_order,
                &now_rfc3339,
                &rule.user_id,
                &rule.id,
            ],
        )
    }

    async fn set_sort_order(
        &self,
        user_id: &str,
        id: &str,
        sort_order: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(
            &conn,
            RULES_SET_SORT_ORDER_SQL,
            &[&sort_order, &now_rfc3339, &user_id, &id],
        )
    }

    async fn list_revisions(&self, user_id: &str) -> Result<Vec<RevisionRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RULE_REVISIONS_LIST_SQL, &[&user_id])
    }

    async fn insert_revision(
        &self,
        user_id: &str,
        rule_id: &str,
        id: Option<&str>,
        effective_from: &str,
        params_json: &str,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let minted = id.map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let conn = lock(&self.db)?;
        exec(
            &conn,
            RULE_REVISIONS_INSERT_SQL,
            &[
                &minted,
                &rule_id,
                &effective_from,
                &params_json,
                &now_rfc3339,
                &rule_id,
                &user_id,
            ],
        )
    }

    async fn list_pauses(&self, user_id: &str) -> Result<Vec<PauseRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, PAUSES_LIST_SQL, &[&user_id])
    }

    async fn insert_pause(
        &self,
        user_id: &str,
        rule_id: Option<&str>,
        from_date: &str,
        to_date: &str,
        reason: &str,
        now_rfc3339: &str,
    ) -> Result<PauseRow, RepoError> {
        let id = uuid::Uuid::new_v4().to_string();
        let conn = lock(&self.db)?;
        exec(
            &conn,
            PAUSES_INSERT_SQL,
            &[
                &id,
                &user_id,
                &rule_id,
                &from_date,
                &to_date,
                &reason,
                &now_rfc3339,
            ],
        )?;
        Ok(PauseRow {
            id,
            user_id: user_id.to_string(),
            rule_id: rule_id.map(|value| value.to_string()),
            from_date: from_date.to_string(),
            to_date: to_date.to_string(),
            reason: reason.to_string(),
            created_at: now_rfc3339.to_string(),
        })
    }

    async fn delete_pause(&self, user_id: &str, id: &str) -> Result<(), RepoError> {
        let conn = lock(&self.db)?;
        exec(&conn, PAUSES_DELETE_SQL, &[&user_id, &id])
    }

    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<CategorySecondsRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RULE_CATEGORY_SECONDS_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }

    async fn load_sleep_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<SleepNightRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(
            &conn,
            RULE_SLEEP_NIGHTS_SQL,
            &[&user_id, &from_date, &to_date],
        )
    }

    async fn load_aggregated_days(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<String>, RepoError> {
        #[derive(Deserialize)]
        struct DateRow {
            local_date: String,
        }
        let conn = lock(&self.db)?;
        let rows: Vec<DateRow> = query_vec(
            &conn,
            RULE_AGGREGATED_DAYS_SQL,
            &[&user_id, &from_date, &to_date],
        )?;
        Ok(rows.into_iter().map(|row| row.local_date).collect())
    }

    async fn load_categories(&self, user_id: &str) -> Result<Vec<CategoryRow>, RepoError> {
        let conn = lock(&self.db)?;
        query_vec(&conn, RULE_CATEGORIES_SQL, &[&user_id])
    }
}

/// Seeds one living category straight into `task_categories` (test helper).
pub fn seed_category(
    conn: &Connection,
    user_id: &str,
    id: &str,
    slug: &str,
    title: &str,
    is_productive: bool,
) -> Result<(), RepoError> {
    exec(
        conn,
        "INSERT INTO task_categories (
           id, user_id, list_id, parent_id, title, slug, color, is_productive,
           google_calendar_id, sort_order, is_untracked, created_at, updated_at
         ) VALUES (?, ?, NULL, NULL, ?, ?, '#000000', ?, NULL, 0, 0, ?, ?)",
        &[
            &id,
            &user_id,
            &title,
            &slug,
            &if is_productive { 1_i64 } else { 0_i64 },
            &"2026-01-01T00:00:00Z",
            &"2026-01-01T00:00:00Z",
        ],
    )
}

/// Seeds one day of category seconds straight into `daily_category_minutes`
/// (test helper). `category_id = ''` is unclassified.
pub fn seed_category_seconds(
    conn: &Connection,
    user_id: &str,
    local_date: &str,
    category_id: &str,
    seconds: i64,
) -> Result<(), RepoError> {
    exec(
        conn,
        "INSERT INTO daily_category_minutes (user_id, local_date, category_id, seconds)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(user_id, local_date, category_id) DO UPDATE SET seconds = excluded.seconds",
        &[&user_id, &local_date, &category_id, &seconds],
    )
}

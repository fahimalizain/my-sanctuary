//! D1-backed rules repository (reports slice 3).
//!
//! Bind order mirrors `packages/d1-sync/src/rules.rs` and the SQL constants
//! live in `api_core::rules::repo`, so a statement that works locally against
//! the real migrations is the one that runs here.
//!
//! Ids are UUIDv4 strings minted here and timestamps are RFC 3339 UTC strings
//! from `worker::Date::now()` — `api-core` has no uuid dependency and never
//! reads the clock.

use api_core::repo::RepoError;
use api_core::rules::repo::{
    CategoryRow, CategorySecondsRow, NewRuleRow, PauseRow, RevisionRow, RuleRepo, RuleRow,
    SleepNightRow, RULES_COUNT_SQL, RULES_GET_SQL, RULES_INSERT_SQL, RULES_LIST_SQL,
    RULES_SET_SORT_ORDER_SQL, RULES_UPDATE_SQL, RULE_AGGREGATED_DAYS_SQL, RULE_CATEGORIES_SQL,
    RULE_CATEGORY_SECONDS_SQL, RULE_REVISIONS_INSERT_SQL, RULE_REVISIONS_LIST_SQL,
    RULE_SLEEP_NIGHTS_SQL, PAUSES_DELETE_SQL, PAUSES_INSERT_SQL, PAUSES_LIST_SQL,
};
use serde::Deserialize;
use worker::{D1Database, D1Type};

use super::{backend, query_vec, run_stmt};

/// `rules` / `rule_revisions` / `pause_periods` plus the digest reads the
/// scorer needs.
pub struct D1RuleRepo {
    db: D1Database,
}

impl D1RuleRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

fn optional_text(value: Option<&str>) -> D1Type<'_> {
    match value {
        Some(value) => D1Type::Text(value),
        None => D1Type::Null,
    }
}

#[async_trait::async_trait(?Send)]
impl RuleRepo for D1RuleRepo {
    async fn list_rules(&self, user_id: &str) -> Result<Vec<RuleRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULES_LIST_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn get_rule(&self, user_id: &str, id: &str) -> Result<Option<RuleRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULES_GET_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(id)])
            .map_err(backend)?;
        stmt.first::<RuleRow>(None).await.map_err(backend)
    }

    async fn count_rules(&self, user_id: &str) -> Result<i64, RepoError> {
        #[derive(Deserialize)]
        struct CountRow {
            rules: i64,
        }
        let stmt = self
            .db
            .prepare(RULES_COUNT_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<CountRow> = stmt.first::<CountRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.rules).unwrap_or(0))
    }

    async fn insert_rule(
        &self,
        rule: &NewRuleRow,
        now_rfc3339: &str,
    ) -> Result<RuleRow, RepoError> {
        // A seed carries its own deterministic id; a user-created rule gets a
        // fresh uuid. `INSERT OR IGNORE` drops a duplicate id silently, which
        // is what makes a concurrent double seed harmless.
        let id = rule
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let sort_order = rule.sort_order.to_string();
        let stmt = self
            .db
            .prepare(RULES_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(&rule.user_id),
                D1Type::Text(&rule.name),
                D1Type::Text(&rule.icon),
                D1Type::Text(&rule.kind),
                D1Type::Text(&rule.status),
                D1Type::Text(&rule.starts_on),
                optional_text(rule.ends_on.as_deref()),
                optional_text(rule.paused_on.as_deref()),
                D1Type::Text(&sort_order),
                D1Type::Text(now_rfc3339),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
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
        let sort_order = rule.sort_order.to_string();
        let stmt = self
            .db
            .prepare(RULES_UPDATE_SQL)
            .bind_refs(&[
                D1Type::Text(&rule.name),
                D1Type::Text(&rule.icon),
                D1Type::Text(&rule.status),
                D1Type::Text(&rule.starts_on),
                optional_text(rule.ends_on.as_deref()),
                optional_text(rule.paused_on.as_deref()),
                D1Type::Text(&sort_order),
                D1Type::Text(now_rfc3339),
                D1Type::Text(&rule.user_id),
                D1Type::Text(&rule.id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn set_sort_order(
        &self,
        user_id: &str,
        id: &str,
        sort_order: i64,
        now_rfc3339: &str,
    ) -> Result<(), RepoError> {
        let sort_order = sort_order.to_string();
        let stmt = self
            .db
            .prepare(RULES_SET_SORT_ORDER_SQL)
            .bind_refs(&[
                D1Type::Text(&sort_order),
                D1Type::Text(now_rfc3339),
                D1Type::Text(user_id),
                D1Type::Text(id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn list_revisions(&self, user_id: &str) -> Result<Vec<RevisionRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULE_REVISIONS_LIST_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
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
        let stmt = self
            .db
            .prepare(RULE_REVISIONS_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&minted),
                D1Type::Text(rule_id),
                D1Type::Text(effective_from),
                D1Type::Text(params_json),
                D1Type::Text(now_rfc3339),
                D1Type::Text(rule_id),
                D1Type::Text(user_id),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn list_pauses(&self, user_id: &str) -> Result<Vec<PauseRow>, RepoError> {
        let stmt = self
            .db
            .prepare(PAUSES_LIST_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
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
        let stmt = self
            .db
            .prepare(PAUSES_INSERT_SQL)
            .bind_refs(&[
                D1Type::Text(&id),
                D1Type::Text(user_id),
                optional_text(rule_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
                D1Type::Text(reason),
                D1Type::Text(now_rfc3339),
            ])
            .map_err(backend)?;
        run_stmt(stmt).await?;
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
        let stmt = self
            .db
            .prepare(PAUSES_DELETE_SQL)
            .bind_refs(&[D1Type::Text(user_id), D1Type::Text(id)])
            .map_err(backend)?;
        run_stmt(stmt).await
    }

    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<CategorySecondsRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULE_CATEGORY_SECONDS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_sleep_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<SleepNightRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULE_SLEEP_NIGHTS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
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
        let stmt = self
            .db
            .prepare(RULE_AGGREGATED_DAYS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        let rows: Vec<DateRow> = query_vec(stmt).await?;
        Ok(rows.into_iter().map(|row| row.local_date).collect())
    }

    async fn load_categories(&self, user_id: &str) -> Result<Vec<CategoryRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RULE_CATEGORIES_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }
}

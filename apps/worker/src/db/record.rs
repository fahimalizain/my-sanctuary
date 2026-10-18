//! D1-backed record repository (reports slice 5).
//!
//! Bind order mirrors `packages/d1-sync/src/record.rs` and the SQL constants
//! live in `api_core::record`, so a statement that works locally against the
//! real migrations is the one that runs here.
//!
//! Every method is a coarse inclusive-date-range read: the report's windows
//! (see `api_core::record::service`) decide the bounds, and the fold lives in
//! `api_core::record::assemble`.

use api_core::record::{
    RecordCategoryRow, RecordCategorySecondsRow, RecordDaySecondsRow, RecordHourSecondsRow,
    RecordMetaRow, RecordNightRow, RecordRepo, RecordTitleRow, RECORD_CATEGORIES_SQL,
    RECORD_CATEGORY_SECONDS_SQL, RECORD_EARLIEST_META_DATE_SQL, RECORD_EARLIEST_NIGHT_DATE_SQL,
    RECORD_HOUR_SECONDS_SQL, RECORD_META_SQL, RECORD_NIGHTS_SQL, RECORD_PRIMARY_TIME_ZONE_SQL,
    RECORD_PRODUCTIVE_SECONDS_SQL, RECORD_UNCLASSIFIED_TITLES_SQL,
};
use api_core::repo::RepoError;
use serde::Deserialize;
use worker::{D1Database, D1Type};

use super::{backend, query_vec};

/// The reads behind `GET /api/reports/record`.
pub struct D1RecordRepo {
    db: D1Database,
}

impl D1RecordRepo {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// A `SELECT MIN(...)` row: no matching rows still yields one row with NULL.
#[derive(Debug, Deserialize)]
struct EarliestRow {
    earliest_date: Option<String>,
}

/// A zone row.
#[derive(Debug, Deserialize)]
struct ZoneRow {
    time_zone: String,
}

#[async_trait::async_trait(?Send)]
impl RecordRepo for D1RecordRepo {
    async fn primary_time_zone(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_PRIMARY_TIME_ZONE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<ZoneRow> = stmt.first::<ZoneRow>(None).await.map_err(backend)?;
        Ok(row.map(|row| row.time_zone))
    }

    async fn load_categories(&self, user_id: &str) -> Result<Vec<RecordCategoryRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_CATEGORIES_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_meta(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordMetaRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_META_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_category_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordCategorySecondsRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_CATEGORY_SECONDS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_hour_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordHourSecondsRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_HOUR_SECONDS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_productive_seconds(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordDaySecondsRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_PRODUCTIVE_SECONDS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn load_nights(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
    ) -> Result<Vec<RecordNightRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_NIGHTS_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }

    async fn earliest_meta_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_EARLIEST_META_DATE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<EarliestRow> = stmt.first::<EarliestRow>(None).await.map_err(backend)?;
        Ok(row.and_then(|row| row.earliest_date))
    }

    async fn earliest_night_date(&self, user_id: &str) -> Result<Option<String>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_EARLIEST_NIGHT_DATE_SQL)
            .bind_refs(&[D1Type::Text(user_id)])
            .map_err(backend)?;
        let row: Option<EarliestRow> = stmt.first::<EarliestRow>(None).await.map_err(backend)?;
        Ok(row.and_then(|row| row.earliest_date))
    }

    async fn unclassified_titles(
        &self,
        user_id: &str,
        from_date: &str,
        to_date: &str,
        limit: usize,
    ) -> Result<Vec<RecordTitleRow>, RepoError> {
        let stmt = self
            .db
            .prepare(RECORD_UNCLASSIFIED_TITLES_SQL)
            .bind_refs(&[
                D1Type::Text(user_id),
                D1Type::Text(from_date),
                D1Type::Text(to_date),
                D1Type::Integer(limit.min(i32::MAX as usize) as i32),
            ])
            .map_err(backend)?;
        query_vec(stmt).await
    }
}

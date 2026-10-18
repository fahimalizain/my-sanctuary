//! The record service: the async entry point the Worker calls.
//!
//! It resolves "today" in the user's primary calendar zone, loads the digest
//! rows the report needs (each range derived from the report's own windows),
//! and folds them through the pure [`assemble_record`]. Nothing here scores
//! anything, and nothing here waits on the digest: the report reads whatever
//! the aggregates already contain and asks the tick to fill in the rest.

use crate::digest::{ensure_backfill_job, shift_date, DigestRepo};
use crate::time::{civil_date_in_zone, parse_iana_tz, unix_secs_to_rfc3339};

use super::assemble::assemble_record;
use super::repo::{earliest_of, min_date, RecordRepo};
use super::{first_year_start, RecordData, RecordError, RecordRange, RecordReport};

/// Extra days behind the range's first day that the trend's `ma60` window
/// needs (`ma60` covers the 60 days *ending* on a day).
pub const TREND_WINDOW_DAYS: i64 = 59;

/// The zone the report's "today" is read in: the primary calendar zone, else
/// the zone of the last day the digest computed, else UTC.
///
/// The same fallback the rules path makes (`rules::service::today`), so the
/// board and the record never disagree about which civil date it is. Before
/// the first calendar sync the primary zone is unknown while `daily_meta`
/// already knows it; jumping straight to UTC would move the user's day.
async fn report_time_zone(
    repo: &dyn RecordRepo,
    digest_repo: &dyn DigestRepo,
    user_id: &str,
) -> Result<String, RecordError> {
    let stored = repo.primary_time_zone(user_id).await?.unwrap_or_default();
    if parse_iana_tz(&stored).is_some() {
        return Ok(stored.trim().to_string());
    }
    let latest = digest_repo.latest_time_zone(user_id).await?.unwrap_or_default();
    if parse_iana_tz(&latest).is_some() {
        return Ok(latest.trim().to_string());
    }
    Ok("UTC".to_string())
}

/// `GET /api/reports/record` — the assembled report.
///
/// `now_unix` comes from the Worker's clock; api-core never reads
/// `SystemTime`. The backfill job is started when it is due, but never awaited
/// to completion: the report renders what exists.
pub async fn record_report(
    repo: &dyn RecordRepo,
    digest_repo: &dyn DigestRepo,
    user_id: &str,
    range: RecordRange,
    now_unix: i64,
) -> Result<RecordReport, RecordError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let time_zone = report_time_zone(repo, digest_repo, user_id).await?;
    let as_of = civil_date_in_zone(now_unix, &time_zone);

    // Opening the report also queues history. The rebuild itself belongs to the
    // cron/queue tick; a failure here only means the report shows what it has.
    let _ = ensure_backfill_job(digest_repo, user_id, &now_rfc3339).await;

    let Some(window) = range.window(&as_of) else {
        return Err(RecordError::Invalid(format!(
            "range {} is not a civil-date window",
            range.as_str()
        )));
    };

    // The trend windows must reach back far enough for the *first* visible day
    // (the previous period alone is too short for a 7-day range), and the
    // ranking needs the whole previous period.
    let trend_from = shift_date(&window.from, -window.length().max(TREND_WINDOW_DAYS))
        .unwrap_or_else(|| window.from.clone());

    let categories = repo.load_categories(user_id).await?;
    let earliest_date = earliest_of(
        repo.earliest_meta_date(user_id).await?,
        repo.earliest_night_date(user_id).await?,
    );
    // The years table starts at the earliest aggregate day, capped to the most
    // recent years — the same floor the assembler renders from.
    let years_from = first_year_start(earliest_date.as_deref(), &as_of);
    let history_from = min_date(&trend_from, &years_from);
    // The title list is filtered on each event's **local** civil date in the
    // assembler, so the read is padded by a day on each side of the UTC prefix.
    let titles_from = shift_date(&window.from, -1).unwrap_or_else(|| window.from.clone());
    let titles_to = shift_date(&window.to, 1).unwrap_or_else(|| window.to.clone());

    let data = RecordData {
        categories,
        meta: repo.load_meta(user_id, &window.from, &window.to).await?,
        category_seconds: repo
            .load_category_seconds(user_id, &window.previous_from, &window.to)
            .await?,
        hour_seconds: repo
            .load_hour_seconds(user_id, &window.from, &window.to)
            .await?,
        productive_seconds: repo
            .load_productive_seconds(user_id, &history_from, &window.to)
            .await?,
        // Every night since the first shown year: `quality` keeps the
        // estimated ones in the range, `years` the paired ones per year.
        nights: repo.load_nights(user_id, &years_from, &window.to).await?,
        earliest_date,
        titles: repo
            .unclassified_titles(user_id, &titles_from, &titles_to)
            .await?,
    };

    Ok(assemble_record(&data, &as_of, &time_zone, range, now_unix))
}

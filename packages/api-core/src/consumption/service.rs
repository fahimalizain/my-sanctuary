//! The consumption service: the async entry point the Worker calls.
//!
//! It resolves "today" in the user's primary calendar zone, derives the 14-day
//! window and its padded read bounds, loads the cached events and the living
//! categories, and folds them through the pure [`assemble_consumption`].
//!
//! Unlike The Record this never queues a backfill: the page is a lens on what
//! the diary already contains, and opening it must not enqueue digest work.

use crate::digest::{shift_date, DigestRepo};
use crate::record::{RecordError, RecordRepo};
use crate::time::civil_date_in_zone;

use super::assemble::{assemble_consumption, ConsumptionReport};

/// Civil days the report covers, inclusive.
const WINDOW_DAYS: i64 = 14;
/// Extra days on each side of the window's read bounds. See
/// [`DIGEST_LOAD_EVENTS_SQL`](crate::digest::repo::DIGEST_LOAD_EVENTS_SQL):
/// the SQL compares RFC 3339 **text prefixes**, so a zone-shifted instant can
/// sit one civil date away from its UTC stamp and two days is the documented
/// slack.
const LOAD_SLACK_DAYS: i64 = 2;

/// The 14-day report window plus the padded bounds the event read uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConsumptionWindow {
    /// First civil date of the window, inclusive.
    pub(crate) from: String,
    /// Last civil date of the window, inclusive (== `as_of`).
    pub(crate) to: String,
    /// Padded read bound, inclusive.
    pub(crate) load_from: String,
    /// Padded read bound, inclusive.
    pub(crate) load_to: String,
}

/// The window ending on `as_of`: 14 civil days inclusive, with the read bounds
/// padded for the SQL's text-prefix compare.
///
/// A date that cannot be shifted is a 400, never an invented date.
pub(crate) fn consumption_window(as_of: &str) -> Result<ConsumptionWindow, RecordError> {
    let invalid = || RecordError::Invalid(format!("{as_of} is not a civil date"));
    let from = shift_date(as_of, -(WINDOW_DAYS - 1)).ok_or_else(invalid)?;
    let load_from = shift_date(&from, -LOAD_SLACK_DAYS).ok_or_else(invalid)?;
    let load_to = shift_date(as_of, LOAD_SLACK_DAYS).ok_or_else(invalid)?;
    Ok(ConsumptionWindow {
        from,
        to: as_of.to_string(),
        load_from,
        load_to,
    })
}

/// The 14-day consumption report.
///
/// `now_unix` comes from the Worker's clock; api-core never reads
/// `SystemTime`. The window is resolved in the user's own zone, but each event
/// is dated in **its own** zone by the assembler.
pub async fn consumption_report(
    repo: &dyn RecordRepo,
    digest_repo: &dyn DigestRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<ConsumptionReport, RecordError> {
    let time_zone = crate::record::service::report_time_zone(repo, digest_repo, user_id).await?;
    let as_of = civil_date_in_zone(now_unix, &time_zone);
    let window = consumption_window(&as_of)?;

    let events = digest_repo
        .load_events_touching(user_id, &window.load_from, &window.load_to)
        .await?;
    let categories = repo.load_categories(user_id).await?;

    Ok(assemble_consumption(
        &events,
        &categories,
        &window.from,
        &window.to,
        &as_of,
        &time_zone,
    ))
}

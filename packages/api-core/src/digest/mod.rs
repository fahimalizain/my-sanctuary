//! Per-day time aggregates (reports slice 2).
//!
//! Pure pipeline ported from the `calendar-ipynb` notebooks: filter the cached
//! Google events, date and split them, share overlapping time exactly, infer
//! each day's time zone, derive sleep nights, and bucket the result into
//! category and hour rows. Nothing here touches D1, `worker`, or the clock —
//! callers pass `now_unix`, and persistence lives in [`repo`] / the worker.
//!
//! # Units
//!
//! Every stored duration is an integer number of **seconds**, never fractional
//! minutes. Overlap sharing is exact in seconds (largest remainder), so
//! `6.5h = 23400` exactly and a full civil day is `86400`. A DST day is
//! technically 23h/25h; the untracked fill still uses 86400 and accepts being
//! an hour off on those two days a year.
//!
//! # Filters
//!
//! Dropped entirely: soft-deleted rows, `status == "cancelled"`, recurring
//! **masters** (trimmed `recurrence` non-empty), all-day rows, rows that are
//! not self-organized, and `event_type` values outside
//! `{"", "default", "fromGmail"}` (`""` is legacy/unknown and **included**).
//!
//! RRULEs are never expanded: with `singleEvents=false` the normal occurrences
//! are absent from the cache until the user's weekly script materializes them.
//! That is intentional.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{DateTime, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::time::{civil_date_in_zone, parse_iana_tz, rfc3339_to_unix_secs, unix_secs_to_rfc3339};

pub mod queue;
pub mod repo;
pub mod sleep;
pub mod tick;

#[cfg(test)]
mod tests;

pub use queue::{DigestJobKind, DigestQueueMessage, WorkerQueueMessage};
pub use repo::{
    dirty_dates_for_span, dirty_dates_for_spans, DigestEventRow, DigestRepo, EventSpan,
    EventSpanRow, JobStatus, ReclassifyRow, DIGEST_MARK_DIRTY_CHUNK_SIZE,
};
pub use sleep::{
    bed_minutes_past_noon, marker_kind, resolve_sleep_category_id, wake_minutes_past_midnight,
    DayAnchors, MarkerKind, NightRow, SleepMarker, SleepSource, ALGO_VERSION,
    DEFAULT_SLEEP_SECS, SLEEP_PAIR_MAX_SECS, SYNTHETIC_SLEEP_CATEGORY_ID,
};
pub use tick::{
    digest_status, ensure_backfill_job, run_digest_tick, run_digest_tick_user, BackfillJob,
    DigestStatus, DigestTickReport, JOB_BACKFILL, JOB_DONE, JOB_RECLASSIFY, JOB_RUNNING,
    DIGEST_TICK_BUDGET_DAYS, RECLASSIFY_BATCH_LIMIT,
};

/// A cached calendar event as the digest reads it.
///
/// Mirrors the `calendar_events` projection; every filter runs in Rust so the
/// SQL stays a coarse, padded window scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestEvent {
    pub id: String,
    pub title: String,
    pub start_rfc3339: String,
    pub end_rfc3339: String,
    /// IANA zone; may be empty (→ UTC, or the end zone when that is known).
    pub start_time_zone: String,
    pub end_time_zone: String,
    /// Stamped leaf category. `None` = unclassified — never the untracked sink.
    pub category_id: Option<String>,
    pub is_all_day: bool,
    pub is_self_organized: bool,
    /// Google `eventType`; `''` = unknown/legacy.
    pub event_type: String,
    /// Non-empty ⇒ recurring master (excluded).
    pub recurrence: String,
    pub status: String,
    pub deleted: bool,
}

/// One category's post-share seconds on a date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySeconds {
    /// `''` = unclassified real tracked time (not the untracked fill).
    pub category_id: String,
    pub seconds: i64,
}

/// One hour-of-day bucket's post-share seconds on a date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HourSeconds {
    /// `0..23`, in the day's winning zone.
    pub hour: i64,
    pub category_id: String,
    pub seconds: i64,
}

/// One day's computed rollup, ready for [`repo::DigestRepo::replace_day`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayDigest {
    pub local_date: String,
    /// Duration-weighted (real-event) IANA zone for the day.
    pub time_zone: String,
    /// `sum(category_seconds.seconds)` and `sum(hour_seconds.seconds)`.
    pub tracked_seconds: i64,
    /// Post-share seconds of **synthetic** sleep intervals only — a real event
    /// that merely classified as `sleep` is not counted here.
    pub sleep_seconds: i64,
    pub computed_at: String,
    pub category_seconds: Vec<CategorySeconds>,
    pub hour_seconds: Vec<HourSeconds>,
    /// Nights whose `wake_date` is this date. A night is owned by the day it
    /// wakes on; its pre-midnight synthetic seconds still land on the previous
    /// date's rows (rebuilt through the ±1 dirty marking).
    pub nights: Vec<NightRow>,
}

/// Whether an event survives the digest filters.
pub fn include_event(event: &DigestEvent) -> bool {
    if event.deleted {
        return false;
    }
    // Only the exact Google value is cancelled — a legacy/blank status is not.
    if event.status == "cancelled" {
        return false;
    }
    if !event.recurrence.trim().is_empty() {
        return false;
    }
    if event.is_all_day {
        return false;
    }
    if !event.is_self_organized {
        return false;
    }
    matches!(event.event_type.as_str(), "" | "default" | "fromGmail")
}

/// Effective IANA zone name: the start zone; when that is empty/unknown, the
/// end zone (the travel-day fallback vote); else `UTC` (ADR 0004).
pub fn effective_zone_name(start_time_zone: &str, end_time_zone: &str) -> String {
    let start = start_time_zone.trim();
    if parse_iana_tz(start).is_some() {
        return start.to_string();
    }
    let end = end_time_zone.trim();
    if parse_iana_tz(end).is_some() {
        return end.to_string();
    }
    "UTC".to_string()
}

/// Normalizes a caller-supplied zone name, falling back to `UTC`.
pub fn normalized_zone_name(zone: &str) -> String {
    let zone = zone.trim();
    if parse_iana_tz(zone).is_some() {
        zone.to_string()
    } else {
        "UTC".to_string()
    }
}

fn instant(unix: i64) -> DateTime<chrono::Utc> {
    DateTime::from_timestamp(unix, 0)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is a valid instant"))
}

/// `(hour, minute)` of an instant in a zone (empty/unknown → UTC).
pub(crate) fn zone_hour_minute(unix: i64, zone: &str) -> (i64, i64) {
    let tz = parse_iana_tz(zone).unwrap_or(Tz::UTC);
    let local = instant(unix).with_timezone(&tz);
    (local.hour() as i64, local.minute() as i64)
}

/// The instant of a local wall-clock time (`date` `hour:minute`) in `zone`.
///
/// DST discipline (same as the overnight split): an unambiguous local time
/// wins, a fold resolves to the **earliest** instant, and a time that does not
/// exist at all (spring-forward gap) returns `None` so the caller degrades
/// instead of inventing an instant.
pub(crate) fn local_hh_mm_unix(date: &str, hour: u32, minute: u32, zone: &str) -> Option<i64> {
    let tz = parse_iana_tz(zone).unwrap_or(Tz::UTC);
    let day = chrono::NaiveDate::from_str(date).ok()?;
    let naive = day.and_hms_opt(hour, minute, 0)?;
    let resolved = tz.from_local_datetime(&naive);
    resolved
        .single()
        .or_else(|| resolved.earliest())
        .map(|dt| dt.timestamp())
}

/// Shifts a `YYYY-MM-DD` civil date by whole days.
pub fn shift_date(date: &str, days: i64) -> Option<String> {
    let day = chrono::NaiveDate::from_str(date).ok()?;
    let shifted = if days >= 0 {
        day.checked_add_days(chrono::Days::new(days as u64))?
    } else {
        day.checked_sub_days(chrono::Days::new((-days) as u64))?
    };
    Some(shifted.format("%Y-%m-%d").to_string())
}

/// The instant of local midnight starting `date` in `zone`, if it exists.
fn local_midnight_unix(date: &str, tz: Tz) -> Option<i64> {
    let day = chrono::NaiveDate::from_str(date).ok()?;
    let naive = day.and_hms_opt(0, 0, 0)?;
    let resolved = tz.from_local_datetime(&naive);
    resolved
        .single()
        .or_else(|| resolved.earliest())
        .map(|dt| dt.timestamp())
}

/// Splits `[start, end)` at every local midnight of `tz` between them.
///
/// Half-open: an event that ends **exactly** on the next local midnight is not
/// split (same guard as `events.py`'s `end_datetime > midnight`), so no
/// zero-length tail is produced.
///
/// **Deliberate improvement over `calendar-ipynb`:** the civil date and the
/// split are resolved in the event's **start IANA zone** (chrono-tz, DST
/// aware), not from the numeric offset embedded in the RFC 3339 string. A
/// `+05:30` stamp and an `Asia/Kolkata` zone therefore agree even across a DST
/// transition, and a zone-less event still dates itself in a real zone.
///
/// A local midnight that does not exist (a spring-forward gap) is skipped: the
/// event stays whole in its start zone's civil day, which is the documented
/// DST compromise.
fn split_at_local_midnights(start: i64, end: i64, tz: Tz) -> Vec<(i64, i64)> {
    if end <= start {
        return Vec::new();
    }
    let start_tz = instant(start).with_timezone(&tz);
    let mut boundaries: Vec<i64> = Vec::new();
    let mut cursor_date = start_tz.format("%Y-%m-%d").to_string();
    // Walk local midnights forward until they pass the event's end.
    while let Some(next_date) = shift_date(&cursor_date, 1) {
        let Some(midnight) = local_midnight_unix(&next_date, tz) else {
            // Nonexistent midnight: leave the event whole rather than guess.
            break;
        };
        if midnight >= end {
            break;
        }
        if midnight > start {
            boundaries.push(midnight);
        }
        cursor_date = next_date;
    }

    let mut parts = Vec::with_capacity(boundaries.len() + 1);
    let mut from = start;
    for boundary in boundaries {
        parts.push((from, boundary));
        from = boundary;
    }
    parts.push((from, end));
    parts
}

/// One dated, clipped interval in the sharing pool.
#[derive(Debug, Clone)]
struct Segment {
    key: SegKey,
    local_date: String,
    start: i64,
    end: i64,
    /// Effective start zone name — votes for the day's zone (real only).
    zone: String,
    /// `''` = unclassified real time; the sleep sentinel for synthetic sleep.
    category_id: String,
    /// Synthetic sleep intervals do not vote and are counted as `sleep_seconds`.
    synthetic: bool,
    /// Sleep markers never count as a "real event" for the estimation anchors.
    is_marker: bool,
}

/// Deterministic identity for the remainder tie-break: event id, then the
/// segment's stable creation order (two segments of one event never share a
/// slice, but the order keeps the sort total).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SegKey {
    id: String,
    ord: usize,
}

/// Seconds per segment after overlap sharing, in the pool's order.
///
/// The port of `events.py`'s `handle_overlapping_event_durations`, in integer
/// seconds on absolute instants:
///
/// * boundaries are every segment start and end;
/// * for each adjacent slice with `secs > 0`, the *active* segments are those
///   covering it (`start <= slice_start && end >= slice_end`, the Python
///   predicate);
/// * `base = secs / n` goes to every active segment, and the `secs % n`
///   leftover seconds go one each to the active segments with the smallest
///   event id (string order).
///
/// The parts always sum to the slice length, so nothing is created or lost.
fn share_seconds(segments: &[Segment]) -> Vec<i64> {
    let mut out = vec![0_i64; segments.len()];
    if segments.is_empty() {
        return out;
    }
    let mut boundaries: Vec<i64> = Vec::with_capacity(segments.len() * 2);
    for segment in segments {
        boundaries.push(segment.start);
        boundaries.push(segment.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut order: Vec<usize> = Vec::new();
    for window in boundaries.windows(2) {
        let (slice_start, slice_end) = (window[0], window[1]);
        let slice_secs = slice_end - slice_start;
        if slice_secs <= 0 {
            continue;
        }
        order.clear();
        order.extend(
            segments
                .iter()
                .enumerate()
                .filter(|(_, segment)| segment.start <= slice_start && segment.end >= slice_end)
                .map(|(index, _)| index),
        );
        if order.is_empty() {
            continue;
        }
        let active = order.len() as i64;
        let base = slice_secs / active;
        let remainder = slice_secs % active;
        order.sort_by(|&left, &right| segments[left].key.cmp(&segments[right].key));
        for (rank, &index) in order.iter().enumerate() {
            out[index] += base + i64::from((rank as i64) < remainder);
        }
    }
    out
}

/// Splits `[start, end)` into `(local hour, seconds)` buckets in `tz`.
///
/// A local hour boundary is resolved through chrono-tz (unambiguous, else the
/// earliest instant). When the boundary does not exist (spring-forward gap) the
/// clock jumps a whole hour, so advancing by `3600 - seconds_into_hour`
/// seconds lands exactly on the next existing wall clock hour. A DST day can
/// still be an hour off; that is accepted.
fn hour_slices(start: i64, end: i64, tz: Tz) -> Vec<(i64, i64)> {
    let mut slices = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let local = instant(cursor).with_timezone(&tz);
        let hour = local.hour() as i64;
        let seconds_into_hour = local.minute() as i64 * 60 + local.second() as i64;
        let naive = local.naive_local();
        let next_naive = naive
            .date()
            .and_hms_opt(local.hour(), 0, 0)
            .and_then(|hour_start| hour_start.checked_add_signed(chrono::Duration::hours(1)));
        let boundary = next_naive
            .map(|next| {
                let resolved = tz.from_local_datetime(&next);
                resolved.single().or_else(|| resolved.earliest())
            })
            .unwrap_or(None)
            .map(|dt| dt.timestamp())
            .unwrap_or(cursor + (3600 - seconds_into_hour));
        let mut next = boundary.min(end);
        if next <= cursor {
            next = end;
        }
        slices.push((hour, next - cursor));
        cursor = next;
    }
    slices
}

/// Distributes a segment's `total_secs` over its hour slices so the parts sum
/// **exactly** to `total_secs` (largest remainder, ties by earlier slice).
fn allocate_hours(start: i64, end: i64, total_secs: i64, tz: Tz) -> Vec<(i64, i64)> {
    let slices = hour_slices(start, end, tz);
    if slices.is_empty() || total_secs <= 0 {
        return Vec::new();
    }
    if slices.len() == 1 {
        return vec![(slices[0].0, total_secs)];
    }
    let span: i64 = slices.iter().map(|(_, secs)| secs).sum();
    if span <= 0 {
        return vec![(slices[0].0, total_secs)];
    }
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(slices.len());
    let mut assigned = 0_i64;
    for (_, secs) in &slices {
        let part = secs * total_secs / span;
        assigned += part;
        out.push((0, part));
    }
    for (index, (hour, _)) in slices.iter().enumerate() {
        out[index].0 = *hour;
    }
    let mut leftover = total_secs - assigned;
    if leftover > 0 {
        let mut fractions: Vec<(i64, usize)> = slices
            .iter()
            .enumerate()
            .map(|(index, (_, secs))| (secs * total_secs % span, index))
            .collect();
        fractions.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        for (_, index) in fractions {
            if leftover <= 0 {
                break;
            }
            out[index].1 += 1;
            leftover -= 1;
        }
    }
    out
}

/// Per-date accumulator built from the shared pool.
#[derive(Default)]
struct DateAggregate {
    tracked_seconds: i64,
    sleep_seconds: i64,
    categories: BTreeMap<String, i64>,
    hours: BTreeMap<(i64, String), i64>,
}

/// Builds the rollup for `dates` (empty ⇒ every date that got a segment or a
/// night).
///
/// `now_unix` clips the pool: an interval starting at or after `now` is
/// dropped, one straddling `now` is cut at `now`, and one that empties is
/// dropped — today is provisional. Clipping happens **before** overlap
/// sharing, so the shared total counts only elapsed time.
///
/// `fallback_tz` is the user's primary calendar zone, used when a date has no
/// voting segment and no earlier voted date to inherit from (empty/unknown ⇒
/// UTC). `sleep_category_id` comes from the user's `sleep` category; `None`
/// falls back to [`SYNTHETIC_SLEEP_CATEGORY_ID`].
pub fn digest_days(
    events: &[DigestEvent],
    dates: &[String],
    now_unix: i64,
    fallback_tz: &str,
    sleep_category_id: Option<&str>,
) -> Vec<DayDigest> {
    let fallback = normalized_zone_name(fallback_tz);
    let sleep_category = sleep_category_id
        .filter(|id| !id.is_empty())
        .unwrap_or(SYNTHETIC_SLEEP_CATEGORY_ID)
        .to_string();

    // ── Filter, date, split, clip ───────────────────────────────────────────
    let mut segments: Vec<Segment> = Vec::new();
    let mut markers: Vec<SleepMarker> = Vec::new();
    let mut marker_ids: BTreeSet<String> = BTreeSet::new();
    let mut ord = 0_usize;
    for event in events {
        if !include_event(event) {
            continue;
        }
        let (Some(start), Some(end)) = (
            rfc3339_to_unix_secs(&event.start_rfc3339),
            rfc3339_to_unix_secs(&event.end_rfc3339),
        ) else {
            // An unparseable instant drops the event entirely.
            continue;
        };
        let zone = effective_zone_name(&event.start_time_zone, &event.end_time_zone);
        let kind = sleep::marker_kind(&event.title);
        if matches!(kind, MarkerKind::Asleep | MarkerKind::Woke) {
            markers.push(SleepMarker {
                event_id: event.id.clone(),
                kind,
                start_unix: start,
                zone: zone.clone(),
            });
            marker_ids.insert(event.id.clone());
        }
        let tz = parse_iana_tz(&zone).unwrap_or(Tz::UTC);
        for (part_start, part_end) in split_at_local_midnights(start, end, tz) {
            if part_start >= now_unix {
                continue;
            }
            let part_end = part_end.min(now_unix);
            if part_end <= part_start {
                continue;
            }
            segments.push(Segment {
                key: SegKey {
                    id: event.id.clone(),
                    ord,
                },
                local_date: civil_date_in_zone(part_start, &zone),
                start: part_start,
                end: part_end,
                zone: zone.clone(),
                category_id: event.category_id.clone().unwrap_or_default(),
                synthetic: false,
                is_marker: marker_ids.contains(&event.id),
            });
            ord += 1;
        }
    }

    // ── Day zone vote (real segments only) ──────────────────────────────────
    let mut votes: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut anchors: BTreeMap<String, DayAnchors> = BTreeMap::new();
    for segment in &segments {
        if !segment.is_marker {
            let entry = anchors.entry(segment.local_date.clone()).or_default();
            entry.first_start = Some(
                entry
                    .first_start
                    .map_or(segment.start, |current| current.min(segment.start)),
            );
            entry.last_end = Some(
                entry
                    .last_end
                    .map_or(segment.end, |current| current.max(segment.end)),
            );
        }
        *votes
            .entry(segment.local_date.clone())
            .or_default()
            .entry(segment.zone.clone())
            .or_insert(0) += segment.end - segment.start;
    }
    let mut winners: BTreeMap<String, String> = BTreeMap::new();
    for (date, weights) in &votes {
        // Highest weight wins; ties go to the lexicographically smaller zone.
        let winner = weights
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
            .map(|(zone, _)| zone.clone());
        if let Some(winner) = winner {
            winners.insert(date.clone(), winner);
        }
    }
    // Vote, then the previous date's resolved zone, then `override_zone` (the
    // wake marker's own zone for sleep minutes), then the caller's fallback.
    //
    // "Previous date" is the nearest **voted** date before this one: carrying
    // forward through a hole and looking back past a hole are the same thing
    // once the walk is ordered.
    let previous_winner = |date: &str| -> Option<String> {
        winners
            .iter()
            .rev()
            .find(|(key, _)| key.as_str() < date)
            .map(|(_, zone)| zone.clone())
    };
    let resolve_zone = |date: &str, override_zone: Option<&str>| -> String {
        if let Some(winner) = winners.get(date) {
            return winner.clone();
        }
        if let Some(winner) = previous_winner(date) {
            return winner;
        }
        if let Some(zone) = override_zone {
            let zone = zone.trim();
            if parse_iana_tz(zone).is_some() {
                return zone.to_string();
            }
        }
        fallback.clone()
    };

    // ── Sleep nights ────────────────────────────────────────────────────────
    let paired = sleep::pair_markers(&markers);
    let mut paired_marker_indices: BTreeSet<usize> = BTreeSet::new();
    let mut paired_nights: Vec<NightRow> = Vec::new();
    // Synthetic `[asleep, woke)` intervals with the zone they are split in
    // (the Asleep marker's own zone — the same "start zone" rule the real
    // events use).
    let mut paired_intervals: Vec<(i64, i64, String)> = Vec::new();
    for night in &paired {
        paired_marker_indices.insert(night.asleep_idx);
        paired_marker_indices.insert(night.woke_idx);
        // Bedtime minutes are measured in the wake date's day zone; when that
        // date has no vote and no earlier vote, the wake marker's own zone.
        let bed_zone =
            resolve_zone(&night.wake_date, Some(&markers[night.woke_idx].zone));
        paired_nights.push(NightRow {
            wake_date: night.wake_date.clone(),
            asleep_at: unix_secs_to_rfc3339(night.asleep_unix),
            woke_at: unix_secs_to_rfc3339(night.woke_unix),
            seconds: night.seconds,
            bed_min_past_noon: sleep::bed_minutes_past_noon(night.asleep_unix, &bed_zone),
            wake_min_past_midnight: sleep::wake_minutes_past_midnight(
                night.woke_unix,
                &bed_zone,
            ),
            source: SleepSource::Paired,
        });
        paired_intervals.push((
            night.asleep_unix,
            night.woke_unix,
            markers[night.asleep_idx].zone.clone(),
        ));
    }
    paired_nights.sort_by(|left, right| {
        (left.wake_date.clone(), left.asleep_at.clone())
            .cmp(&(right.wake_date.clone(), right.asleep_at.clone()))
    });

    let unpaired: Vec<&SleepMarker> = markers
        .iter()
        .enumerate()
        .filter(|(index, _)| !paired_marker_indices.contains(index))
        .map(|(_, marker)| marker)
        .collect();

    // Estimation only walks the dates the caller is rebuilding, and never
    // guesses an in-progress night for today.
    let mut estimated_nights: Vec<NightRow> = Vec::new();
    let mut estimated_intervals: Vec<(i64, i64, String)> = Vec::new();
    let mut paired_wake_dates: BTreeSet<&str> = BTreeSet::new();
    for night in &paired_nights {
        paired_wake_dates.insert(night.wake_date.as_str());
    }
    for day1 in dates {
        if paired_wake_dates.contains(day1.as_str()) {
            continue;
        }
        let Some(day0) = shift_date(day1, -1) else {
            continue;
        };
        let bed_index = sleep::pick_bed_marker(&day0, day1, &unpaired);
        let wake_index = sleep::pick_wake_marker(day1, &unpaired);
        let bed = bed_index.map(|index| unpaired[index]);
        let wake = wake_index.map(|index| unpaired[index]);

        let day1_zone = resolve_zone(day1, wake.map(|marker| marker.zone.as_str()));
        if wake.is_none() && civil_date_in_zone(now_unix, &day1_zone) == *day1 {
            let (hour, _) = zone_hour_minute(now_unix, &day1_zone);
            if hour < 12 {
                // The night may still be in progress — do not estimate yet.
                continue;
            }
        }

        let day0_zone = resolve_zone(&day0, None);
        let empty_anchors = DayAnchors::default();
        let day0_ctx = sleep::EstimateDay {
            date: &day0,
            zone: &day0_zone,
            anchors: anchors.get(&day0).unwrap_or(&empty_anchors),
        };
        let day1_ctx = sleep::EstimateDay {
            date: day1,
            zone: &day1_zone,
            anchors: anchors.get(day1).unwrap_or(&empty_anchors),
        };
        let Some(night) = sleep::estimate_night(bed, wake, &day0_ctx, &day1_ctx) else {
            continue;
        };
        let day_zone = resolve_zone(day1, Some(&night.asleep_zone));
        estimated_nights.push(NightRow {
            wake_date: day1.clone(),
            asleep_at: unix_secs_to_rfc3339(night.asleep_unix),
            woke_at: unix_secs_to_rfc3339(night.woke_unix),
            seconds: night.woke_unix - night.asleep_unix,
            bed_min_past_noon: sleep::bed_minutes_past_noon(night.asleep_unix, &day_zone),
            wake_min_past_midnight: sleep::wake_minutes_past_midnight(
                night.woke_unix,
                &day_zone,
            ),
            source: night.source,
        });
        estimated_intervals.push((
            night.asleep_unix,
            night.woke_unix,
            night.asleep_zone.clone(),
        ));
    }

    // ── Synthetic sleep intervals ───────────────────────────────────────────
    //
    // Every emitted night contributes a half-open `[asleep, woke)` interval to
    // the same pool the real events live in, so a 1am event during sleep
    // **shares** its slice (`insert_sleep_events` +
    // `handle_overlapping_event_durations`). The interval is split at local
    // midnights of the bedtime's own zone — the same "start zone" rule the
    // real events use.
    let mut synthetic_intervals: Vec<(i64, i64, String)> = paired_intervals;
    synthetic_intervals.extend(estimated_intervals.iter().cloned());

    let mut synthetic_segments: Vec<Segment> = Vec::new();
    for (asleep_unix, woke_unix, zone) in &synthetic_intervals {
        let tz = parse_iana_tz(zone).unwrap_or(Tz::UTC);
        for (part_start, part_end) in split_at_local_midnights(*asleep_unix, *woke_unix, tz) {
            if part_start >= now_unix {
                continue;
            }
            let part_end = part_end.min(now_unix);
            if part_end <= part_start {
                continue;
            }
            synthetic_segments.push(Segment {
                key: SegKey {
                    id: format!(
                        "{SYNTHETIC_SLEEP_CATEGORY_ID}:{}",
                        unix_secs_to_rfc3339(*asleep_unix)
                    ),
                    ord,
                },
                local_date: civil_date_in_zone(part_start, zone),
                start: part_start,
                end: part_end,
                zone: zone.clone(),
                category_id: sleep_category.clone(),
                synthetic: true,
                is_marker: false,
            });
            ord += 1;
        }
    }

    // ── Emit set ────────────────────────────────────────────────────────────
    //
    // `dates` is the explicit emit set: a caller rebuilding `[D]` must not
    // receive (and therefore must not *replace*) its neighbour, whose nights
    // this computation never estimated. With no explicit dates, emit every
    // date that got a segment or a night.
    let emit: Vec<String> = if dates.is_empty() {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for segment in segments.iter().chain(synthetic_segments.iter()) {
            set.insert(segment.local_date.clone());
        }
        for night in paired_nights.iter().chain(estimated_nights.iter()) {
            set.insert(night.wake_date.clone());
        }
        set.into_iter().collect()
    } else {
        let mut set: BTreeSet<String> = BTreeSet::new();
        set.extend(dates.iter().cloned());
        set.into_iter().collect()
    };

    // ── Share, then bucket ──────────────────────────────────────────────────
    let mut pool = segments;
    pool.extend(synthetic_segments);
    let shares = share_seconds(&pool);
    let mut per_date: BTreeMap<String, DateAggregate> = BTreeMap::new();
    for (segment, seconds) in pool.iter().zip(shares) {
        if seconds <= 0 {
            continue;
        }
        let day_zone = resolve_zone(&segment.local_date, None);
        let tz = parse_iana_tz(&day_zone).unwrap_or(Tz::UTC);
        let aggregate = per_date.entry(segment.local_date.clone()).or_default();
        aggregate.tracked_seconds += seconds;
        if segment.synthetic {
            aggregate.sleep_seconds += seconds;
        }
        *aggregate
            .categories
            .entry(segment.category_id.clone())
            .or_insert(0) += seconds;
        for (hour, hour_seconds) in allocate_hours(segment.start, segment.end, seconds, tz) {
            if hour_seconds <= 0 {
                continue;
            }
            // The hour is the day zone's clock hour; seconds that the day-zone
            // conversion would place on a neighbouring civil date stay on the
            // segment's original `local_date` (travel-day edge, accepted).
            *aggregate
                .hours
                .entry((hour, segment.category_id.clone()))
                .or_insert(0) += hour_seconds;
        }
    }

    let all_nights: Vec<NightRow> = paired_nights
        .iter()
        .chain(estimated_nights.iter())
        .cloned()
        .collect();
    let computed_at = unix_secs_to_rfc3339(now_unix);

    emit.iter()
        .map(|date| {
            let aggregate = per_date.get(date);
            DayDigest {
                local_date: date.clone(),                time_zone: resolve_zone(date, None),
                tracked_seconds: aggregate.map_or(0, |a| a.tracked_seconds),
                sleep_seconds: aggregate.map_or(0, |a| a.sleep_seconds),
                computed_at: computed_at.clone(),
                category_seconds: aggregate
                    .map(|a| {
                        a.categories
                            .iter()
                            .map(|(category_id, seconds)| CategorySeconds {
                                category_id: category_id.clone(),
                                seconds: *seconds,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                hour_seconds: aggregate
                    .map(|a| {
                        a.hours
                            .iter()
                            .map(|((hour, category_id), seconds)| HourSeconds {
                                hour: *hour,
                                category_id: category_id.clone(),
                                seconds: *seconds,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                nights: all_nights
                    .iter()
                    .filter(|night| night.wake_date == *date)
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

/// Untracked fill for a day: `86400 - tracked`, floored at 0.
///
/// `None` while `local_date` is **today** in `day_tz` — the day is still being
/// written, so an untracked figure would only show the hours that have not
/// happened yet. Past days and future days both get a number; the caller
/// renders it.
///
/// DST: a 23h/25h civil day is still measured against 86400, so untracked can
/// be an hour off on those days. Accepted.
pub fn untracked_seconds(
    tracked_seconds: i64,
    local_date: &str,
    day_tz: &str,
    now_unix: i64,
) -> Option<i64> {
    let zone = normalized_zone_name(day_tz);
    if civil_date_in_zone(now_unix, &zone) == local_date {
        return None;
    }
    Some((sleep::DAY_SECS - tracked_seconds).max(0))
}

//! The pure 14-day consumption assembler: cached events in,
//! [`ConsumptionReport`] out.
//!
//! No I/O, no clock, no D1 — the service loads the rows and this module folds
//! them. Every ordering that reaches the client is deterministic, so the same
//! events always render the same report.
//!
//! # What counts
//!
//! An event must first survive [`include_event`] — the digest's filters, shared
//! rather than reimplemented. Deleted rows, `status == "cancelled"`, recurring
//! masters, all-day rows, non-self-organized rows, and `event_type` values
//! outside `{"", "default", "fromGmail"}` never count, so a `focusTime` block is
//! excluded even when its slug would otherwise classify.
//!
//! Each survivor is dated by **its own zone** (the start zone, then the end
//! zone, then UTC — see [`effective_zone_name`]), not the report's zone, and
//! kept when that civil date falls inside `[from, to]`. Its **full** duration is
//! credited once: a block crossing midnight is not split, and overlapping blocks
//! are not shared. These are gross logged seconds, so they will not match The
//! Record's ranking.
//!
//! # A fixed slug grammar
//!
//! The four slugs in [`classify`] are a fixed personal grammar. Renaming one
//! silently empties its group; matching calendar summaries or title patterns
//! instead would be guessing, so that is not done here. Any other slug is
//! skipped, as is an unclassified row, an unknown category id, and an empty
//! platform after trimming.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::digest::{effective_zone_name, include_event, DigestEvent};
use crate::record::RecordCategoryRow;
use crate::time::{civil_date_in_zone, rfc3339_to_unix_secs};

/// The series group: films and series watched.
const GROUP_SERIES: &str = "series";
/// The gaming group.
const GROUP_GAMING: &str = "gaming";
/// The passive-scroll group: short-form and social feeds.
const GROUP_SOCIAL: &str = "social";
/// The messaging group (WhatsApp calls on the `social` slug).
const GROUP_MESSAGING: &str = "messaging";

/// Every `Anything | YouTube` row collapses into this one bucket, so a hundred
/// video titles are not a hundred platforms.
const YOUTUBE_NAMED: &str = "YouTube (named videos)";
/// The same collapse for Crunchyroll.
const CRUNCHYROLL_NAMED: &str = "Crunchyroll (named)";

// ──────────────────────────────────────────
// Outputs
// ──────────────────────────────────────────

/// The 14-day consumption body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsumptionReport {
    /// The civil date the window ends on, in [`Self::time_zone`].
    pub as_of: String,
    /// The zone `as_of` was read in — the report's own zone, not a dating rule.
    pub time_zone: String,
    /// First civil date of the window, inclusive.
    pub from: String,
    /// Last civil date of the window, inclusive (== `as_of`).
    pub to: String,
    /// `sum(row.seconds)`.
    pub total_seconds: i64,
    /// Per-group totals, seconds descending, then group ascending.
    pub groups: Vec<ConsumptionGroupTotal>,
    /// Per-platform totals, seconds descending, then platform ascending.
    pub rows: Vec<ConsumptionRow>,
}

/// One group's gross seconds across the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsumptionGroupTotal {
    pub group: String,
    pub seconds: i64,
}

/// One platform's gross seconds across the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConsumptionRow {
    pub platform: String,
    /// `"series" | "social" | "messaging" | "gaming"`.
    pub group: String,
    pub seconds: i64,
    /// Event count — adjacent blocks are never merged.
    pub sessions: i64,
    /// The single longest block's seconds.
    pub longest_seconds: i64,
}

// ──────────────────────────────────────────
// The fold
// ──────────────────────────────────────────

/// Running totals for one `(group, platform)` bucket.
#[derive(Debug, Default)]
struct Bucket {
    seconds: i64,
    sessions: i64,
    longest_seconds: i64,
}

/// Folds the loaded events into the 14-day report.
///
/// `from`/`to` are inclusive `YYYY-MM-DD` civil dates; an event is dated in its
/// own zone and counted only when that date falls inside them. `as_of` and
/// `time_zone` are echoed back untouched.
pub fn assemble_consumption(
    events: &[DigestEvent],
    categories: &[RecordCategoryRow],
    from: &str,
    to: &str,
    as_of: &str,
    time_zone: &str,
) -> ConsumptionReport {
    // Category id → slug. Slugs are unique per user; if a fixture (or a hand
    // edited database) repeats one, the last row wins.
    let mut slugs: HashMap<&str, &str> = HashMap::with_capacity(categories.len());
    for category in categories {
        slugs.insert(category.id.as_str(), category.slug.as_str());
    }

    let mut buckets: BTreeMap<(String, String), Bucket> = BTreeMap::new();

    for event in events {
        if !include_event(event) {
            continue;
        }
        let (Some(start_unix), Some(end_unix)) = (
            rfc3339_to_unix_secs(&event.start_rfc3339),
            rfc3339_to_unix_secs(&event.end_rfc3339),
        ) else {
            continue;
        };
        let seconds = end_unix - start_unix;
        if seconds <= 0 {
            continue;
        }

        // Dated by the event's own zone: a travel-day row belongs to the day it
        // was lived in, not to the report's current zone.
        let zone = effective_zone_name(&event.start_time_zone, &event.end_time_zone);
        let date = civil_date_in_zone(start_unix, &zone);
        if date.as_str() < from || date.as_str() > to {
            continue;
        }

        let Some(category_id) = event.category_id.as_deref() else {
            continue;
        };
        let Some(slug) = slugs.get(category_id) else {
            continue;
        };
        let Some((group, platform)) = classify(slug, &event.title) else {
            continue;
        };
        let platform = platform.trim();
        if platform.is_empty() {
            continue;
        }

        let bucket = buckets
            .entry((group.to_string(), platform.to_string()))
            .or_default();
        bucket.seconds += seconds;
        bucket.sessions += 1;
        bucket.longest_seconds = bucket.longest_seconds.max(seconds);
    }

    let mut rows: Vec<ConsumptionRow> = buckets
        .into_iter()
        .map(|((group, platform), bucket)| ConsumptionRow {
            platform,
            group,
            seconds: bucket.seconds,
            sessions: bucket.sessions,
            longest_seconds: bucket.longest_seconds,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.seconds
            .cmp(&a.seconds)
            .then_with(|| a.platform.cmp(&b.platform))
    });

    let mut group_seconds: BTreeMap<&str, i64> = BTreeMap::new();
    for row in &rows {
        *group_seconds.entry(row.group.as_str()).or_default() += row.seconds;
    }
    let mut groups: Vec<ConsumptionGroupTotal> = group_seconds
        .into_iter()
        .filter(|(_, seconds)| *seconds > 0)
        .map(|(group, seconds)| ConsumptionGroupTotal {
            group: group.to_string(),
            seconds,
        })
        .collect();
    groups.sort_by(|a, b| {
        b.seconds
            .cmp(&a.seconds)
            .then_with(|| a.group.cmp(&b.group))
    });

    let total_seconds = rows.iter().map(|row| row.seconds).sum();

    ConsumptionReport {
        as_of: as_of.to_string(),
        time_zone: time_zone.to_string(),
        from: from.to_string(),
        to: to.to_string(),
        total_seconds,
        groups,
        rows,
    }
}

// ──────────────────────────────────────────
// Classification
// ──────────────────────────────────────────

/// Maps a leaf slug (plus its title, when the platform lives in the title) to a
/// `(group, platform)`, or `None` when the row does not count.
///
/// **These four slugs are a fixed personal grammar.** Renaming one silently
/// empties that group and that is accepted; there are no category UUIDs and no
/// calendar-summary matches here on purpose.
fn classify(slug: &str, title: &str) -> Option<(&'static str, String)> {
    match slug {
        "movies" => Some((GROUP_SERIES, series_platform(title))),
        "gaming" => Some((GROUP_GAMING, gaming_platform(title))),
        "social-media-youtube" => Some((GROUP_SOCIAL, social_platform(title))),
        // Only the `social` slug means "a messaging app". A WhatsApp title on
        // any other slug (`applix`, `anfield`, `whatsapppersonal`, …) is not
        // this group.
        "social" if title.to_lowercase().contains("whatsapp") => {
            Some((GROUP_MESSAGING, "WhatsApp".to_string()))
        }
        _ => None,
    }
}

/// Series/film stem: the text before the last ` | `, else the whole title.
fn series_platform(title: &str) -> String {
    split_before_last_pipe(title).trim().to_string()
}

/// Gaming stem, most specific delimiter first: `Game / Platform | Group` names
/// the game, `Game | Group` falls back to the same pipe rule as series.
fn gaming_platform(title: &str) -> String {
    match title.split_once(" / ") {
        Some((stem, _)) => stem.trim().to_string(),
        None => split_before_last_pipe(title).trim().to_string(),
    }
}

/// Social stem: a canonical token, a named-video bucket, or the title's own
/// stem.
fn social_platform(title: &str) -> String {
    let trimmed = title.trim();
    if let Some(platform) = canonical_platform(trimmed) {
        return platform.to_string();
    }

    match trimmed.rsplit_once(" | ") {
        None => trimmed.to_string(),
        Some((stem, suffix)) => {
            let stem = stem.trim();
            let suffix = suffix.trim();
            // A real video title is one bucket. The stem must be a title, not
            // the platform itself: `YouTube | YouTube` falls through to the
            // canonical suffix rule and stays plain `YouTube`.
            if !stem.is_empty()
                && suffix.eq_ignore_ascii_case("youtube")
                && !stem.eq_ignore_ascii_case("youtube")
            {
                return YOUTUBE_NAMED.to_string();
            }
            if !stem.is_empty()
                && suffix.eq_ignore_ascii_case("crunchyroll")
                && !stem.eq_ignore_ascii_case("crunchyroll")
            {
                return CRUNCHYROLL_NAMED.to_string();
            }
            if let Some(platform) = canonical_platform(suffix) {
                return platform.to_string();
            }
            stem.to_string()
        }
    }
}

/// The text before the last ` | `, or the whole title when there is none.
fn split_before_last_pipe(title: &str) -> &str {
    match title.rsplit_once(" | ") {
        Some((stem, _)) => stem,
        None => title,
    }
}

/// The canonical platform for a bare token, case-insensitive.
///
/// `twitter` and `x.com` are the same platform under one name, which is why
/// two loggable titles (or one of each) add into a single row.
fn canonical_platform(token: &str) -> Option<&'static str> {
    match token.trim().to_lowercase().as_str() {
        "instagram" => Some("Instagram"),
        "twitter" | "x.com" => Some("X / Twitter"),
        "youtube" => Some("YouTube"),
        "reddit" => Some("Reddit"),
        "linkedin" => Some("LinkedIn"),
        "threads" => Some("Threads"),
        "crunchyroll" => Some("Crunchyroll"),
        _ => None,
    }
}

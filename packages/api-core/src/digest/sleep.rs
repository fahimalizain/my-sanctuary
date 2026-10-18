//! Sleep nights: marker detection, pairing, and estimation.
//!
//! Ported from `calendar_ipynb/enforcement/scorer.py` (`_paired_nights`) and
//! `calendar_ipynb/sleep_events.py` (`insert_sleep_events`), with three
//! deliberate corrections:
//!
//! 1. **Instants.** A night runs from the **start** of the `Asleep` event to
//!    the **start** of the `Woke Up` event. `sleep_events.py` used the Asleep
//!    event's **end**, which yields a 0-hour night whenever the Asleep block
//!    runs until the Woke Up event starts (touching markers).
//!    `scorer.py` already used both starts; we keep that.
//! 2. **No 2-hour floor.** `scorer.py`'s `SLEEP_PAIR_MIN_HOURS = 2.0` is gone:
//!    a paired night only needs a positive duration. The 16-hour cap stays.
//! 3. **Short interruptions merge, nothing else.** `scorer.py`'s greedy walk
//!    splits a short evening pair off as its own night and then hands the
//!    morning wake to the real bedtime; the latest-wake-first walk that briefly
//!    replaced it over-merged instead (a nap up to 16h before a wake was
//!    swallowed by it). Pairing is greedy again — oldest Asleep first, first
//!    unused wake inside `(0, 16h]` — and a later pair joins the current night
//!    only when the awake gap is at most [`SLEEP_INTERRUPTION_MAX_SECS`] and
//!    the merged span still fits in [`SLEEP_PAIR_MAX_SECS`]. The stored night
//!    is that outer span, awake gap included. See [`pair_markers`].
//!
//! All durations are integer **seconds** (never fractional minutes). The
//! user-facing word is minutes; divide by 60 only at read time (slice 5).

use regex::Regex;
use std::sync::OnceLock;

use crate::time::civil_date_in_zone;

use super::{local_hh_mm_unix, zone_hour_minute};

/// Title regex for the bedtime marker. Case-sensitive, anchored — the legacy
/// `sleep_events.py` default (`^.*Asleep.*$`). `asleep` in other cases is not
/// a marker; `Going Asleep now` is.
pub const ASLEEP_TITLE_RE: &str = r"^.*Asleep.*$";
/// Title regex for the wake marker (legacy default `^.*Woke Up.*$`).
pub const WOKE_TITLE_RE: &str = r"^.*Woke Up.*$";
/// Longest plausible night: `(0, 16h]` pairs. `16h` exactly pairs; `16h + 1s`
/// does not.
pub const SLEEP_PAIR_MAX_SECS: i64 = 16 * 3600;
/// Longest awake interruption that still belongs to one night: a later pair
/// merges into the current night when the gap between its Asleep start and the
/// current Woke Up start is at most this. `60m` exactly merges; `60m + 1s`
/// does not. A negative gap (overlapping markers) is short by construction.
pub const SLEEP_INTERRUPTION_MAX_SECS: i64 = 3600;
/// 6.5h — the fallback estimated-night length. A deliberate user override of
/// `sleep_events.py`'s 8-hour default.
///
/// Only the fallback: a rebuild passes the user's `sleep_duration` rule target
/// ([`digest_days`](super::digest_days)' `default_sleep_secs`) and uses this
/// constant when the user has no such rule.
pub const DEFAULT_SLEEP_SECS: i64 = 23_400;
/// Category id used for synthetic sleep when the user has no `sleep` category.
/// Starts with `*` so it can never collide with a UUID category id, and is
/// never the `untracked` sink's id.
pub const SYNTHETIC_SLEEP_CATEGORY_ID: &str = "*sleep";
/// Cache version of the whole pipeline. A `daily_meta` row with a different
/// `algo_version` is stale and must be rebuilt.
///
/// `2`: nights are paired greedily and only **short** interruptions merge (see
/// [`pair_markers`]). Version `1` stored the latest-wake-first walk, whose
/// nights are wrong — a nap starting exactly 16h before a wake was swallowed by
/// it — so every stored night has to be rebuilt.
pub const ALGO_VERSION: i64 = 2;

/// Midnight is `12 * 60` minutes after noon; estimation treats the sleep day
/// as starting there (`bed_min_past_noon` shifts pre-noon instants forward a
/// day).
const NOON_MINUTES: i64 = 12 * 60;
/// A civil day's seconds. DST days are technically 23h/25h; the untracked fill
/// uses a flat 86400 and accepts being an hour off on those two days a year.
pub const DAY_SECS: i64 = 86_400;

fn asleep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(ASLEEP_TITLE_RE).expect("ASLEEP_TITLE_RE is a valid regex"))
}

fn woke_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(WOKE_TITLE_RE).expect("WOKE_TITLE_RE is a valid regex"))
}

/// What an event title is, sleep-wise.
///
/// Titles are trimmed before matching (the legacy code stripped the summary)
/// and matched case-sensitively. A title matching **both** markers is
/// [`MarkerKind::Ambiguous`] and is ignored as a marker — but it stays a real
/// event for the tracked-time pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// Matches [`ASLEEP_TITLE_RE`] only — bedtime marker.
    Asleep,
    /// Matches [`WOKE_TITLE_RE`] only — wake marker.
    Woke,
    /// Matches both: ignored (ambiguous).
    Ambiguous,
    /// Matches neither: an ordinary event.
    NotAMarker,
}

/// Classifies a title into a [`MarkerKind`].
pub fn marker_kind(title: &str) -> MarkerKind {
    let title = title.trim();
    match (asleep_re().is_match(title), woke_re().is_match(title)) {
        (true, true) => MarkerKind::Ambiguous,
        (true, false) => MarkerKind::Asleep,
        (false, true) => MarkerKind::Woke,
        (false, false) => MarkerKind::NotAMarker,
    }
}

/// One `Asleep` / `Woke Up` marker: the event's **start** instant plus its
/// effective IANA zone (start zone, else end zone when the start zone is
/// empty/invalid, else `UTC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SleepMarker {
    pub event_id: String,
    pub kind: MarkerKind,
    pub start_unix: i64,
    pub zone: String,
}

/// How a night row was derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepSource {
    /// Two markers paired inside `(0, 16h]`.
    Paired,
    /// Bedtime marker only: the wake was estimated.
    EstNoWake,
    /// Wake marker only: the bedtime was estimated.
    EstNoAsleep,
    /// Neither marker: the whole night was estimated.
    EstNone,
}

impl SleepSource {
    /// The stored `sleep_nights.source` string.
    pub fn as_str(self) -> &'static str {
        match self {
            SleepSource::Paired => "paired",
            SleepSource::EstNoWake => "est_no_wake",
            SleepSource::EstNoAsleep => "est_no_asleep",
            SleepSource::EstNone => "est_none",
        }
    }
}

/// One stored night. `seconds` is the **full marker delta**, never the
/// overlap-shared amount — rules read the real duration; the rollup reads the
/// shared one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NightRow {
    /// Civil date of the wake instant in the wake event's start zone.
    pub wake_date: String,
    /// RFC 3339 UTC `Z` instant of the Asleep marker's start.
    pub asleep_at: String,
    /// RFC 3339 UTC `Z` instant of the Woke Up marker's start.
    pub woke_at: String,
    pub seconds: i64,
    /// Bedtime as minutes past noon of `wake_date`'s day zone.
    pub bed_min_past_noon: i64,
    /// Wake time as minutes past midnight of the same zone.
    pub wake_min_past_midnight: i64,
    pub source: SleepSource,
}

/// A paired night before the day-zone lookup resolves its stored minutes.
///
/// The night is the **outer** span of its markers: after interruptions merge,
/// `asleep_idx` is the night's first Asleep and `woke_idx` its last Woke Up, and
/// `seconds` spans the awake gap between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairedNight {
    /// Indices into the marker slice passed to [`pair_markers`].
    pub asleep_idx: usize,
    pub woke_idx: usize,
    pub asleep_unix: i64,
    pub woke_unix: i64,
    /// Civil date of the wake instant in the **wake** marker's zone.
    pub wake_date: String,
    pub seconds: i64,
}

/// Marker pairing. **Deliberately different** from `scorer.py`'s
/// `_paired_nights`, which has no merge step, and from the latest-wake-first
/// walk that briefly replaced it here. Both failure modes:
///
/// * **The greedy cascade.** Giving each Asleep the first unused wake inside
///   `(0, 16h]` lets a 30-minute evening pair (Asleep 23:00, Woke Up 23:30)
///   take the evening wake; the real bedtime (Asleep 23:35) then takes the
///   morning wake, and one night becomes two — 0.5h plus ~6.4h — instead of the
///   23:00 → 06:00 night those four markers describe.
/// * **The over-merge.** Taking wakes latest-first and giving each the
///   **earliest** unused Asleep in `[wake - 16h, wake)` swallows naps: a
///   14:00 → 15:00 nap plus a 23:00 → 06:00 night becomes one 16h "night" with
///   a 14:00 bedtime, because 14:00 is exactly 16h before 06:00.
///
/// The rule is therefore:
///
/// 1. **Greedy pairs.** Walk the Asleep markers oldest-first; each takes the
///    first **unused** Woke Up whose start lies in `(asleep, asleep + 16h]` —
///    the shape `scorer.py` uses, minus its 2-hour floor. A wake that fails the
///    window is not consumed, and a wake is used at most once. On its own this
///    still splits 23:00/23:30 + 23:35/06:00 into two pairs.
/// 2. **Merge short interruptions.** Walk those pairs by Asleep start and fold
///    a pair into the current night when the awake gap
///    (`next.asleep_unix - current.woke_unix`) is at most
///    [`SLEEP_INTERRUPTION_MAX_SECS`] **and** the merged span
///    (`next.woke_unix - current.asleep_unix`) is positive and still within
///    [`SLEEP_PAIR_MAX_SECS`]. The chain repeats, so three short interruptions
///    become one night when every adjacent gap is short and the whole span
///    stays within 16h. A pair that would push the span past 16h starts a new
///    night instead.
///
/// The stored night is the **outer** span: the first Asleep's start to the last
/// Woke Up's start. The awake gap in the middle **is included** in `seconds`,
/// and `asleep_idx` / `woke_idx` are those outer markers.
///
/// Duration rules: `(0, 16h]` pairs, zero and negative never do, `16h` exactly
/// pairs and `16h + 1s` does not. The 2-hour floor stays gone, so a **lone**
/// short pair (22:00 → 22:30) still counts. A negative gap — the next Asleep
/// starts before the current wake, i.e. overlapping markers — is `<= 1h` by
/// construction, so it merges whenever the span still fits.
///
/// `markers` may be in any order; indices in the result refer to it.
pub(crate) fn pair_markers(markers: &[SleepMarker]) -> Vec<PairedNight> {
    let mut asleep: Vec<usize> = markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| marker.kind == MarkerKind::Asleep)
        .map(|(index, _)| index)
        .collect();
    let mut woke: Vec<usize> = markers
        .iter()
        .enumerate()
        .filter(|(_, marker)| marker.kind == MarkerKind::Woke)
        .map(|(index, _)| index)
        .collect();
    // Stable sort on the instant; ties keep input order (deterministic).
    asleep.sort_by_key(|&index| markers[index].start_unix);
    woke.sort_by_key(|&index| markers[index].start_unix);

    // ── Step 1: greedy pairs ────────────────────────────────────────────────
    let mut used_woke: Vec<bool> = vec![false; woke.len()];
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for &asleep_idx in &asleep {
        let asleep_unix = markers[asleep_idx].start_unix;
        // `woke` is sorted, so the first match is the earliest usable wake.
        // Zero and negative deltas fail the strict `start > asleep` and
        // `16h + 1s` fails the cap; a wake outside the window is skipped, not
        // consumed.
        let found = woke.iter().enumerate().find(|(slot, &index)| {
            let start = markers[index].start_unix;
            !used_woke[*slot]
                && start > asleep_unix
                && start - asleep_unix <= SLEEP_PAIR_MAX_SECS
        });
        let Some((slot, &woke_idx)) = found else {
            // No usable wake: this bedtime stays unpaired, for estimation.
            continue;
        };
        used_woke[slot] = true;
        pairs.push((asleep_idx, woke_idx));
    }

    // ── Step 2: merge short interruptions ───────────────────────────────────
    //
    // Step 1 walks the Asleep markers in order, so `pairs` is already sorted by
    // Asleep start and the fold below only ever extends the current night
    // forward. Pair wakes are strictly increasing too (a later pair can only
    // take a wake an earlier pair could not have taken), so the merged night's
    // wake is simply the later pair's.
    let mut nights: Vec<PairedNight> = Vec::new();
    let mut current: Option<PairedNight> = None;
    for (asleep_idx, woke_idx) in pairs {
        let asleep_unix = markers[asleep_idx].start_unix;
        let woke_unix = markers[woke_idx].start_unix;
        let merges = current.as_ref().is_some_and(|night| {
            asleep_unix - night.woke_unix <= SLEEP_INTERRUPTION_MAX_SECS
                && woke_unix - night.asleep_unix > 0
                && woke_unix - night.asleep_unix <= SLEEP_PAIR_MAX_SECS
        });
        if merges {
            let night = current.as_mut().expect("checked just above");
            night.woke_idx = woke_idx;
            night.woke_unix = woke_unix;
            night.wake_date = civil_date_in_zone(woke_unix, &markers[woke_idx].zone);
            night.seconds = woke_unix - night.asleep_unix;
            continue;
        }
        if let Some(night) = current.take() {
            nights.push(night);
        }
        current = Some(PairedNight {
            asleep_idx,
            woke_idx,
            asleep_unix,
            woke_unix,
            wake_date: civil_date_in_zone(woke_unix, &markers[woke_idx].zone),
            seconds: woke_unix - asleep_unix,
        });
    }
    if let Some(night) = current {
        nights.push(night);
    }
    nights.sort_by_key(|night| (night.wake_date.clone(), night.asleep_unix));
    nights
}

/// The bed marker for the night that wakes on `day1`, from the **unpaired**
/// Asleep markers: an Asleep whose own-zone-local civil date is `day0` with
/// local hour `>= 10`, or whose local civil date is `day1` with local hour
/// `< 10` (a post-midnight bedtime credited to the previous night). When
/// several qualify, the **latest** instant wins.
///
/// Returns an index into `unpaired`.
pub(crate) fn pick_bed_marker(
    day0: &str,
    day1: &str,
    unpaired: &[&SleepMarker],
) -> Option<usize> {
    let mut best: Option<(i64, usize)> = None;
    for (index, marker) in unpaired.iter().enumerate() {
        if marker.kind != MarkerKind::Asleep {
            continue;
        }
        let local_date = civil_date_in_zone(marker.start_unix, &marker.zone);
        let (hour, _) = zone_hour_minute(marker.start_unix, &marker.zone);
        let qualifies = (local_date == day0 && hour >= 10) || (local_date == day1 && hour < 10);
        if !qualifies {
            continue;
        }
        if best.is_none_or(|(instant, _)| marker.start_unix > instant) {
            best = Some((marker.start_unix, index));
        }
    }
    best.map(|(_, index)| index)
}

/// The wake marker for `day1` from the **unpaired** Woke markers: one whose
/// own-zone-local civil date is `day1`. The **earliest** wins.
///
/// Returns an index into `unpaired`.
pub(crate) fn pick_wake_marker(day1: &str, unpaired: &[&SleepMarker]) -> Option<usize> {
    let mut best: Option<(i64, usize)> = None;
    for (index, marker) in unpaired.iter().enumerate() {
        if marker.kind != MarkerKind::Woke {
            continue;
        }
        if civil_date_in_zone(marker.start_unix, &marker.zone) != day1 {
            continue;
        }
        if best.is_none_or(|(instant, _)| marker.start_unix < instant) {
            best = Some((marker.start_unix, index));
        }
    }
    best.map(|(_, index)| index)
}

/// First/last **real** (non-marker) event instants on one civil date, after
/// the overnight split and the "now" clip but **before** overlap sharing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayAnchors {
    /// Earliest start instant of a real event on the date.
    pub first_start: Option<i64>,
    /// Latest end instant of a real event on the date.
    pub last_end: Option<i64>,
}

/// One estimated night, before the stored minutes are resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EstimatedNight {
    pub asleep_unix: i64,
    pub woke_unix: i64,
    pub source: SleepSource,
    /// Zone the synthetic interval is split at local midnights in (the bed
    /// marker's own zone when there is one, else `day0`'s day zone).
    pub asleep_zone: String,
}

/// One day's civil date, resolved zone, and real-event anchors, as the
/// estimator sees them.
pub(crate) struct EstimateDay<'a> {
    /// `YYYY-MM-DD` of this day.
    pub date: &'a str,
    /// Resolved IANA zone for the date (vote winner, carry, fallback chain).
    pub zone: &'a str,
    pub anchors: &'a DayAnchors,
}

/// Estimates the night that wakes on `day1`, per `sleep_events.py`'s case
/// table, with the "missing anchor uses the default alone" fix (the legacy
/// code collapsed cases 1–2 to a zero-length night when the other day had no
/// events).
///
/// `default_sleep_secs` is the caller's estimated-night length — the user's
/// `sleep_duration` rule target, else [`DEFAULT_SLEEP_SECS`]. The caller
/// normalizes it, so it is always positive here.
///
/// * bed only → `end = min(bed + default, day1 first event)`, dropped when
///   `end <= bed`.
/// * wake only → `start = max(wake - default, day0 last event end)`, dropped
///   when `start >= wake`.
/// * neither → **dropped when neither civil date has a real (non-marker)
///   event** — a blank date must not invent a night (`sleep_events.py` only
///   inserted sleep between days that had events, and a backfill walks every
///   date since the first event). Otherwise `start = max(day0 last event end,
///   day0 21:00 local)`, `end = min(start + default, day1 first event)`,
///   dropped when `end <= start`.
///
/// The today guard lives in the caller: an in-progress night is not guessed.
pub(crate) fn estimate_night(
    bed: Option<&SleepMarker>,
    wake: Option<&SleepMarker>,
    day0: &EstimateDay<'_>,
    day1_day: &EstimateDay<'_>,
    default_sleep_secs: i64,
) -> Option<EstimatedNight> {
    let bed_instant = bed.map(|marker| marker.start_unix);
    let wake_instant = wake.map(|marker| marker.start_unix);

    match (bed_instant, wake_instant) {
        (Some(asleep_unix), None) => {
            let mut end = asleep_unix + default_sleep_secs;
            if let Some(first) = day1_day.anchors.first_start {
                end = end.min(first);
            }
            if end <= asleep_unix {
                return None;
            }
            Some(EstimatedNight {
                asleep_unix,
                woke_unix: end,
                source: SleepSource::EstNoWake,
                asleep_zone: bed
                    .map(|marker| marker.zone.clone())
                    .unwrap_or_else(|| day0.zone.to_string()),
            })
        }
        (None, Some(woke_unix)) => {
            let mut start = woke_unix - default_sleep_secs;
            if let Some(last) = day0.anchors.last_end {
                start = start.max(last);
            }
            if start >= woke_unix {
                return None;
            }
            Some(EstimatedNight {
                asleep_unix: start,
                woke_unix,
                source: SleepSource::EstNoAsleep,
                asleep_zone: day0.zone.to_string(),
            })
        }
        (None, None) => {
            // Neither day has a real (non-marker) event: nothing anchors a
            // night, so a blank civil date stays blank. Markers are excluded
            // from anchors, so a lone Asleep/Woke pair does not count here —
            // and a paired night never reaches this branch anyway.
            if day0.anchors.last_end.is_none() && day1_day.anchors.first_start.is_none() {
                return None;
            }
            let anchor_21 = local_hh_mm_unix(day0.date, 21, 0, day0.zone);
            let start = match (day0.anchors.last_end, anchor_21) {
                (Some(last), Some(anchor)) => last.max(anchor),
                (Some(last), None) => last,
                (None, Some(anchor)) => anchor,
                (None, None) => return None,
            };
            let mut end = start + default_sleep_secs;
            if let Some(first) = day1_day.anchors.first_start {
                end = end.min(first);
            }
            if end <= start {
                return None;
            }
            Some(EstimatedNight {
                asleep_unix: start,
                woke_unix: end,
                source: SleepSource::EstNone,
                asleep_zone: day0.zone.to_string(),
            })
        }
        // A bed AND a wake marker is the paired case — never estimated.
        (Some(_), Some(_)) => None,
    }
}

/// Minutes past noon for a bedtime: `hh:mm` (seconds truncated); pre-noon
/// instants are pushed 24h forward first. 22:30 → 630, 00:00 → 720,
/// 01:00 → 780.
pub fn bed_minutes_past_noon(unix: i64, zone: &str) -> i64 {
    let (hour, minute) = zone_hour_minute(unix, zone);
    let mut minutes = hour * 60 + minute;
    if minutes < NOON_MINUTES {
        minutes += 24 * 60;
    }
    minutes - NOON_MINUTES
}

/// Minutes past midnight for a wake instant: 05:00 → 300.
pub fn wake_minutes_past_midnight(unix: i64, zone: &str) -> i64 {
    let (hour, minute) = zone_hour_minute(unix, zone);
    hour * 60 + minute
}

/// Resolves the catalog's `sleep` category: the first living category whose
/// slug **or** title equals `sleep` case-insensitively. `None` → the caller
/// falls back to [`SYNTHETIC_SLEEP_CATEGORY_ID`].
///
/// Reads the already-loaded taxonomy (the tick loads it for the reclassify
/// job anyway) — no second source of truth and no extra query.
pub fn resolve_sleep_category_id(categories: &[crate::models::TaskCategory]) -> Option<String> {
    categories
        .iter()
        .find(|category| {
            category.slug.eq_ignore_ascii_case("sleep") || category.title.eq_ignore_ascii_case("sleep")
        })
        .map(|category| category.id.clone())
}

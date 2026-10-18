//! The pure consistency scorer: `score_rule` and `score_board`.
//!
//! No async, no repositories, no clock — callers pass two civil dates and the
//! already-loaded digest slice, so every rule of the model is unit-testable by
//! hand-built days. `score_as_of` is the card's cursor: the window, the strip,
//! `on_track`, and the owed numbers all end there. `today` is the user's real
//! civil today, and it alone is left provisional.
//!
//! # Units
//!
//! The digest stores **integer seconds**; hours are derived here and only
//! here (`seconds / 3600`). Bedtimes and wakes are minutes past noon /
//! midnight, exactly as `sleep_nights` stores them.
//!
//! # The model in one paragraph
//!
//! A verdict for date `D` with `window_days = N` looks at the **N calendar
//! days ending on `D`**, inclusive (`start = D - (N - 1)`), extended backward
//! by the paused days inside that span ([`window_start`]). Travel-excluded days
//! never extend anything: they are dropped from a daily average's numerator and
//! denominator, not replaced. Paused days are never counted and never break a
//! streak. Days before `starts_on` are out of play (strip `off`, no streak) but
//! their digest rows still feed the window of the first in-play days. Each day
//! is scored with the revision active on it. A window is only judged when it is
//! **full**: every non-paused day in it has a `daily_meta` row, so a hole the
//! backfill has not reached yet is never scored as a zero. `on_track` is the
//! window ending `score_as_of`; the real `today` is always `pending` in the
//! strip and never consumed by a streak. A finished challenge is scored at its
//! `ends_on`, so that last day is judged — and counts — while its phase is
//! still derived from `today`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Datelike;
use serde::Serialize;

use crate::digest::shift_date;

use super::params::{
    format_bedtime, format_wake, CategoryCatalog, CategoryDailyAvgParams, CategorySelector,
    Comparator, RuleKind, RuleParams, RulePhase, RuleStatus, SleepMetric, MAX_WINDOW_DAYS,
};
use super::RulesError;

/// Days in the strip (ending at `score_as_of`, oldest first).
pub const STRIP_DAYS: i64 = 14;
/// Hardest lookback for any window, and for the streak history.
pub const MAX_LOOKBACK_DAYS: i64 = 400;
/// Selected weekdays in a daily-average evidence list.
pub const EVIDENCE_WEEKDAYS: usize = 10;
/// Paired nights in a sleep evidence list.
pub const EVIDENCE_NIGHTS: usize = 14;
/// The owed sentence shown when a sleep rule cannot see enough nights.
pub const NOT_ENOUGH_NIGHTS: &str = "log Asleep / Woke Up — not enough paired nights";
/// The owed sentence shown while a window still has un-aggregated days.
pub const HISTORY_BUILDING: &str = "history still building";

/// Seconds in an hour (the only place the digest's seconds become hours).
const HOUR_SECS: f64 = 3600.0;

// ──────────────────────────────────────────
// Inputs
// ──────────────────────────────────────────

/// One `rules` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSpec {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub kind: RuleKind,
    pub status: RuleStatus,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub paused_on: Option<String>,
    pub sort_order: i64,
}

/// One `rule_revisions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub id: String,
    pub effective_from: String,
    pub params_json: String,
    pub created_at: String,
}

/// One `pause_periods` row (`rule_id = None` is global).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausePeriod {
    pub rule_id: Option<String>,
    pub from_date: String,
    pub to_date: String,
}

/// One **paired** night (`source = paired` only — estimates never score).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Night {
    pub wake_date: String,
    pub asleep_at: String,
    pub bed_min_past_noon: i64,
    pub wake_min_past_midnight: i64,
    pub seconds: i64,
    /// `paired | est_no_wake | est_no_asleep | est_none`. Only `paired` scores;
    /// the scorer filters again so a hand-built slice can never smuggle an
    /// estimate in.
    pub source: String,
}

/// The digest rows a scoreboard needs, keyed for the scorer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DigestSlice {
    /// `local_date` → `category_id` → seconds (`''` = unclassified real time).
    pub category_seconds: BTreeMap<String, BTreeMap<String, i64>>,
    /// Paired nights, any order.
    pub nights: Vec<Night>,
    /// The `daily_meta.local_date`s the digest has aggregated. A day that is
    /// not here is a hole, not a zero: the window containing it is not full.
    pub aggregated: BTreeSet<String>,
}

impl DigestSlice {
    /// Records one category's seconds on one day (test convenience).
    ///
    /// A day with rows is an aggregated day, so this marks it too.
    pub fn add(&mut self, date: &str, category_id: &str, seconds: i64) {
        self.aggregated.insert(date.to_string());
        *self
            .category_seconds
            .entry(date.to_string())
            .or_default()
            .entry(category_id.to_string())
            .or_insert(0) += seconds;
    }

    /// Records one paired night (test convenience).
    ///
    /// A paired night comes from a day rebuild, so its `wake_date` is
    /// aggregated.
    pub fn add_night(&mut self, wake_date: &str, bed_min_past_noon: i64, wake_min_past_midnight: i64, seconds: i64) {
        self.aggregated.insert(wake_date.to_string());
        self.nights.push(Night {
            wake_date: wake_date.to_string(),
            asleep_at: format!("{wake_date}T00:00:00Z"),
            bed_min_past_noon,
            wake_min_past_midnight,
            seconds,
            source: "paired".to_string(),
        });
    }

    /// Marks one day as aggregated — a computed day with nothing in it
    /// (test convenience).
    pub fn mark_aggregated(&mut self, date: &str) {
        self.aggregated.insert(date.to_string());
    }

    /// Marks the inclusive `[from, to]` range aggregated (test convenience).
    pub fn mark_aggregated_range(&mut self, from: &str, to: &str) {
        let mut cursor = Some(from.to_string());
        while let Some(date) = cursor {
            if date.as_str() > to {
                break;
            }
            self.aggregated.insert(date.clone());
            cursor = shift_date(&date, 1);
        }
    }
}

/// The digest job flags that ride along with the scoreboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DigestFlags {
    pub reclassifying: bool,
    pub backfilling: bool,
    pub dirty_days: i64,
}

/// Everything `score_board` needs.
pub struct BoardInput<'a> {
    pub as_of: &'a str,
    pub time_zone: &'a str,
    pub rules: Vec<RuleSpec>,
    /// Revisions by rule id (missing = the rule has none, which is an error).
    pub revisions: BTreeMap<String, Vec<Revision>>,
    pub pauses: Vec<PausePeriod>,
    pub catalog: &'a CategoryCatalog,
    pub digest: &'a DigestSlice,
    pub flags: DigestFlags,
}

// ──────────────────────────────────────────
// Outputs
// ──────────────────────────────────────────

/// One rule's rendered score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RuleScore {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub kind: String,
    pub phase: String,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub paused_on: Option<String>,
    pub sort_order: i64,
    pub condition: String,
    pub on_track: bool,
    /// Whether the window ending `score_as_of` has an aggregate row on every
    /// non-paused day in it. `false` means the backfill has not reached the
    /// whole window: `on_track` is `false`, `owed` says why, and the strip's
    /// in-play cells are `pending`.
    pub window_full: bool,
    pub actual_label: String,
    pub target_label: String,
    pub owed: String,
    pub streak: i64,
    pub best_streak: i64,
    pub compliance: Option<f64>,
    /// The params of the revision active on `score_as_of`, as the canonical
    /// JSON stored in `rule_revisions.params_json`. The client's edit form
    /// hydrates from this — the scoreboard is its only source.
    pub params: serde_json::Value,
    pub strip: Vec<StripCell>,
    pub evidence: Evidence,
}

/// One strip cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StripCell {
    pub date: String,
    /// `pass | fail | pending | paused | excluded | off`.
    pub state: String,
}

/// `category_total` evidence: the rolling window sum ending on each strip day.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidencePoint {
    pub date: String,
    /// `None` when the endpoint itself is paused (the strip cell is `paused`).
    pub hours: Option<f64>,
}

/// `category_days` evidence: the counted window ending `score_as_of`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceDay {
    pub date: String,
    pub hit: bool,
}

/// `category_daily_avg` evidence: the last selected weekdays ending
/// `score_as_of`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceWeekday {
    pub date: String,
    pub hours: f64,
    pub excluded: bool,
}

/// Sleep evidence: the newest paired nights.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceNight {
    pub wake_date: String,
    pub bed_min_past_noon: i64,
    pub wake_min_past_midnight: i64,
    pub hours: f64,
    pub source: String,
}

/// Internally tagged evidence, `type` = the rule kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Evidence {
    CategoryTotal { points: Vec<EvidencePoint> },
    CategoryDays { days: Vec<EvidenceDay> },
    CategoryDailyAvg { weekdays: Vec<EvidenceWeekday> },
    SleepDuration { nights: Vec<EvidenceNight> },
    SleepTiming { nights: Vec<EvidenceNight> },
}

/// The `GET /api/rules/scoreboard` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scoreboard {
    pub as_of: String,
    pub time_zone: String,
    /// Headline: passing rules over `active_rules` (paused rules excluded).
    pub on_track: usize,
    /// Rules on the board whose phase is `paused` — the "2 paused" chip.
    pub paused_rules: usize,
    pub active_rules: usize,
    pub all_pass_streak: i64,
    pub best_all_pass_streak: i64,
    pub reclassifying: bool,
    pub backfilling: bool,
    pub dirty_days: i64,
    /// Rules with phase `active` or `paused`, by `sort_order` then name.
    pub rules: Vec<RuleScore>,
    /// Operator-facing notes (a skipped seed, an unreadable rule row). Never
    /// serialized — the Worker logs them; the client never sees them.
    #[serde(skip)]
    pub warnings: Vec<String>,
}

/// The `GET /api/rules/archive` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArchiveResponse {
    pub rules: Vec<RuleScore>,
    /// Operator-facing notes, never serialized (see [`Scoreboard::warnings`]).
    #[serde(skip)]
    pub warnings: Vec<String>,
}

/// How one day lands for a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayState {
    Pass,
    Fail,
    Pending,
    Paused,
    Excluded,
    Off,
}

impl DayState {
    /// The strip cell string.
    pub fn as_str(self) -> &'static str {
        match self {
            DayState::Pass => "pass",
            DayState::Fail => "fail",
            DayState::Pending => "pending",
            DayState::Paused => "paused",
            DayState::Excluded => "excluded",
            DayState::Off => "off",
        }
    }
}

// ──────────────────────────────────────────
// The window
// ──────────────────────────────────────────

/// Why a window cannot be judged yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFull {
    /// The pause extension would start before
    /// `as_of - (MAX_LOOKBACK_DAYS - 1)`. The model refuses to truncate
    /// silently: the window is simply not full.
    Lookback,
    /// A non-paused day inside the window has no `daily_meta` row — the
    /// backfill has not reached it, so it is a hole rather than a zero.
    MissingDay(String),
}

impl std::fmt::Display for NotFull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotFull::Lookback => formatter.write_str("the window reaches past the lookback cap"),
            NotFull::MissingDay(date) => {
                write!(formatter, "{date} has no aggregated day yet")
            }
        }
    }
}

/// The first day of the `window_days` **calendar** window ending at `as_of`,
/// extended backward once for pauses.
///
/// The window is `N` calendar days ending on `as_of`, inclusive:
/// `start = as_of - (N - 1)`. Let `P` be the number of paused days in
/// `[start, as_of]`; the window then starts at `as_of - (N - 1) - P`, and the
/// paused days that extension pulled in are counted again — until a pass adds
/// no new paused day, or the lookback cap is hit ([`NotFull::Lookback`]).
/// Travel-excluded days never extend anything.
///
/// This is the **only** place the window's start date is computed: the scorer
/// and the digest load floor ([`digest_load_floor`]) both call it, so the SQL
/// never reads a narrower range than the scorer asks for.
pub fn window_start(
    as_of: &str,
    window_days: i64,
    is_paused: &dyn Fn(&str) -> bool,
) -> Result<String, NotFull> {
    let window = window_days.clamp(1, MAX_WINDOW_DAYS);
    let floor = shift_date(as_of, -(MAX_LOOKBACK_DAYS - 1)).ok_or(NotFull::Lookback)?;
    let base = shift_date(as_of, -(window - 1)).ok_or(NotFull::Lookback)?;
    let mut start = base.clone();
    let mut previous = 0_i64;
    loop {
        let paused = window_dates(&start, as_of).iter().filter(|date| is_paused(date)).count() as i64;
        if paused <= previous {
            // This pass added no new paused day: the window is settled.
            break;
        }
        let Some(earlier) = shift_date(&base, -paused) else {
            return Err(NotFull::Lookback);
        };
        if earlier < floor {
            return Err(NotFull::Lookback);
        }
        start = earlier;
        previous = paused;
    }
    Ok(start)
}

/// Every civil date in the inclusive `[from, to]`, oldest first.
fn window_dates(from: &str, to: &str) -> Vec<String> {
    let mut dates: Vec<String> = Vec::new();
    let mut cursor = Some(from.to_string());
    while let Some(date) = cursor {
        if date.as_str() > to {
            break;
        }
        dates.push(date.clone());
        cursor = shift_date(&date, 1);
    }
    dates
}

// ──────────────────────────────────────────
// Entry points
// ──────────────────────────────────────────

/// Scores one rule: `score_as_of` is the card's cursor, `today` is the real
/// civil today.
///
/// The live board passes the same date twice. A finished challenge passes its
/// `ends_on` as the cursor and today as itself, so the final day gets a real
/// verdict while the phase stays `finished`.
pub fn score_rule(
    spec: &RuleSpec,
    revisions: &[Revision],
    pauses: &[PausePeriod],
    catalog: &CategoryCatalog,
    digest: &DigestSlice,
    score_as_of: &str,
    today: &str,
) -> Result<RuleScore, RulesError> {
    Ok(
        Evaluator::new(spec, revisions, pauses, catalog, digest, score_as_of, today)?
            .evaluate()?
            .score,
    )
}

/// Scores the scoreboard: every rule's card plus the headline.
///
/// Every rule is *evaluated* (the all-pass walk needs the day states of rules
/// that have since left the board), but only the rules with phase `active` or
/// `paused` pay for a full card: strip, evidence, and params JSON.
pub fn score_board(input: BoardInput<'_>) -> Result<Scoreboard, RulesError> {
    let BoardInput {
        as_of,
        time_zone,
        rules,
        revisions,
        pauses,
        catalog,
        digest,
        flags,
    } = input;

    let mut specs = rules;
    specs.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.name.cmp(&right.name))
    });

    let empty: Vec<Revision> = Vec::new();
    let mut cards: Vec<(RuleSpec, RuleScore, BTreeMap<String, DayState>)> = Vec::new();
    let mut history: Vec<BTreeMap<String, DayState>> = Vec::new();
    for spec in &specs {
        let revisions = revisions.get(&spec.id).unwrap_or(&empty);
        // The board is live: its cursor is today, and today stays pending.
        let evaluator = Evaluator::new(spec, revisions, &pauses, catalog, digest, as_of, as_of)?;
        if rule_phase(spec, as_of).is_on_board() {
            let evaluation = evaluator.evaluate()?;
            history.push(evaluation.states.clone());
            cards.push((spec.clone(), evaluation.score, evaluation.states));
        } else {
            // Off the board: a compact state map is all the streak needs, and
            // the strip / evidence builder is never entered (M7).
            history.push(evaluator.state_map());
        }
    }

    let active: Vec<&RuleScore> = cards
        .iter()
        .map(|(_, score, _)| score)
        .filter(|score| score.phase == RulePhase::Active.as_str())
        .collect();
    let on_track = active.iter().filter(|score| score.on_track).count();
    let active_rules = active.len();
    let paused_rules = cards
        .iter()
        .filter(|(_, score, _)| score.phase == RulePhase::Paused.as_str())
        .count();

    // The all-pass walk considers every rule that was in play on a date —
    // including the ones that have since been archived or finished, so
    // archiving never rewrites the streak.
    let (all_pass_streak, best_all_pass_streak) = all_pass_walk(&specs, &history, as_of);

    let rules = cards.into_iter().map(|(_, score, _)| score).collect();

    Ok(Scoreboard {
        as_of: as_of.to_string(),
        time_zone: time_zone.to_string(),
        on_track,
        paused_rules,
        active_rules,
        all_pass_streak,
        best_all_pass_streak,
        reclassifying: flags.reclassifying,
        backfilling: flags.backfilling,
        dirty_days: flags.dirty_days,
        rules,
        warnings: Vec::new(),
    })
}

/// The all-pass runs over every rule's day states, from the earliest in-play
/// day through yesterday.
///
/// A day counts when at least one rule was in play on it (`Pass`/`Fail`), and
/// the run continues while every one of them passed. `Off` (out of play, an
/// unselected weekday) and `Paused` neither count nor break.
fn all_pass_walk(
    specs: &[RuleSpec],
    history: &[BTreeMap<String, DayState>],
    as_of: &str,
) -> (i64, i64) {
    let floor = shift_date(as_of, -(MAX_LOOKBACK_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    let Some(yesterday) = shift_date(as_of, -1) else {
        return (0, 0);
    };
    let start = specs
        .iter()
        .map(|spec| {
            if spec.starts_on > floor {
                spec.starts_on.clone()
            } else {
                floor.clone()
            }
        })
        .min()
        .unwrap_or_else(|| yesterday.clone());

    let mut current = 0_i64;
    let mut best = 0_i64;
    let mut cursor = Some(start);
    while let Some(date) = cursor {
        if date > yesterday {
            break;
        }
        let mut considered = 0_usize;
        let mut failed = false;
        for states in history {
            match states.get(&date).copied().unwrap_or(DayState::Off) {
                DayState::Pass => considered += 1,
                DayState::Fail => {
                    considered += 1;
                    failed = true;
                }
                _ => {}
            }
        }
        if considered > 0 {
            if failed {
                current = 0;
            } else {
                current += 1;
                best = best.max(current);
            }
        }
        cursor = shift_date(&date, 1);
    }
    (current, best)
}

/// The derived phase of a rule — never stored, and never written on read.
///
/// `archived` wins over everything; then a challenge whose inclusive `ends_on`
/// is behind `as_of` is **finished** (even while its status is `paused` — a
/// paused challenge still ends); then `paused`; else `active`.
pub fn rule_phase(spec: &RuleSpec, as_of: &str) -> RulePhase {
    if spec.status == RuleStatus::Archived {
        return RulePhase::Archived;
    }
    if spec
        .ends_on
        .as_deref()
        .is_some_and(|ends_on| ends_on < as_of)
    {
        return RulePhase::Finished;
    }
    match spec.status {
        RuleStatus::Paused => RulePhase::Paused,
        _ => RulePhase::Active,
    }
}

/// The earliest digest date a scoreboard can read.
///
/// The earliest day any rule is scored at is the earlier of its `starts_on`
/// and the strip start, clamped to the streak floor; every window that ends
/// there is [`window_start`] deep. The caller passes the same pause predicate
/// the scorer uses, so the SQL range can never be narrower than the scorer's
/// window.
pub fn digest_load_floor(
    min_starts_on: &str,
    max_window_days: i64,
    as_of: &str,
    is_paused: &dyn Fn(&str) -> bool,
) -> String {
    let strip_start = shift_date(as_of, -(STRIP_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    let history_floor =
        shift_date(as_of, -(MAX_LOOKBACK_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    // The earliest day any window is evaluated for: never before the streak
    // floor, never after the strip start.
    let anchor = min_starts_on
        .max(history_floor.as_str())
        .min(strip_start.as_str());
    window_start(anchor, max_window_days, is_paused).unwrap_or(history_floor)
}

// ──────────────────────────────────────────
// The evaluator
// ──────────────────────────────────────────

/// A revision with its selector already expanded.
struct ParsedRevision {
    effective_from: String,
    created_at: String,
    id: String,
    params: RuleParams,
    ids: BTreeSet<String>,
    exclude_ids: BTreeSet<String>,
}

struct Evaluation {
    score: RuleScore,
    /// `date → state` over the rule's history range.
    states: BTreeMap<String, DayState>,
}

/// What one window evaluation produced.
#[derive(Debug, Clone, Default)]
struct WindowOutcome {
    /// The window's non-paused days, oldest first — the scoring set.
    days: Vec<String>,
    /// Selector seconds over `days`.
    total_seconds: i64,
    /// Days clearing `min_minutes`.
    hits: i64,
    /// Paired nights on `days`, oldest first.
    nights: Vec<Night>,
    /// Median of the metric (hours for duration, minutes for timing).
    median: f64,
    /// Sample stdev in minutes.
    stdev: f64,
    /// `None` when every non-paused day in the window has an aggregate row.
    not_full: Option<NotFull>,
    /// The verdict, ignoring fullness: only meaningful when `not_full` is
    /// `None`.
    pass: bool,
}

impl WindowOutcome {
    /// Whether the window can be judged at all.
    fn is_full(&self) -> bool {
        self.not_full.is_none()
    }
}

struct Evaluator<'a> {
    spec: &'a RuleSpec,
    kind: RuleKind,
    revisions: Vec<ParsedRevision>,
    global_pauses: Vec<(String, String)>,
    rule_pauses: Vec<(String, String)>,
    catalog: &'a CategoryCatalog,
    sleep_ids: BTreeSet<String>,
    digest: &'a DigestSlice,
    /// The card's cursor: the window, strip, `on_track`, and owed numbers.
    score_as_of: &'a str,
    /// The user's real civil today — the only day left `pending`.
    today: &'a str,
}

impl<'a> Evaluator<'a> {
    fn new(
        spec: &'a RuleSpec,
        revisions: &[Revision],
        pauses: &[PausePeriod],
        catalog: &'a CategoryCatalog,
        digest: &'a DigestSlice,
        score_as_of: &'a str,
        today: &'a str,
    ) -> Result<Self, RulesError> {
        if revisions.is_empty() {
            return Err(RulesError::Invalid(format!(
                "rule {} has no revisions",
                spec.id
            )));
        }
        let mut parsed: Vec<ParsedRevision> = revisions
            .iter()
            .map(|revision| {
                let params = RuleParams::from_json(spec.kind, &revision.params_json)?;
                Ok(ParsedRevision {
                    effective_from: revision.effective_from.clone(),
                    created_at: revision.created_at.clone(),
                    id: revision.id.clone(),
                    ids: params
                        .selector()
                        .map(|selector| catalog.expand(selector))
                        .unwrap_or_default(),
                    exclude_ids: match &params {
                        RuleParams::CategoryDailyAvg(params) => params
                            .exclude_if
                            .as_ref()
                            .map(|exclude| catalog.expand(&exclude.categories))
                            .unwrap_or_default(),
                        _ => BTreeSet::new(),
                    },
                    params,
                })
            })
            .collect::<Result<_, RulesError>>()?;
        // The row active on D is the greatest effective_from <= D, ties broken
        // by created_at then id (latest wins).
        parsed.sort_by(|left, right| {
            (&left.effective_from, &left.created_at, &left.id).cmp(&(
                &right.effective_from,
                &right.created_at,
                &right.id,
            ))
        });

        let (global_pauses, rule_pauses) = pauses
            .iter()
            .filter(|pause| pause.rule_id.is_none() || pause.rule_id.as_deref() == Some(&spec.id))
            .fold((Vec::new(), Vec::new()), |(mut globals, mut rules), pause| {
                let interval = (pause.from_date.clone(), pause.to_date.clone());
                if pause.rule_id.is_none() {
                    globals.push(interval);
                } else {
                    rules.push(interval);
                }
                (globals, rules)
            });

        Ok(Self {
            spec,
            kind: spec.kind,
            revisions: parsed,
            global_pauses,
            rule_pauses,
            catalog,
            sleep_ids: catalog.sleep_ids(),
            digest,
            score_as_of,
            today,
        })
    }

    // ── dates and pauses ────────────────────────────────────────────────────

    /// The revision in force on `date` (falling back to the earliest one when
    /// `starts_on` was moved before every revision).
    fn params_on(&self, date: &str) -> &ParsedRevision {
        let index = self
            .revisions
            .iter()
            .rposition(|revision| revision.effective_from.as_str() <= date)
            .unwrap_or(0);
        &self.revisions[index]
    }

    /// Whether any pause covers `date` for this rule.
    fn is_paused(&self, date: &str) -> bool {
        let covers = |(from, to): &(String, String)| from.as_str() <= date && date <= to.as_str();
        if self.global_pauses.iter().any(covers) || self.rule_pauses.iter().any(covers) {
            return true;
        }
        self.spec.status == RuleStatus::Paused
            && self
                .spec
                .paused_on
                .as_deref()
                .is_some_and(|paused_on| paused_on <= date)
    }

    /// Whether the rule is in play on `date` (`starts_on ..= ends_on`).
    fn in_play(&self, date: &str) -> bool {
        self.spec.starts_on.as_str() <= date
            && self
                .spec
                .ends_on
                .as_deref()
                .map_or(true, |ends_on| date <= ends_on)
    }

    /// The derived phase, never stored. Phase is about the real today: a rule
    /// whose `ends_on` is behind it is finished even while its card is scored
    /// at that `ends_on`.
    fn phase(&self) -> RulePhase {
        rule_phase(self.spec, self.today)
    }

    // ── digest reads ────────────────────────────────────────────────────────

    /// Seconds the expanded selector scored on `date` (missing row = 0).
    fn selector_seconds(&self, ids: &BTreeSet<String>, date: &str) -> i64 {
        self.digest
            .category_seconds
            .get(date)
            .map(|day| {
                day.iter()
                    .filter(|(category_id, _)| ids.contains(category_id.as_str()))
                    .map(|(_, seconds)| *seconds)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Classified, non-sleep seconds on `date` — unclassified (`''`) is not
    /// "classified", so it never counts as waking.
    ///
    /// Waking is therefore **classified non-sleep time**: the unclassified
    /// bucket (`''`) is excluded. This **deliberately differs** from
    /// `scorer.py`, which treated unclassified time differently (it counted
    /// toward waking). The denominator only feeds
    /// `exclude_if.percent_of_waking` — the travel escape hatch — so the
    /// difference narrows an exclusion test rather than a score; see
    /// `unclassified_seconds_are_neither_productive_nor_waking`.
    fn waking_seconds(&self, date: &str) -> i64 {
        self.digest
            .category_seconds
            .get(date)
            .map(|day| {
                day.iter()
                    .filter(|(category_id, _)| {
                        !category_id.is_empty() && !self.sleep_ids.contains(category_id.as_str())
                    })
                    .map(|(_, seconds)| *seconds)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Whether `date` is travel-excluded for a daily-average revision.
    fn travel_excluded(&self, revision: &ParsedRevision, date: &str) -> bool {
        let RuleParams::CategoryDailyAvg(params) = &revision.params else {
            return false;
        };
        let Some(exclude) = &params.exclude_if else {
            return false;
        };
        let waking = self.waking_seconds(date);
        if waking <= 0 {
            return false;
        }
        let excluded = self.selector_seconds(&revision.exclude_ids, date);
        (excluded as f64) / (waking as f64) > exclude.percent_of_waking / 100.0
    }

    // ── the window ──────────────────────────────────────────────────────────

    /// Evaluates the window ending at `end` with `revision`'s params.
    ///
    /// The window is [`window_start`]'s calendar span: non-paused days score,
    /// paused days only extend it, and travel-excluded days are dropped from a
    /// daily average without being replaced. `not_full` is set when a
    /// non-paused day in it has no aggregate row, or when the pause extension
    /// ran past the lookback cap.
    fn outcome(&self, revision: &ParsedRevision, end: &str) -> WindowOutcome {
        let mut outcome = WindowOutcome::default();
        let start = match window_start(end, revision.params.window_days(), &|date| {
            self.is_paused(date)
        }) {
            Ok(start) => start,
            Err(reason) => {
                // Nothing can be read: report the hole, not a zero.
                outcome.not_full = Some(reason);
                return outcome;
            }
        };
        outcome.days = window_dates(&start, end)
            .into_iter()
            .filter(|date| !self.is_paused(date))
            .collect();
        // A day the backfill has not aggregated is a hole, not a zero.
        if let Some(missing) = outcome
            .days
            .iter()
            .find(|date| !self.digest.aggregated.contains(date.as_str()))
        {
            outcome.not_full = Some(NotFull::MissingDay(missing.clone()));
        }
        match &revision.params {
            RuleParams::CategoryTotal(params) => {
                outcome.total_seconds = outcome
                    .days
                    .iter()
                    .map(|date| self.selector_seconds(&revision.ids, date.as_str()))
                    .sum();
                let hours = outcome.total_seconds as f64 / HOUR_SECS;
                outcome.pass = params.comparator.passes(hours, params.target_hours);
            }
            RuleParams::CategoryDays(params) => {
                let threshold = threshold_seconds(params.min_minutes);
                outcome.hits = outcome
                    .days
                    .iter()
                    .filter(|date| self.selector_seconds(&revision.ids, date.as_str()) >= threshold)
                    .count() as i64;
                outcome.pass = outcome.hits >= params.min_days;
            }
            RuleParams::CategoryDailyAvg(params) => {
                // Only the selected weekdays inside the window, and only the
                // ones that are not travel-majority: those are dropped from
                // both the numerator and the denominator, never replaced by an
                // older weekday.
                let included: Vec<&String> = outcome
                    .days
                    .iter()
                    .filter(|date| {
                        weekday(date.as_str()).is_some_and(|day| params.weekdays.contains(&day))
                    })
                    .filter(|date| !self.travel_excluded(revision, date.as_str()))
                    .collect();
                outcome.total_seconds = included
                    .iter()
                    .map(|date| self.selector_seconds(&revision.ids, date.as_str()))
                    .sum();
                outcome.median = if included.is_empty() {
                    0.0
                } else {
                    outcome.total_seconds as f64 / HOUR_SECS / included.len() as f64
                };
                outcome.pass = outcome.median >= params.target_hours;
            }
            RuleParams::SleepDuration(params) => {
                outcome.nights = self.nights_on(&outcome.days);
                // The median is reported even when there are too few nights to
                // judge: the card shows what is logged, the `owed` sentence
                // says why it cannot be scored yet.
                let hours: Vec<f64> = outcome
                    .nights
                    .iter()
                    .map(|night| night.seconds as f64 / HOUR_SECS)
                    .collect();
                outcome.median = median(&hours);
                if outcome.nights.len() as i64 >= params.min_nights {
                    outcome.pass = outcome.median >= params.target_hours;
                }
            }
            RuleParams::SleepTiming(params) => {
                outcome.nights = self.nights_on(&outcome.days);
                let values: Vec<f64> = outcome
                    .nights
                    .iter()
                    .map(|night| {
                        if params.metric.is_bedtime() {
                            night.bed_min_past_noon as f64
                        } else {
                            night.wake_min_past_midnight as f64
                        }
                    })
                    .collect();
                outcome.median = median(&values);
                outcome.stdev = sample_stdev(&values);
                if outcome.nights.len() as i64 >= params.min_nights {
                    let value = if params.metric.is_spread() {
                        outcome.stdev
                    } else {
                        outcome.median
                    };
                    outcome.pass = params
                        .comparator
                        .passes(value, params.target_minutes as f64);
                }
            }
        }
        outcome
    }

    /// Paired nights whose `wake_date` is one of `dates`, oldest first.
    fn nights_on(&self, dates: &[String]) -> Vec<Night> {
        let set: BTreeSet<&str> = dates.iter().map(String::as_str).collect();
        let mut nights: Vec<Night> = self
            .digest
            .nights
            .iter()
            .filter(|night| night.source == "paired" && set.contains(night.wake_date.as_str()))
            .cloned()
            .collect();
        nights.sort_by(|left, right| {
            (&left.wake_date, &left.asleep_at).cmp(&(&right.wake_date, &right.asleep_at))
        });
        nights
    }

    // ── day states ──────────────────────────────────────────────────────────

    /// How `date` lands for this rule (the strip cell, and the streak input).
    fn day_state(&self, date: &str) -> DayState {
        if !self.in_play(date) {
            return DayState::Off;
        }
        if self.is_paused(date) {
            return DayState::Paused;
        }
        let revision = self.params_on(date);
        if let RuleParams::CategoryDailyAvg(params) = &revision.params {
            if !weekday(date).is_some_and(|day| params.weekdays.contains(&day)) {
                return DayState::Off;
            }
            if self.travel_excluded(revision, date) {
                return DayState::Excluded;
            }
        }
        // Only the real today is provisional: a finished challenge's last day
        // is judged even though it is the card's cursor.
        if date == self.today {
            return DayState::Pending;
        }
        let outcome = self.outcome(revision, date);
        if !outcome.is_full() {
            // The backfill has not reached this day's window: a hole is not a
            // failure.
            return DayState::Pending;
        }
        if outcome.pass {
            DayState::Pass
        } else {
            DayState::Fail
        }
    }

    /// The earliest date worth scoring: `starts_on`, clamped to the lookback.
    fn history_start(&self) -> String {
        let floor = shift_date(self.score_as_of, -(MAX_LOOKBACK_DAYS - 1))
            .unwrap_or_else(|| self.spec.starts_on.clone());
        if self.spec.starts_on > floor {
            self.spec.starts_on.clone()
        } else {
            floor
        }
    }

    fn state_map(&self) -> BTreeMap<String, DayState> {
        let mut out: BTreeMap<String, DayState> = BTreeMap::new();
        let mut cursor = Some(self.history_start());
        while let Some(date) = cursor {
            if date.as_str() > self.score_as_of {
                break;
            }
            out.insert(date.clone(), self.day_state(&date));
            cursor = shift_date(&date, 1);
        }
        out
    }

    // ── rendering ───────────────────────────────────────────────────────────

    fn evaluate(&self) -> Result<Evaluation, RulesError> {
        let states = self.state_map();
        let revision = self.params_on(self.score_as_of);
        let outcome = self.outcome(revision, self.score_as_of);
        let window_full = outcome.is_full();

        // A window that is not full has no verdict and no history: the card
        // says the history is still building instead of reporting a partial
        // number as a failure.
        let (mut streak, mut best_streak, mut compliance) = (0_i64, 0_i64, None);
        if window_full {
            let mut passes = 0_i64;
            let mut counted_completed = 0_i64;
            for (date, state) in &states {
                // A completed day: at or before the card's cursor and strictly
                // before the real today. Live, that is "through yesterday"; a
                // finished challenge scored at its `ends_on` counts that day
                // too.
                if date.as_str() > self.score_as_of || date.as_str() >= self.today {
                    continue;
                }
                match state {
                    DayState::Pass => {
                        streak += 1;
                        best_streak = best_streak.max(streak);
                        passes += 1;
                        counted_completed += 1;
                    }
                    DayState::Fail => {
                        streak = 0;
                        counted_completed += 1;
                    }
                    _ => {}
                }
            }
            compliance = if counted_completed == 0 {
                None
            } else {
                Some(passes as f64 / counted_completed as f64)
            };
        }

        let strip: Vec<StripCell> = self
            .strip_dates()
            .into_iter()
            .map(|date| StripCell {
                state: states
                    .get(&date)
                    .copied()
                    .unwrap_or(DayState::Off)
                    .as_str()
                    .to_string(),
                date,
            })
            .collect();

        let score = RuleScore {
            id: self.spec.id.clone(),
            name: self.spec.name.clone(),
            icon: self.spec.icon.clone(),
            kind: self.kind.as_str().to_string(),
            phase: self.phase().as_str().to_string(),
            starts_on: self.spec.starts_on.clone(),
            ends_on: self.spec.ends_on.clone(),
            paused_on: self.spec.paused_on.clone(),
            sort_order: self.spec.sort_order,
            condition: revision.params.condition(),
            on_track: window_full && outcome.pass,
            window_full,
            actual_label: self.actual_label(revision, &outcome),
            target_label: revision.params.target_label(),
            owed: self.owed(revision, &outcome),
            streak,
            best_streak,
            compliance,
            // The canonical JSON round-trips through `serde_json::Value` so the
            // client reads exactly what a `PATCH` would re-send.
            params: serde_json::from_str(&revision.params.to_json())
                .expect("rule params always serialize to JSON"),
            strip,
            evidence: self.evidence(revision, &outcome),
        };
        Ok(Evaluation { score, states })
    }

    /// The 14 civil dates ending at `score_as_of`, oldest first.
    fn strip_dates(&self) -> Vec<String> {
        let mut dates: Vec<String> = Vec::with_capacity(STRIP_DAYS as usize);
        let mut cursor = Some(self.score_as_of.to_string());
        for _ in 0..STRIP_DAYS {
            let Some(date) = cursor else {
                break;
            };
            dates.push(date.clone());
            cursor = shift_date(&date, -1);
        }
        dates.reverse();
        dates
    }

    fn actual_label(&self, revision: &ParsedRevision, outcome: &WindowOutcome) -> String {
        match &revision.params {
            RuleParams::CategoryTotal(_) => {
                format!("{:.1}h", outcome.total_seconds as f64 / HOUR_SECS)
            }
            RuleParams::CategoryDays(_) => format!("{} days", outcome.hits),
            RuleParams::CategoryDailyAvg(_) => format!("{:.2}h mean", outcome.median),
            RuleParams::SleepDuration(_) => format!("{:.2}h median", outcome.median),
            RuleParams::SleepTiming(params) => {
                if params.metric.is_spread() {
                    format!("{}m", outcome.stdev.round() as i64)
                } else if params.metric.is_bedtime() {
                    format_bedtime(outcome.median)
                } else {
                    format_wake(outcome.median)
                }
            }
        }
    }

    /// The rule's actual params — never hardcoded 12 / 7 / 4.
    ///
    /// A window that is not full owes nothing but history: there is no honest
    /// number to report yet.
    fn owed(&self, revision: &ParsedRevision, outcome: &WindowOutcome) -> String {
        if !outcome.is_full() {
            return HISTORY_BUILDING.to_string();
        }
        match &revision.params {
            RuleParams::CategoryTotal(params) => {
                if outcome.pass {
                    return "on track".to_string();
                }
                let hours = outcome.total_seconds as f64 / HOUR_SECS;
                let gap = (params.target_hours - hours).abs();
                match params.comparator {
                    Comparator::Gte => format!("need +{gap:.1}h"),
                    Comparator::Lte => format!("need -{gap:.1}h"),
                }
            }
            RuleParams::CategoryDays(params) => {
                if outcome.pass {
                    return "on track".to_string();
                }
                format!(
                    "need {} more day(s) with ≥{} {}",
                    params.min_days - outcome.hits,
                    super::params::compact_minutes(params.min_minutes),
                    self.category_noun(&params.categories)
                )
            }
            RuleParams::CategoryDailyAvg(params) => {
                let mut owed = if outcome.pass {
                    "on track".to_string()
                } else {
                    format!("need +{:.1}h on the mean", params.target_hours - outcome.median)
                };
                // The live progress rides along even when the mean already
                // passes: the card is about today, not only about the gap.
                if let Some(today) = self.today_fragment(revision, params) {
                    owed.push(' ');
                    owed.push_str(&today);
                }
                owed
            }
            RuleParams::SleepDuration(params) => {
                if (outcome.nights.len() as i64) < params.min_nights {
                    return NOT_ENOUGH_NIGHTS.to_string();
                }
                if outcome.pass {
                    "on track".to_string()
                } else {
                    format!("median is {:.2}h short", params.target_hours - outcome.median)
                }
            }
            RuleParams::SleepTiming(params) => {
                if (outcome.nights.len() as i64) < params.min_nights {
                    return NOT_ENOUGH_NIGHTS.to_string();
                }
                if outcome.pass {
                    return "on track".to_string();
                }
                let symbol = params.comparator.symbol();
                let target = params.target_minutes as f64;
                match params.metric {
                    SleepMetric::BedtimeMedian => format!(
                        "median bed {} ({symbol}{})",
                        format_bedtime(outcome.median),
                        format_bedtime(target)
                    ),
                    SleepMetric::WakeMedian => format!(
                        "median wake {} ({symbol}{})",
                        format_wake(outcome.median),
                        format_wake(target)
                    ),
                    SleepMetric::BedtimeStdev => format!(
                        "bed stdev {}m ({symbol}{}m)",
                        outcome.stdev.round() as i64,
                        params.target_minutes
                    ),
                    SleepMetric::WakeStdev => format!(
                        "wake stdev {}m ({symbol}{}m)",
                        outcome.stdev.round() as i64,
                        params.target_minutes
                    ),
                }
            }
        }
    }

    /// `today 4.2 / 7.0h · 2.8h left` — only on the live board (the card's
    /// cursor is the real today) and when today is an included weekday. A
    /// frozen final day must never be described as "today". The "left" clause
    /// is dropped once today's own hours already clear the target.
    fn today_fragment(
        &self,
        revision: &ParsedRevision,
        params: &CategoryDailyAvgParams,
    ) -> Option<String> {
        if self.score_as_of != self.today {
            return None;
        }
        let today = self.today;
        if !self.in_play(today) || self.is_paused(today) {
            return None;
        }
        if !weekday(today).is_some_and(|day| params.weekdays.contains(&day)) {
            return None;
        }
        if self.travel_excluded(revision, today) {
            return None;
        }
        let so_far = self.selector_seconds(&revision.ids, today) as f64 / HOUR_SECS;
        let target = params.target_hours;
        let mut fragment = format!("today {so_far:.1} / {target:.1}h");
        if so_far < target {
            fragment.push_str(&format!(" · {:.1}h left", target - so_far));
        }
        Some(fragment)
    }

    /// The lowercase noun a `category_days` owed sentence uses: the selector's
    /// own category (`fitness`), never the rule's display name (which the user
    /// may have renamed to anything).
    fn category_noun(&self, selector: &CategorySelector) -> String {
        match selector {
            CategorySelector::AllProductive => "productive".to_string(),
            CategorySelector::Ids { ids, .. } => ids
                .iter()
                .find_map(|id| self.catalog.get(id))
                .map(|cat| cat.title.to_lowercase())
                .unwrap_or_else(|| self.spec.name.to_lowercase()),
        }
    }

    fn evidence(&self, revision: &ParsedRevision, outcome: &WindowOutcome) -> Evidence {
        match &revision.params {
            RuleParams::CategoryTotal(_) => Evidence::CategoryTotal {
                points: self
                    .strip_dates()
                    .into_iter()
                    .map(|date| {
                        // A day that is not in play (before `starts_on`, after
                        // `ends_on`) or paused has no window to sum.
                        if self.is_paused(&date) || !self.in_play(&date) {
                            return EvidencePoint { date, hours: None };
                        }
                        let revision = self.params_on(&date);
                        let outcome = self.outcome(revision, &date);
                        EvidencePoint {
                            date,
                            hours: Some(outcome.total_seconds as f64 / HOUR_SECS),
                        }
                    })
                    .collect(),
            },
            RuleParams::CategoryDays(params) => {
                let threshold = threshold_seconds(params.min_minutes);
                Evidence::CategoryDays {
                    days: outcome
                        .days
                        .iter()
                        .map(|date| EvidenceDay {
                            date: date.clone(),
                            hit: self.selector_seconds(&revision.ids, date) >= threshold,
                        })
                        .collect(),
                }
            }
            RuleParams::CategoryDailyAvg(params) => Evidence::CategoryDailyAvg {
                weekdays: self
                    .evidence_weekdays(revision, params)
                    .into_iter()
                    .map(|(date, excluded)| EvidenceWeekday {
                        hours: self.selector_seconds(&revision.ids, &date) as f64 / HOUR_SECS,
                        date,
                        excluded,
                    })
                    .collect(),
            },
            RuleParams::SleepDuration(_) => Evidence::SleepDuration {
                nights: self.evidence_nights(outcome),
            },
            RuleParams::SleepTiming(_) => Evidence::SleepTiming {
                nights: self.evidence_nights(outcome),
            },
        }
    }

    /// The last `EVIDENCE_WEEKDAYS` selected weekdays ending at `score_as_of`,
    /// oldest first — travel-excluded ones included so the UI can grey them.
    /// Paused days are not counted, so they do not appear.
    fn evidence_weekdays(
        &self,
        revision: &ParsedRevision,
        params: &CategoryDailyAvgParams,
    ) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = Vec::new();
        let mut cursor = Some(self.score_as_of.to_string());
        let mut examined = 0_i64;
        while let Some(date) = cursor {
            if examined >= MAX_LOOKBACK_DAYS {
                break;
            }
            examined += 1;
            if !self.is_paused(&date)
                && weekday(&date).is_some_and(|day| params.weekdays.contains(&day))
            {
                let excluded = self.travel_excluded(revision, &date);
                out.push((date.clone(), excluded));
                if out.len() >= EVIDENCE_WEEKDAYS {
                    break;
                }
            }
            cursor = shift_date(&date, -1);
        }
        out.reverse();
        out
    }

    /// The newest `EVIDENCE_NIGHTS` paired nights **inside the scoring
    /// window**, oldest first.
    ///
    /// A window is the unit a sleep rule is judged over, so its evidence is
    /// that window's nights — never an unbounded last-14 from all history.
    /// Still capped at `EVIDENCE_NIGHTS`.
    fn evidence_nights(&self, outcome: &WindowOutcome) -> Vec<EvidenceNight> {
        let mut nights: Vec<&Night> = outcome.nights.iter().collect();
        if nights.len() > EVIDENCE_NIGHTS {
            nights.drain(..nights.len() - EVIDENCE_NIGHTS);
        }
        nights
            .into_iter()
            .map(|night| EvidenceNight {
                wake_date: night.wake_date.clone(),
                bed_min_past_noon: night.bed_min_past_noon,
                wake_min_past_midnight: night.wake_min_past_midnight,
                hours: night.seconds as f64 / HOUR_SECS,
                source: "paired".to_string(),
            })
            .collect()
    }
}

// ──────────────────────────────────────────
// Small math
// ──────────────────────────────────────────

/// `min_minutes * 60` with the unrepresentable case pinned to `i64::MAX` — a
/// threshold nothing can clear, never a debug overflow.
///
/// `RuleParams::validate` rejects such a value on the way in, so this only
/// guards a hand-edited row.
fn threshold_seconds(min_minutes: i64) -> i64 {
    min_minutes.saturating_mul(60)
}

/// `0 = Monday … 6 = Sunday` (`chrono::Datelike::num_days_from_monday`).
fn weekday(date: &str) -> Option<u32> {
    let day = <chrono::NaiveDate as std::str::FromStr>::from_str(date).ok()?;
    Some(day.weekday().num_days_from_monday())
}

/// Python `statistics.median`: the middle value, or the mean of the two middle
/// values for an even count. An empty slice is `0.0`.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let len = sorted.len();
    if len % 2 == 1 {
        sorted[len / 2]
    } else {
        (sorted[len / 2 - 1] + sorted[len / 2]) / 2.0
    }
}

/// Python `statistics.stdev`: the sample stdev (divisor `n - 1`). Fewer than
/// two values is `0.0`.
fn sample_stdev(values: &[f64]) -> f64 {
    let len = values.len();
    if len < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / len as f64;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (len - 1) as f64;
    variance.sqrt()
}

// ──────────────────────────────────────────
// Tests
// ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::params::{
        CategoryDailyAvgParams, CategoryDaysParams, CategorySelector, CategoryTotalParams, Cat,
        Comparator, ExcludeIf, RuleKind, RuleStatus, SleepDurationParams, SleepMetric,
        SleepTimingParams,
    };

    // ── fixtures ────────────────────────────────────────────────────────────

    /// 2026-10-18 is a Sunday; 2026-10-19 a Monday.
    const SUNDAY: &str = "2026-10-18";
    const MONDAY: &str = "2026-10-19";
    const EARLY: &str = "2026-06-01";

    fn catalog() -> CategoryCatalog {
        CategoryCatalog::new(vec![
            Cat::new("fitness", None, false, "fitness", "Fitness"),
            Cat::new("learning", None, false, "learning", "Learning"),
            Cat::new("work", None, true, "work", "Work"),
            Cat::new("work-deep", Some("work"), false, "deep", "Deep Work"),
            Cat::new("trip", None, false, "trip", "Trip"),
            Cat::new("travel", None, false, "travel", "Travel"),
            Cat::new("sleep", None, false, "sleep", "Sleep"),
        ])
    }

    fn spec(id: &str, name: &str, kind: RuleKind, starts_on: &str) -> RuleSpec {
        RuleSpec {
            id: id.to_string(),
            name: name.to_string(),
            icon: String::new(),
            kind,
            status: RuleStatus::Active,
            starts_on: starts_on.to_string(),
            ends_on: None,
            paused_on: None,
            sort_order: 0,
        }
    }

    fn revision(effective_from: &str, params: &RuleParams) -> Revision {
        Revision {
            id: format!("rev-{effective_from}"),
            effective_from: effective_from.to_string(),
            params_json: params.to_json(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn days_params(ids: &[&str], window: i64, min_minutes: i64, min_days: i64) -> RuleParams {
        RuleParams::CategoryDays(CategoryDaysParams {
            categories: CategorySelector::Ids {
                ids: ids.iter().map(|id| id.to_string()).collect(),
                include_children: true,
            },
            window_days: window,
            min_minutes,
            min_days,
        })
    }

    fn total_params(
        selector: CategorySelector,
        window: i64,
        comparator: Comparator,
        target_hours: f64,
    ) -> RuleParams {
        RuleParams::CategoryTotal(CategoryTotalParams {
            categories: selector,
            window_days: window,
            comparator,
            target_hours,
        })
    }

    fn ids(ids: &[&str]) -> CategorySelector {
        CategorySelector::Ids {
            ids: ids.iter().map(|id| id.to_string()).collect(),
            include_children: true,
        }
    }

    fn avg_params(
        selector: CategorySelector,
        weekdays: &[u32],
        window: i64,
        target_hours: f64,
        exclude: Option<(&[&str], f64)>,
    ) -> RuleParams {
        RuleParams::CategoryDailyAvg(CategoryDailyAvgParams {
            categories: selector,
            window_days: window,
            weekdays: weekdays.to_vec(),
            target_hours,
            exclude_if: exclude.map(|(ids, percent)| ExcludeIf {
                categories: CategorySelector::Ids {
                    ids: ids.iter().map(|id| id.to_string()).collect(),
                    include_children: true,
                },
                percent_of_waking: percent,
            }),
        })
    }

    fn duration_params(window: i64, min_nights: i64, target_hours: f64) -> RuleParams {
        RuleParams::SleepDuration(SleepDurationParams {
            window_days: window,
            min_nights,
            target_hours,
        })
    }

    fn timing_params(
        metric: SleepMetric,
        comparator: Comparator,
        target_minutes: i64,
        window: i64,
        min_nights: i64,
    ) -> RuleParams {
        RuleParams::SleepTiming(SleepTimingParams {
            window_days: window,
            min_nights,
            metric,
            comparator,
            target_minutes,
        })
    }

    /// The `n` civil dates ending at `as_of`, oldest first.
    fn days_ending(as_of: &str, n: i64) -> Vec<String> {
        (0..n)
            .rev()
            .map(|offset| shift_date(as_of, -offset).expect("valid date"))
            .collect()
    }

    fn hours(digest: &mut DigestSlice, date: &str, category_id: &str, value: f64) {
        digest.add(date, category_id, (value * 3600.0).round() as i64);
    }

    /// A digest whose inclusive `[from, to]` range is already aggregated — no
    /// backfill hole in the window (M3).
    fn digest_with_days(from: &str, to: &str) -> DigestSlice {
        let mut digest = DigestSlice::default();
        digest.mark_aggregated_range(from, to);
        digest
    }

    /// The window's first day for a `window`-day span ending at `as_of`, with
    /// no pauses.
    fn window_from(as_of: &str, window: i64) -> String {
        shift_date(as_of, -(window - 1)).expect("valid date")
    }

    /// One closed pause period.
    fn pause(rule_id: Option<&str>, from: &str, to: &str) -> PausePeriod {
        PausePeriod {
            rule_id: rule_id.map(|id| id.to_string()),
            from_date: from.to_string(),
            to_date: to.to_string(),
        }
    }

    /// Scores with the card's cursor and the real today both at `as_of` — the
    /// live board.
    fn run(
        spec: &RuleSpec,
        revisions: &[Revision],
        catalog: &CategoryCatalog,
        digest: &DigestSlice,
        as_of: &str,
    ) -> RuleScore {
        score_rule(spec, revisions, &[], catalog, digest, as_of, as_of).expect("rule scores")
    }

    fn cell(score: &RuleScore, date: &str) -> String {
        score
            .strip
            .iter()
            .find(|cell| cell.date == date)
            .unwrap_or_else(|| panic!("strip has {date}"))
            .state
            .clone()
    }

    fn evidence_points(score: &RuleScore) -> Vec<(String, Option<f64>)> {
        match &score.evidence {
            Evidence::CategoryTotal { points } => {
                points.iter().map(|point| (point.date.clone(), point.hours)).collect()
            }
            other => panic!("expected category_total evidence, got {other:?}"),
        }
    }

    // ── 1. fitness pass/fail ────────────────────────────────────────────────

    #[test]
    fn category_days_passes_at_twelve_hits_and_fails_at_eleven() {
        let catalog = catalog();
        let rule = spec("r-fitness", "Fitness", RuleKind::CategoryDays, EARLY);
        let revisions = [revision(EARLY, &days_params(&["fitness"], 60, 60, 12))];
        let window = days_ending(SUNDAY, 60);

        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in window.iter().take(12) {
            hours(&mut digest, date, "fitness", 1.0);
        }
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.window_full);
        assert!(score.on_track);
        assert_eq!(score.owed, "on track");
        assert_eq!(score.actual_label, "12 days");
        assert_eq!(score.target_label, "12 days / 60d");
        assert_eq!(score.condition, "≥12 days of ≥1h / 60d");

        // One second short of the threshold is not a hit.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in window.iter().take(11) {
            hours(&mut digest, date, "fitness", 1.0);
        }
        digest.add(&window[11], "fitness", 3599);
        digest.add(&window[12], "fitness", 50 * 60);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert!(score.owed.contains("1 more day"), "{}", score.owed);
        assert_eq!(
            score.owed,
            "need 1 more day(s) with ≥1h fitness",
            "the owed noun is the category, never the rule's display name"
        );
        assert_eq!(score.actual_label, "11 days");

        // A single day of exactly one hour does hit.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        digest.add(&window[59], "fitness", 3600);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "1 days");

        // The noun follows the selector: `all_productive` is `productive`.
        let productive = spec("r-productive", "Fitness", RuleKind::CategoryDays, EARLY);
        let revisions = [revision(
            EARLY,
            &RuleParams::CategoryDays(CategoryDaysParams {
                categories: CategorySelector::AllProductive,
                window_days: 60,
                min_minutes: 60,
                min_days: 12,
            }),
        )];
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        hours(&mut digest, SUNDAY, "work", 1.0);
        let score = run(&productive, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.owed.ends_with("productive"), "{}", score.owed);
    }

    // ── 2. one total per category per day ───────────────────────────────────

    #[test]
    fn one_day_holds_one_total_per_category() {
        // The scorer sees `date -> category -> seconds`; two slices of the same
        // category on one day are already summed by the loader (and by
        // `DigestSlice::add`), so 1800 + 1800 is a single 3600 row.
        let catalog = catalog();
        let spec = spec("r-fitness", "Fitness", RuleKind::CategoryDays, EARLY);
        let revisions = [revision(EARLY, &days_params(&["fitness"], 1, 60, 1))];
        let mut digest = DigestSlice::default();
        digest.add(SUNDAY, "fitness", 1800);
        digest.add(SUNDAY, "fitness", 1800);
        assert_eq!(digest.category_seconds[SUNDAY].len(), 1);
        assert_eq!(digest.category_seconds[SUNDAY]["fitness"], 3600);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "1 days");
    }

    // ── 3. the calendar window ──────────────────────────────────────────────

    /// The pin test for M1/M2: 60 **calendar** days, not 60 counted weekdays.
    #[test]
    fn a_sixty_day_window_is_calendar_days_not_counted_weekdays() {
        let catalog = catalog();
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(
                CategorySelector::AllProductive,
                &[0, 1, 2, 3, 4],
                60,
                7.0,
                Some((&["trip", "travel"], 50.0)),
            ),
        )];
        // `SUNDAY` is 2026-10-18; the window's first day is 2026-08-20.
        let start = window_from(SUNDAY, 60);
        assert_eq!(start, "2026-08-20");
        let window = days_ending(SUNDAY, 60);
        assert_eq!(window.first(), Some(&start));
        let weekdays: Vec<&String> = window
            .iter()
            .filter(|date| weekday(date).is_some_and(|day| day <= 4))
            .collect();
        assert_eq!(weekdays.len(), 42, "exactly 42 Mon–Fri dates in the span");

        // Every selected weekday is 8h except one travel-excluded Wednesday.
        let travel = "2026-10-14";
        assert_eq!(weekday(travel), Some(2), "a Wednesday inside the window");
        let mut digest = digest_with_days(&start, SUNDAY);
        for date in &weekdays {
            if date.as_str() == travel {
                continue;
            }
            hours(&mut digest, date.as_str(), "work", 8.0);
        }
        hours(&mut digest, travel, "work", 2.0);
        hours(&mut digest, travel, "travel", 3.0);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);

        assert!(score.window_full);
        assert_eq!(cell(&score, travel), "excluded", "still shown as excluded");
        // 41 weekdays, not 42 and not backfilled with an older Wednesday:
        // (41 * 8) / 41 = 8.00h.
        assert_eq!(score.actual_label, "8.00h mean");
        assert!(score.on_track);
        match &score.evidence {
            Evidence::CategoryDailyAvg { weekdays } => {
                assert!(
                    weekdays.iter().any(|day| day.date == travel && day.excluded),
                    "the excluded Wednesday is on the strip"
                );
            }
            other => panic!("expected daily-avg evidence, got {other:?}"),
        }

        // If the excluded day were replaced by an older weekday, the count
        // would be 42 — prove the window is the calendar span by adding an
        // older Monday: it is outside the window and must not move the mean.
        hours(&mut digest, "2026-08-17", "work", 8.0);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "8.00h mean");
    }

    /// The pin test for the pause extension: one paused day inside the window
    /// pulls the start one day earlier, and the paused day is not scored.
    #[test]
    fn a_pause_extends_the_calendar_window_by_paused_days() {
        let catalog = catalog();
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(CategorySelector::AllProductive, &[0, 1, 2, 3, 4], 60, 7.0, None),
        )];
        let start = window_from(SUNDAY, 60);
        assert_eq!(start, "2026-08-20");
        let paused_day = "2026-10-14"; // a Wednesday, inside the strip
        let pauses = [pause(Some("r-productive"), paused_day, paused_day)];
        let is_paused = |date: &str| date == paused_day;
        assert_eq!(
            window_start(SUNDAY, 60, &is_paused).expect("a window"),
            "2026-08-19",
            "one paused day pulls the start one day earlier"
        );

        // Every selected weekday is 8h, so the paused Wednesday must not be in
        // the mean (42 weekdays, not 43, and not 41).
        let mut digest = digest_with_days("2026-08-19", SUNDAY);
        for date in days_ending(SUNDAY, 60) {
            if weekday(&date).is_some_and(|day| day <= 4) {
                hours(&mut digest, date.as_str(), "work", 8.0);
            }
        }
        hours(&mut digest, "2026-08-19", "work", 8.0); // a Wednesday, in reach
        let score = score_rule(&spec, &revisions, &pauses, &catalog, &digest, SUNDAY, SUNDAY)
            .expect("scores");
        assert_eq!(cell(&score, paused_day), "paused");
        assert_eq!(
            score.actual_label, "8.00h mean",
            "the paused Wednesday is out of the mean"
        );
        assert!(score.window_full);

        // A cascade: a second paused day just inside the extension keeps
        // pulling the start earlier.
        let cascade = ["2026-09-16", "2026-08-20"];
        let is_paused = |date: &str| cascade.contains(&date);
        assert_eq!(
            window_start(SUNDAY, 60, &is_paused).expect("a window"),
            "2026-08-18",
            "two paused days inside the span"
        );
    }

    #[test]
    fn a_paused_day_does_not_break_a_streak() {
        let catalog = catalog();
        let spec = spec("r-fitness", "Fitness", RuleKind::CategoryDays, "2026-10-15");
        let revisions = [revision("2026-10-15", &days_params(&["fitness"], 1, 60, 1))];
        let mut digest = digest_with_days("2026-10-15", SUNDAY);
        hours(&mut digest, "2026-10-15", "fitness", 1.0);
        hours(&mut digest, "2026-10-17", "fitness", 1.0);
        let pauses = [pause(Some("r-fitness"), "2026-10-16", "2026-10-16")];
        let score = score_rule(
            &spec, &revisions, &pauses, &catalog, &digest, SUNDAY, SUNDAY,
        )
        .expect("scores");
        assert_eq!(cell(&score, "2026-10-16"), "paused");
        assert_eq!(score.streak, 2);
        assert_eq!(score.best_streak, 2);
        assert_eq!(score.compliance, Some(1.0));
    }

    // ── 4. learning window and comparators ──────────────────────────────────

    #[test]
    fn category_total_window_and_comparators() {
        let catalog = catalog();
        let rule = spec("r-learning", "Learning", RuleKind::CategoryTotal, EARLY);
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let window = days_ending(SUNDAY, 7);

        let mut digest = digest_with_days(&window[0], SUNDAY);
        digest.add(&window[3], "learning", 4 * 3600);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "4.0h");
        assert_eq!(score.target_label, "≥4h / 7d");
        assert_eq!(score.condition, "≥4h / 7d");
        assert_eq!(evidence_points(&score).len(), 14);

        let mut digest = digest_with_days(&window[0], SUNDAY);
        digest.add(&window[3], "learning", 4 * 3600 - 1);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "need +0.0h");
        assert_eq!(score.actual_label, "4.0h");

        // `lte` fails when the sum is over the target.
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Lte, 1.0),
        )];
        let mut digest = digest_with_days(&window[0], SUNDAY);
        digest.add(&window[3], "learning", 2 * 3600);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "need -1.0h");
        assert_eq!(score.condition, "≤1h / 7d");

        // A day outside the window does not count.
        let mut digest = digest_with_days(&days_ending(SUNDAY, 8)[0], SUNDAY);
        digest.add(&days_ending(SUNDAY, 8)[0], "learning", 4 * 3600);
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);

        // A day in play but outside the window is `null` in the evidence, not
        // a zero sum.
        let points = evidence_points(&score);
        assert_eq!(points.last().map(|(date, _)| date.as_str()), Some(SUNDAY));
        let out_of_window = shift_date(&window[0], -1).expect("date");
        let before = points
            .iter()
            .find(|(date, _)| date == &out_of_window)
            .map(|(_, hours)| *hours);
        assert_eq!(
            before,
            Some(Some(4.0)),
            "the evidence is the rolling window sum ending on that day"
        );

        // A strip day that is not in play (before `starts_on`) has no window to
        // sum: `null`, not 0h.
        let late = spec("r-late", "Learning", RuleKind::CategoryTotal, "2026-10-12");
        let late_revisions = [revision(
            "2026-10-12",
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let mut digest = digest_with_days("2026-10-12", SUNDAY);
        hours(&mut digest, SUNDAY, "learning", 5.0);
        let score = run(&late, &late_revisions, &catalog, &digest, SUNDAY);
        let points = evidence_points(&score);
        assert_eq!(
            points.first().map(|(_, hours)| *hours),
            Some(None),
            "before starts_on"
        );
        assert_eq!(
            points.last().map(|(_, hours)| *hours),
            Some(Some(5.0)),
            "the live day still sums"
        );
    }

    // ── 5. productive weekday mean ──────────────────────────────────────────

    #[test]
    fn weekday_mean_counts_empty_weekdays_and_ignores_weekends() {
        let catalog = catalog();
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(CategorySelector::AllProductive, &[0, 1, 2, 3, 4], 5, 7.0, None),
        )];
        // The 5-day window ending MONDAY: Thu 10-15 .. Mon 10-19.
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        for date in ["2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "work", 7.0);
        }
        // A Saturday does not count (and is `off` in the strip).
        hours(&mut digest, "2026-10-17", "work", 10.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, "2026-10-17"), "off");
        assert_eq!(cell(&score, "2026-10-18"), "off");
        assert_eq!(cell(&score, MONDAY), "pending");
        // Monday (today) has no rows: it is an included weekday worth 0h, which
        // pulls the mean down to 14 / 3.
        assert_eq!(score.actual_label, "4.67h mean");
        assert!(!score.on_track);
        assert_eq!(score.owed, "need +2.3h on the mean today 0.0 / 7.0h · 7.0h left");
        assert_eq!(score.target_label, "≥7h weekday mean");
        assert_eq!(score.condition, "≥7h weekday mean / 5d");
    }

    #[test]
    fn travel_days_are_excluded_and_do_not_extend_the_window() {
        let catalog = catalog();
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(
                CategorySelector::AllProductive,
                &[0, 1, 2, 3, 4],
                5,
                7.0,
                Some((&["trip", "travel"], 50.0)),
            ),
        )];

        // Waking = 2h work + 3h travel → 60% → excluded, and the mean is over
        // the remaining weekdays only: the window is not extended.
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        hours(&mut digest, MONDAY, "work", 2.0);
        hours(&mut digest, MONDAY, "travel", 3.0);
        for date in ["2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "work", 7.0);
        }
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "excluded");
        assert!(score.on_track);
        assert_eq!(score.actual_label, "7.00h mean");
        match &score.evidence {
            Evidence::CategoryDailyAvg { weekdays } => {
                assert_eq!(weekdays.len(), 10);
                let today = weekdays.last().expect("today included");
                assert_eq!(today.date, MONDAY);
                assert!(today.excluded);
                assert_eq!(today.hours, 2.0);
            }
            other => panic!("expected daily-avg evidence, got {other:?}"),
        }

        // An excluded day is not replaced by an older weekday: with Monday
        // excluded the mean is over the two 7h days (Thu + Fri).
        let mut digest = digest_with_days("2026-10-01", MONDAY);
        hours(&mut digest, MONDAY, "work", 2.0);
        hours(&mut digest, MONDAY, "travel", 3.0);
        for date in ["2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "work", 7.0);
        }
        // 2026-10-12 is a Monday outside the 5-day window: if the window
        // reached back for a replacement it would count.
        hours(&mut digest, "2026-10-12", "work", 1.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(score.actual_label, "7.00h mean", "{score:?}");

        // Exactly 50% is not "more than 50%".
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        hours(&mut digest, MONDAY, "work", 1.0);
        hours(&mut digest, MONDAY, "travel", 1.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "pending");
        assert_eq!(score.actual_label, "0.33h mean");

        // Waking 0 (unclassified only) is never excluded, and productive stays 0.
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        hours(&mut digest, MONDAY, "", 5.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "pending");
        assert_eq!(score.actual_label, "0.00h mean");
    }

    // ── sleep children are never waking (m8) ────────────────────────────────

    #[test]
    fn a_child_of_sleep_is_not_waking() {
        let catalog = CategoryCatalog::new(vec![
            Cat::new("sleep", None, false, "sleep", "Sleep"),
            Cat::new("nap", Some("sleep"), false, "nap", "Nap"),
            Cat::new("trip", None, false, "trip", "Trip"),
            Cat::new("work", None, true, "work", "Work"),
        ]);
        let sleep_ids = catalog.sleep_ids();
        assert!(sleep_ids.contains("nap"), "a child of sleep is sleep: {sleep_ids:?}");
        assert!(sleep_ids.contains("sleep"));
        assert!(
            sleep_ids.contains(crate::digest::SYNTHETIC_SLEEP_CATEGORY_ID),
            "the synthetic sentinel always counts"
        );

        // Waking is classified non-sleep time: a 5h nap must not make the 1h
        // trip look like 16% of the day.
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(
                CategorySelector::AllProductive,
                &[0, 1, 2, 3, 4],
                1,
                7.0,
                Some((&["trip"], 50.0)),
            ),
        )];
        let mut digest = digest_with_days(MONDAY, MONDAY);
        hours(&mut digest, MONDAY, "nap", 5.0);
        hours(&mut digest, MONDAY, "trip", 1.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "excluded", "{score:?}");
    }

    // ── 6. productive children ──────────────────────────────────────────────
    #[test]
    fn productive_expands_through_one_level_of_children() {
        let catalog = CategoryCatalog::new(vec![
            Cat::new("work", None, true, "work", "Work"),
            Cat::new("deep", Some("work"), false, "deep", "Deep Work"),
            Cat::new("trip", None, false, "trip", "Trip"),
            Cat::new("trip-food", Some("trip"), false, "food", "Food"),
            Cat::new("other", None, false, "other", "Other"),
            Cat::new("other-child", Some("other"), true, "other-child", "Other Child"),
        ]);
        let expanded = catalog.expand(&CategorySelector::AllProductive);
        assert!(expanded.contains("deep"), "a productive root covers its children");
        assert!(expanded.contains("work"));
        assert!(!expanded.contains("trip-food"));
        assert!(expanded.contains("other-child"), "a productive child counts on its own");
        assert!(!expanded.contains("other"));
        assert!(!expanded.contains("trip"));

        // The digest stores the leaf, so a child's seconds must score.
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(CategorySelector::AllProductive, &[0, 1, 2, 3, 4], 1, 7.0, None),
        )];
        let mut digest = DigestSlice::default();
        hours(&mut digest, MONDAY, "deep", 8.0);
        hours(&mut digest, MONDAY, "trip-food", 8.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(score.actual_label, "8.00h mean");
        assert!(score.on_track);
    }

    // ── 7. sleep duration ───────────────────────────────────────────────────

    #[test]
    fn sleep_duration_needs_min_nights_and_the_median() {
        let catalog = catalog();
        let spec = spec("r-sleep", "Sleep duration", RuleKind::SleepDuration, EARLY);
        let revisions = [revision(EARLY, &duration_params(60, 14, 6.5))];
        let window = days_ending(SUNDAY, 14);

        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.window_full);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "6.50h median");
        assert_eq!(score.target_label, "≥6.5h median");
        assert_eq!(score.condition, "median ≥6.5h / 60d");
        assert_eq!(score.owed, "on track");

        // Thirteen nights is a fail, not a pass.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in days_ending(SUNDAY, 13) {
            digest.add_night(&date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert!(score.owed.contains("not enough"), "{}", score.owed);
        assert_eq!(score.owed, NOT_ENOUGH_NIGHTS);
        assert_eq!(
            score.actual_label, "6.50h median",
            "the card still reports what is logged"
        );

        // An estimated night in the window does not count toward `min_nights`.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        digest.nights.push(Night {
            wake_date: "2026-10-04".to_string(),
            asleep_at: "2026-10-03T23:00:00Z".to_string(),
            bed_min_past_noon: 660,
            wake_min_past_midnight: 360,
            seconds: 7200,
            source: "est_none".to_string(),
        });
        let strict = [revision(EARLY, &duration_params(60, 15, 6.5))];
        let score = run(&spec, &strict, &catalog, &digest, SUNDAY);
        assert_eq!(
            score.owed, NOT_ENOUGH_NIGHTS,
            "15 nights are present but only 14 are paired"
        );
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "6.50h median");

        // An even count averages the two middle values.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for (index, date) in window.iter().enumerate() {
            let seconds = if index < 7 { 6 * 3600 } else { 7 * 3600 };
            digest.add_night(date, 630, 300, seconds);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "6.50h median");
        assert!(score.on_track);

        // Too short: the deficit sentence.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 6 * 3600);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "median is 0.50h short");

        // A night outside the window does not count, and the evidence is the
        // window's own nights — never an unbounded last-14 from all history.
        let mut digest = digest_with_days("2026-08-01", SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        // 20 older nights, all outside the 60-day window (which starts
        // 2026-08-20).
        for offset in 62..82 {
            let date = shift_date(SUNDAY, -offset).expect("date");
            digest.add_night(&date, 630, 300, 8 * 3600);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "6.50h median", "only the window's nights");
        match &score.evidence {
            Evidence::SleepDuration { nights } => {
                assert_eq!(nights.len(), EVIDENCE_NIGHTS, "capped at 14");
                assert_eq!(
                    nights.first().map(|night| night.wake_date.as_str()),
                    Some("2026-10-05"),
                    "the oldest night in the window"
                );
                assert_eq!(nights.last().map(|night| night.wake_date.as_str()), Some(SUNDAY));
            }
            other => panic!("expected sleep evidence, got {other:?}"),
        }
    }

    // ── 8. sleep timing ─────────────────────────────────────────────────────

    #[test]
    fn sleep_timing_medians_and_sample_stdev() {
        let catalog = catalog();
        let spec = spec("r-bedtime", "Bedtime", RuleKind::SleepTiming, EARLY);
        let window = days_ending(SUNDAY, 14);

        // All bedtimes 22:30 (630 past noon) pass `<= 630`.
        let revisions = [revision(
            EARLY,
            &timing_params(SleepMetric::BedtimeMedian, Comparator::Lte, 630, 60, 14),
        )];
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "22:30");
        assert_eq!(score.target_label, "≤22:30");
        assert_eq!(score.condition, "median bedtime ≤22:30");
        assert_eq!(score.owed, "on track");

        // A median of 631 (22:31) fails.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for (index, date) in window.iter().enumerate() {
            digest.add_night(date, if index < 7 { 631 } else { 631 }, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.actual_label, "22:31");
        assert_eq!(score.owed, "median bed 22:31 (≤22:30)");

        // Wake 05:00 passes `<= 300`; 05:01 fails.
        let revisions = [revision(
            EARLY,
            &timing_params(SleepMetric::WakeMedian, Comparator::Lte, 300, 60, 14),
        )];
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.condition, "median wake ≤05:00");

        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 301, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "median wake 05:01 (≤05:00)");

        // A spread of identical values is 0.
        let revisions = [revision(
            EARLY,
            &timing_params(SleepMetric::BedtimeStdev, Comparator::Lte, 45, 60, 14),
        )];
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "0m");
        assert_eq!(score.condition, "bedtime spread ≤45m");

        // Sample stdev (n - 1): values 0 and 10 → sqrt(50) ≈ 7.07.
        let revisions = [revision(
            EARLY,
            &timing_params(SleepMetric::BedtimeStdev, Comparator::Lte, 8, 60, 2),
        )];
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        digest.add_night(SUNDAY, 0, 300, 23_400);
        digest.add_night("2026-10-17", 10, 300, 23_400);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "7m");
        assert!(score.on_track, "sqrt(50) = 7.07 <= 8");

        let revisions = [revision(
            EARLY,
            &timing_params(SleepMetric::BedtimeStdev, Comparator::Lte, 7, 60, 2),
        )];
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track, "7.07 > 7");
        assert_eq!(score.owed, "bed stdev 7m (≤7m)");

        // One night is a fail when `min_nights` is 2, even with stdev 0.
        let mut digest = digest_with_days(&window_from(SUNDAY, 60), SUNDAY);
        digest.add_night(SUNDAY, 630, 300, 23_400);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, NOT_ENOUGH_NIGHTS);
    }

    // ── 9. revisions ────────────────────────────────────────────────────────

    #[test]
    fn each_day_is_scored_with_the_revision_active_on_it() {
        let catalog = catalog();
        let spec = spec("r-learning", "Learning", RuleKind::CategoryTotal, "2026-10-10");
        let revisions = [
            revision(
                "2026-10-01",
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 4.0),
            ),
            revision(
                "2026-10-11",
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 10.0),
            ),
        ];
        let mut digest = digest_with_days("2026-10-09", "2026-10-13");
        hours(&mut digest, "2026-10-10", "learning", 5.0);
        hours(&mut digest, "2026-10-11", "learning", 5.0);

        let score = run(&spec, &revisions, &catalog, &digest, "2026-10-13");
        // Day 10 (before the new revision) passes against the old 4h target.
        assert_eq!(cell(&score, "2026-10-10"), "pass");
        // Day 11 onward is scored against 10h, and 5h does not clear it.
        assert_eq!(cell(&score, "2026-10-11"), "fail");
        assert_eq!(cell(&score, "2026-10-12"), "fail");
        assert_eq!(cell(&score, "2026-10-13"), "pending");
        assert_eq!(cell(&score, "2026-09-30"), "off");
        assert!(!score.on_track);
        assert_eq!(score.target_label, "≥10h / 1d");
        assert_eq!(score.streak, 0);
        assert_eq!(score.best_streak, 1);
        assert_eq!(score.compliance, Some(1.0 / 3.0));
    }

    // ── 10. today is pending ────────────────────────────────────────────────

    #[test]
    fn today_is_pending_and_never_consumed_by_a_streak() {
        let catalog = catalog();
        let spec = spec("r-learning", "Learning", RuleKind::CategoryTotal, SUNDAY);
        let revisions = [revision(
            SUNDAY,
            &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
        )];
        let mut digest = DigestSlice::default();
        hours(&mut digest, SUNDAY, "learning", 2.0);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(cell(&score, SUNDAY), "pending");
        assert_eq!(score.streak, 0);
        assert_eq!(score.best_streak, 0);
        assert_eq!(score.compliance, None);
    }
    // ── 11. finished challenges ─────────────────────────────────────────────

    #[test]
    fn a_finished_challenge_scores_at_its_ends_on() {
        let catalog = catalog();
        let mut finished = spec("r-done", "Learning", RuleKind::CategoryTotal, EARLY);
        finished.ends_on = Some("2026-10-17".to_string());
        let active = spec("r-open", "Fitness", RuleKind::CategoryDays, EARLY);

        let mut digest = DigestSlice::default();
        hours(&mut digest, "2026-10-17", "learning", 2.0);
        hours(&mut digest, SUNDAY, "fitness", 1.0);

        let revisions = BTreeMap::from([
            (
                "r-done".to_string(),
                vec![revision(
                    EARLY,
                    &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
                )],
            ),
            (
                "r-open".to_string(),
                vec![revision(EARLY, &days_params(&["fitness"], 1, 60, 1))],
            ),
        ]);
        let board = score_board(BoardInput {
            as_of: SUNDAY,
            time_zone: "Asia/Kolkata",
            rules: vec![finished.clone(), active.clone()],
            revisions,
            pauses: Vec::new(),
            catalog: &catalog,
            digest: &digest,
            flags: DigestFlags::default(),
        })
        .expect("board scores");

        assert_eq!(board.as_of, SUNDAY);
        assert_eq!(board.time_zone, "Asia/Kolkata");
        assert_eq!(board.active_rules, 1);
        assert_eq!(board.on_track, 1);
        assert_eq!(board.rules.len(), 1);
        assert_eq!(board.rules[0].id, "r-open");
        assert_eq!(board.rules[0].phase, "active");

        // The finished rule is out of the headline but still has a final score:
        // its cursor is `ends_on`, where the 2h live, while `today` is the real
        // civil day — so the last day is judged rather than left pending.
        let score = score_rule(
            &finished,
            &[revision(
                EARLY,
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
            )],
            &[],
            &catalog,
            &digest,
            "2026-10-17",
            SUNDAY,
        )
        .expect("scores");
        assert!(score.on_track);
        assert_eq!(score.phase, "finished", "ends_on is behind the real today");
        assert_eq!(cell(&score, "2026-10-17"), "pass", "final day judged");
        assert!(score.streak >= 1, "the final day counts: {}", score.streak);
        assert_eq!(rule_phase(&finished, SUNDAY), RulePhase::Finished);
        // Scored with its cursor at the real today instead, the same rule fails
        // (nothing on the day) and leaves it pending.
        let naive = score_rule(
            &finished,
            &[revision(
                EARLY,
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
            )],
            &[],
            &catalog,
            &digest,
            SUNDAY,
            SUNDAY,
        )
        .expect("scores");
        assert!(!naive.on_track);
    }

    // ── 12. unclassified seconds ────────────────────────────────────────────

    #[test]
    fn unclassified_seconds_are_neither_productive_nor_waking() {
        let catalog = catalog();
        let total = spec("r-learning", "Learning", RuleKind::CategoryTotal, EARLY);
        let revisions = [revision(
            EARLY,
            &total_params(CategorySelector::AllProductive, 1, Comparator::Gte, 1.0),
        )];
        let mut digest = DigestSlice::default();
        hours(&mut digest, SUNDAY, "work", 1.0);
        hours(&mut digest, SUNDAY, "", 5.0);
        let score = run(&total, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "1.0h");

        // `''` is not waking either: 1h trip against 1h waking is 100%, so the
        // day is travel-excluded. Counting `''` would make it 50% — not excluded.
        let average = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(
                CategorySelector::AllProductive,
                &[0, 1, 2, 3, 4],
                1,
                1.0,
                Some((&["trip"], 50.0)),
            ),
        )];
        let mut digest = DigestSlice::default();
        hours(&mut digest, MONDAY, "trip", 1.0);
        hours(&mut digest, MONDAY, "", 1.0);
        let score = run(&average, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "excluded");
    }

    // ── 13. today's progress in the owed label ──────────────────────────────

    #[test]
    fn daily_avg_owed_shows_todays_progress() {
        let catalog = catalog();
        let spec = spec("r-productive", "Productive weekdays", RuleKind::CategoryDailyAvg, EARLY);
        let revisions = [revision(
            EARLY,
            &avg_params(CategorySelector::AllProductive, &[0, 1, 2, 3, 4], 5, 7.0, None),
        )];
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        digest.add(MONDAY, "work", 15_120); // 4.2h
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        // Three included weekdays in the 5-day window (Thu, Fri, Mon).
        assert_eq!(score.actual_label, "1.40h mean");
        assert!(score.owed.contains("today 4.2 / 7.0h"), "{}", score.owed);
        assert!(score.owed.contains("2.8h left"), "{}", score.owed);
        assert_eq!(score.owed, "need +5.6h on the mean today 4.2 / 7.0h · 2.8h left");

        // m6: the fragment rides along even when the mean already passes, and
        // only the "left" clause is dropped once today clears the target.
        let mut digest = digest_with_days(&window_from(MONDAY, 5), MONDAY);
        for date in ["2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "work", 9.0);
        }
        digest.add(MONDAY, "work", 15_120); // 4.2h
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert!(score.on_track, "7.40h mean");
        assert_eq!(score.owed, "on track today 4.2 / 7.0h · 2.8h left");

        digest.add(MONDAY, "work", 3 * 3600 + 2880); // 8.0h total
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert!(score.on_track);
        assert_eq!(score.owed, "on track today 8.0 / 7.0h", "no left clause");
    }

    // ── board: headline, streaks, compliance ────────────────────────────────

    #[test]
    fn board_headline_streaks_and_compliance() {
        let catalog = catalog();
        let learning_revisions = vec![revision(
            EARLY,
            &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
        )];
        let fitness_revisions = vec![revision(EARLY, &days_params(&["fitness"], 1, 60, 1))];
        let mut a = spec("r-a", "Learning", RuleKind::CategoryTotal, "2026-10-13");
        a.sort_order = 0;
        let mut b = spec("r-b", "Fitness", RuleKind::CategoryDays, "2026-10-13");
        b.sort_order = 1;
        let mut c = spec("r-c", "Sleep duration", RuleKind::SleepDuration, "2026-10-13");
        c.sort_order = 2;
        c.status = RuleStatus::Paused;
        c.paused_on = Some("2026-10-16".to_string());

        let mut digest = DigestSlice::default();
        for date in ["2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "learning", 2.0);
            hours(&mut digest, date, "fitness", 2.0);
        }
        // B's one-day window is empty on 2026-10-17, so B fails that day.
        hours(&mut digest, "2026-10-17", "learning", 2.0);
        hours(&mut digest, SUNDAY, "learning", 2.0);
        hours(&mut digest, SUNDAY, "fitness", 2.0);
        for date in ["2026-10-13", "2026-10-14", "2026-10-15"] {
            digest.add_night(date, 630, 300, 8 * 3600);
        }

        let revisions = BTreeMap::from([
            ("r-a".to_string(), learning_revisions),
            ("r-b".to_string(), fitness_revisions),
            ("r-c".to_string(), vec![revision(EARLY, &duration_params(1, 1, 1.0))]),
        ]);
        let board = score_board(BoardInput {
            as_of: SUNDAY,
            time_zone: "UTC",
            rules: vec![a, b, c],
            revisions,
            pauses: Vec::new(),
            catalog: &catalog,
            digest: &digest,
            flags: DigestFlags {
                reclassifying: true,
                backfilling: false,
                dirty_days: 3,
            },
        })
        .expect("board scores");

        assert_eq!(board.rules.len(), 3, "paused rules stay on the board");
        assert_eq!(board.active_rules, 2, "the paused rule is not in the headline");
        assert_eq!(board.paused_rules, 1, "the paused chip's count");
        assert_eq!(board.on_track, 2);
        assert!(board.reclassifying);
        assert!(!board.backfilling);
        assert_eq!(board.dirty_days, 3);

        let b_score = board.rules.iter().find(|rule| rule.id == "r-b").expect("b");
        assert_eq!(b_score.compliance, Some(0.8), "4 of 5 completed days passed");
        assert_eq!(b_score.streak, 0);
        assert_eq!(b_score.best_streak, 4);
        assert_eq!(cell(b_score, SUNDAY), "pending");

        // A+B pass through 10-16, then B fails on 10-17 and breaks the run.
        assert_eq!(board.all_pass_streak, 0);
        assert_eq!(board.best_all_pass_streak, 4);
    }

    // ── JSON contract ───────────────────────────────────────────────────────

    #[test]
    fn scoreboard_json_matches_the_documented_shape() {
        let catalog = catalog();
        let spec = spec("r-learning", "Learning", RuleKind::CategoryTotal, EARLY);
        let revisions = BTreeMap::from([(
            "r-learning".to_string(),
            vec![revision(
                EARLY,
                &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
            )],
        )]);
        // Every day back to September is aggregated, so the 7-day windows
        // behind the whole strip are full.
        let mut digest = digest_with_days("2026-09-01", SUNDAY);
        hours(&mut digest, SUNDAY, "learning", 4.5);
        let board = score_board(BoardInput {
            as_of: SUNDAY,
            time_zone: "Asia/Kolkata",
            rules: vec![spec],
            revisions,
            pauses: Vec::new(),
            catalog: &catalog,
            digest: &digest,
            flags: DigestFlags::default(),
        })
        .expect("board scores");
        let value = serde_json::to_value(&board).expect("serializes");
        assert_eq!(
            value.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec![
                "active_rules",
                "all_pass_streak",
                "as_of",
                "backfilling",
                "best_all_pass_streak",
                "dirty_days",
                "on_track",
                "paused_rules",
                "reclassifying",
                "rules",
                "time_zone",
            ]
        );
        assert_eq!(value["as_of"], SUNDAY);
        assert_eq!(value["time_zone"], "Asia/Kolkata");
        assert_eq!(value["on_track"], 1);
        assert_eq!(value["active_rules"], 1);
        assert_eq!(value["paused_rules"], 0);
        assert_eq!(value["all_pass_streak"], 0);
        assert_eq!(value["dirty_days"], 0);
        assert!(
            value.get("warnings").is_none(),
            "operator notes never reach the client"
        );

        let rule = &value["rules"][0];
        assert_eq!(
            rule.as_object().expect("object").keys().collect::<Vec<_>>(),
            vec![
                "actual_label",
                "best_streak",
                "compliance",
                "condition",
                "ends_on",
                "evidence",
                "icon",
                "id",
                "kind",
                "name",
                "on_track",
                "owed",
                "params",
                "paused_on",
                "phase",
                "sort_order",
                "starts_on",
                "streak",
                "strip",
                "target_label",
                "window_full",
            ]
        );
        assert_eq!(rule["kind"], "category_total");
        assert_eq!(rule["phase"], "active");
        assert_eq!(rule["window_full"], true);
        assert_eq!(rule["ends_on"], serde_json::Value::Null);
        assert_eq!(rule["actual_label"], "4.5h");
        assert_eq!(rule["condition"], "≥4h / 7d");
        // The edit form hydrates from the revision active on `score_as_of`.
        assert_eq!(rule["params"]["target_hours"], 4.0);
        assert_eq!(rule["params"]["window_days"], 7);
        assert_eq!(rule["params"]["comparator"], "gte");
        assert_eq!(
            rule["params"]["categories"],
            serde_json::json!({"ids": ["learning"], "include_children": true})
        );
        assert!(rule["compliance"].is_number());
        assert_eq!(
            rule["strip"][13],
            serde_json::json!({"date": SUNDAY, "state": "pending"})
        );
        assert_eq!(rule["evidence"]["type"], "category_total");
        assert_eq!(rule["evidence"]["points"][13]["date"], SUNDAY);
        assert_eq!(rule["evidence"]["points"][13]["hours"], 4.5);

        // `compliance` is null, never absent, when nothing completed.
        let empty = spec_json(&catalog);
        assert_eq!(empty["compliance"], serde_json::Value::Null);
        // A window the backfill has not reached is not a failure either.
        assert_eq!(empty["window_full"], false);
        assert_eq!(empty["on_track"], false);
        assert_eq!(empty["owed"], HISTORY_BUILDING);
        assert_eq!(empty["streak"], 0);
    }

    /// A fresh rule with no history: `compliance` must serialize as `null`, and
    /// the card says the history is still building.
    fn spec_json(catalog: &CategoryCatalog) -> serde_json::Value {
        let spec = spec("r-new", "Learning", RuleKind::CategoryTotal, SUNDAY);
        let revisions = [revision(
            SUNDAY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let score = run(&spec, &revisions, catalog, &DigestSlice::default(), SUNDAY);
        serde_json::to_value(&score).expect("serializes")
    }

    // ── the load floor ──────────────────────────────────────────────────────

    /// The digest load uses the **same** start-date expression as the scorer.
    #[test]
    fn the_load_floor_is_the_scorers_window_start() {
        let never = |_: &str| false;
        // A 60-day window ending 2026-10-18 starts on 2026-08-20.
        assert_eq!(
            digest_load_floor("2026-06-01", 60, SUNDAY, &never),
            window_start("2026-06-01", 60, &never).expect("a window"),
            "the floor is the anchor's own window start"
        );
        assert_eq!(digest_load_floor("2026-06-01", 60, SUNDAY, &never), "2026-04-03");

        // A rule created today still needs the strip's own windows: the anchor
        // is the strip start (2026-10-05), and a 1-day window starts there.
        assert_eq!(digest_load_floor(SUNDAY, 1, SUNDAY, &never), "2026-10-05");
        assert_eq!(
            digest_load_floor(SUNDAY, 1, SUNDAY, &never),
            window_start("2026-10-05", 1, &never).expect("a window")
        );

        // A very old rule is clamped to the streak floor: as_of - 399 days,
        // which is 2025-09-14.
        assert_eq!(digest_load_floor("2000-01-01", 366, SUNDAY, &never), "2024-09-14");
        assert_eq!(
            digest_load_floor("2000-01-01", 366, SUNDAY, &never),
            window_start("2025-09-14", 366, &never).expect("a window")
        );

        // A pause widens the floor exactly as it widens the window.
        let paused = |date: &str| date == "2026-05-05";
        assert_eq!(
            digest_load_floor("2026-06-01", 60, SUNDAY, &paused),
            "2026-04-02",
            "one paused day inside the anchor's window"
        );
        assert_eq!(
            digest_load_floor("2026-06-01", 60, SUNDAY, &paused),
            window_start("2026-06-01", 60, &paused).expect("a window")
        );
    }

    #[test]
    fn max_window_days_fits_inside_the_lookback() {
        // A stored `window_days` is rejected above `MAX_WINDOW_DAYS`, and that
        // bound is tighter than the scorer's lookback cap — so every window a
        // rule can ask for is one the lookback can reach.
        assert!(MAX_WINDOW_DAYS <= MAX_LOOKBACK_DAYS);
        // The cap itself is not full: the extension cannot reach past it.
        let never = |_: &str| false;
        assert!(window_start(SUNDAY, MAX_LOOKBACK_DAYS, &never).is_ok());
        let always = |_: &str| true;
        assert_eq!(
            window_start(SUNDAY, 60, &always),
            Err(NotFull::Lookback),
            "every day paused: the extension runs past the cap"
        );
    }

    // ── M3: a hole is not a failure ─────────────────────────────────────────

    #[test]
    fn a_window_with_a_hole_is_not_scored_as_a_failure() {
        let catalog = catalog();
        let rule = spec("r-learning", "Learning", RuleKind::CategoryTotal, EARLY);
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let window = days_ending(SUNDAY, 7);
        // Six of the seven days are aggregated; the hole is 2026-10-14.
        let mut digest = digest_with_days(&window[0], SUNDAY);
        digest.aggregated.remove("2026-10-14");
        hours(&mut digest, SUNDAY, "learning", 5.0);

        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.window_full);
        assert!(!score.on_track);
        assert_eq!(score.owed, HISTORY_BUILDING);
        assert_eq!(score.streak, 0);
        assert_eq!(score.best_streak, 0);
        assert_eq!(score.compliance, None, "no history is consumed");
        assert_eq!(
            cell(&score, SUNDAY),
            "pending",
            "today stays pending anyway"
        );
        assert_eq!(
            cell(&score, "2026-10-17"),
            "pending",
            "an in-play day whose window has a hole is pending, not a fail"
        );
        // The hole is only a hole: the day itself still shows what is logged.
        assert_eq!(score.actual_label, "5.0h");

        // Fill the hole and the very same days get verdicts.
        let mut digest = digest_with_days(&window[0], SUNDAY);
        hours(&mut digest, SUNDAY, "learning", 5.0);
        let score = run(&rule, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.window_full);
        assert!(score.on_track);
        assert_eq!(score.owed, "on track");

        // A sleep rule does not fail for "not enough nights" while the window
        // is still building.
        let sleep = spec("r-sleep", "Sleep duration", RuleKind::SleepDuration, EARLY);
        let revisions = [revision(EARLY, &duration_params(7, 3, 6.5))];
        let mut digest = DigestSlice::default();
        digest.add_night(SUNDAY, 630, 300, 23_400);
        let score = run(&sleep, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.window_full);
        assert_eq!(score.owed, HISTORY_BUILDING);
        assert!(!score.on_track);

        // With the window full, the same one night is the usual failure.
        let mut digest = digest_with_days(&window[0], SUNDAY);
        digest.add_night(SUNDAY, 630, 300, 23_400);
        let score = run(&sleep, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.window_full);
        assert_eq!(score.owed, NOT_ENOUGH_NIGHTS);
    }

    // ── M4: a paused challenge still finishes ───────────────────────────────

    #[test]
    fn a_paused_challenge_past_its_end_is_finished() {
        let catalog = catalog();
        let mut paused = spec("r-fitness", "Fitness", RuleKind::CategoryDays, EARLY);
        paused.status = RuleStatus::Paused;
        paused.paused_on = Some("2026-10-01".to_string());
        paused.ends_on = Some("2026-10-10".to_string());

        assert_eq!(
            rule_phase(&paused, SUNDAY),
            RulePhase::Finished,
            "the challenge ended, even while it is paused"
        );
        assert!(!rule_phase(&paused, SUNDAY).is_on_board());

        // Still paused before its end.
        assert_eq!(rule_phase(&paused, "2026-10-05"), RulePhase::Paused);
        // The end day itself is in play: `ends_on == as_of` is not finished.
        assert_eq!(rule_phase(&paused, "2026-10-10"), RulePhase::Paused);
        // Archived wins over finished.
        paused.status = RuleStatus::Archived;
        assert_eq!(rule_phase(&paused, SUNDAY), RulePhase::Archived);

        // The archive picks it up.
        let mut open = spec("r-open", "Learning", RuleKind::CategoryTotal, EARLY);
        open.status = RuleStatus::Paused;
        open.paused_on = Some("2026-10-01".to_string());
        open.ends_on = Some("2026-10-10".to_string());
        let revisions = BTreeMap::from([(
            "r-open".to_string(),
            vec![revision(
                EARLY,
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
            )],
        )]);
        let mut digest = digest_with_days(EARLY, SUNDAY);
        hours(&mut digest, "2026-10-10", "learning", 2.0);
        let board = score_board(BoardInput {
            as_of: SUNDAY,
            time_zone: "UTC",
            rules: vec![open.clone()],
            revisions,
            pauses: Vec::new(),
            catalog: &catalog,
            digest: &digest,
            flags: DigestFlags::default(),
        })
        .expect("board scores");
        assert!(board.rules.is_empty(), "a finished paused rule leaves the board");
        assert_eq!(board.paused_rules, 0);
        // Its final day is still judged at its own cursor.
        let score = score_rule(
            &open,
            &[revision(
                EARLY,
                &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
            )],
            &[],
            &catalog,
            &digest,
            "2026-10-10",
            SUNDAY,
        )
        .expect("scores");
        assert_eq!(score.phase, "finished");
        assert_eq!(
            cell(&score, "2026-10-10"),
            "paused",
            "the open pause covers the final day, so nothing is scored there"
        );
        assert!(!score.on_track, "an empty (all-paused) window passes nothing");
        assert_eq!(score.actual_label, "0.0h");
    }

    // ── M6: archiving never rewrites the streak ─────────────────────────────

    #[test]
    fn archiving_a_rule_does_not_rewrite_the_all_pass_streak() {
        let catalog = catalog();
        let learning = spec("r-learning", "Learning", RuleKind::CategoryTotal, "2026-10-13");
        let fitness = spec("r-fitness", "Fitness", RuleKind::CategoryDays, "2026-10-13");
        let revisions = BTreeMap::from([
            (
                "r-learning".to_string(),
                vec![revision(
                    "2026-10-13",
                    &total_params(ids(&["learning"]), 1, Comparator::Gte, 1.0),
                )],
            ),
            (
                "r-fitness".to_string(),
                vec![revision("2026-10-13", &days_params(&["fitness"], 1, 60, 1))],
            ),
        ]);
        let mut digest = digest_with_days("2026-10-13", SUNDAY);
        for date in ["2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16", "2026-10-17"] {
            hours(&mut digest, date, "learning", 2.0);
            hours(&mut digest, date, "fitness", 2.0);
        }

        let board = |rules: Vec<RuleSpec>| {
            score_board(BoardInput {
                as_of: SUNDAY,
                time_zone: "UTC",
                rules,
                revisions: revisions.clone(),
                pauses: Vec::new(),
                catalog: &catalog,
                digest: &digest,
                flags: DigestFlags::default(),
            })
            .expect("board scores")
        };
        let both = board(vec![learning.clone(), fitness.clone()]);
        assert_eq!(both.best_all_pass_streak, 5, "10-13 .. 10-17 pass together");

        // Archive one of the two: it leaves the board but stays in the walk,
        // because it *was* in play on those days.
        let mut archived = fitness.clone();
        archived.status = RuleStatus::Archived;
        let after = board(vec![learning.clone(), archived.clone()]);
        assert!(after.rules.iter().all(|rule| rule.id == "r-learning"));
        assert_eq!(
            after.best_all_pass_streak, 5,
            "archiving must not rewrite the streak"
        );

        // A rule that fails alone still breaks the run.
        let revisions = BTreeMap::from([
            (
                "r-learning".to_string(),
                vec![revision(
                    "2026-10-13",
                    &total_params(ids(&["learning"]), 1, Comparator::Gte, 5.0),
                )],
            ),
            (
                "r-fitness".to_string(),
                vec![revision("2026-10-13", &days_params(&["fitness"], 1, 60, 1))],
            ),
        ]);
        let board = score_board(BoardInput {
            as_of: SUNDAY,
            time_zone: "UTC",
            rules: vec![learning, archived],
            revisions,
            pauses: Vec::new(),
            catalog: &catalog,
            digest: &digest,
            flags: DigestFlags::default(),
        })
        .expect("board scores");
        assert_eq!(
            board.best_all_pass_streak, 0,
            "the live rule never clears its 5h target"
        );
    }
}


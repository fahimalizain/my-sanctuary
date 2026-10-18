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
//! A verdict for date `D` walks backward from `D` collecting **counted** days
//! until `window_days` are gathered or the lookback cap is hit. Paused days are
//! never counted and never break a streak; a daily-average travel-excluded
//! weekday is skipped the same way. Days before `starts_on` are out of play
//! (strip `off`, no streak) but their digest rows still feed the window of the
//! first in-play days. Each day is scored with the revision active on it.
//! `on_track` is the window ending `score_as_of`; the real `today` is always
//! `pending` in the strip and never consumed by a streak. A finished challenge
//! is scored at its `ends_on`, so that last day is judged — and counts — while
//! its phase is still derived from `today`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Datelike;
use serde::Serialize;

use crate::digest::shift_date;

use super::params::{
    format_bedtime, format_wake, CategoryCatalog, CategoryDailyAvgParams, Comparator, RuleKind,
    RuleParams, RulePhase, RuleStatus, SleepMetric, MAX_WINDOW_DAYS,
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
}

impl DigestSlice {
    /// Records one category's seconds on one day (test convenience).
    pub fn add(&mut self, date: &str, category_id: &str, seconds: i64) {
        *self
            .category_seconds
            .entry(date.to_string())
            .or_default()
            .entry(category_id.to_string())
            .or_insert(0) += seconds;
    }

    /// Records one paired night (test convenience).
    pub fn add_night(&mut self, wake_date: &str, bed_min_past_noon: i64, wake_min_past_midnight: i64, seconds: i64) {
        self.nights.push(Night {
            wake_date: wake_date.to_string(),
            asleep_at: format!("{wake_date}T00:00:00Z"),
            bed_min_past_noon,
            wake_min_past_midnight,
            seconds,
            source: "paired".to_string(),
        });
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
    pub active_rules: usize,
    pub all_pass_streak: i64,
    pub best_all_pass_streak: i64,
    pub reclassifying: bool,
    pub backfilling: bool,
    pub dirty_days: i64,
    /// Rules with phase `active` or `paused`, by `sort_order` then name.
    pub rules: Vec<RuleScore>,
}

/// The `GET /api/rules/archive` body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArchiveResponse {
    pub rules: Vec<RuleScore>,
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

    let mut evaluated: Vec<(RuleSpec, RuleScore, BTreeMap<String, DayState>)> = Vec::new();
    for spec in &specs {
        let empty: Vec<Revision> = Vec::new();
        let revisions = revisions.get(&spec.id).unwrap_or(&empty);
        // The board is live: its cursor is today, and today stays pending.
        let evaluation =
            Evaluator::new(spec, revisions, &pauses, catalog, digest, as_of, as_of)?.evaluate()?;
        evaluated.push((spec.clone(), evaluation.score, evaluation.states));
    }

    let active: Vec<&RuleScore> = evaluated
        .iter()
        .map(|(_, score, _)| score)
        .filter(|score| score.phase == RulePhase::Active.as_str())
        .collect();
    let on_track = active.iter().filter(|score| score.on_track).count();
    let active_rules = active.len();

    // The all-pass walk considers the board (active + paused): a day counts
    // when at least one such rule has a real pass/fail on it, and the run
    // continues while every one of them passed.
    let board: Vec<&(RuleSpec, RuleScore, BTreeMap<String, DayState>)> = evaluated
        .iter()
        .filter(|(_, score, _)| {
            score.phase == RulePhase::Active.as_str() || score.phase == RulePhase::Paused.as_str()
        })
        .collect();
    let (all_pass_streak, best_all_pass_streak) = all_pass_walk(&board, as_of);

    let rules = evaluated
        .into_iter()
        .map(|(_, score, _)| score)
        .filter(|score| {
            score.phase == RulePhase::Active.as_str() || score.phase == RulePhase::Paused.as_str()
        })
        .collect();

    Ok(Scoreboard {
        as_of: as_of.to_string(),
        time_zone: time_zone.to_string(),
        on_track,
        active_rules,
        all_pass_streak,
        best_all_pass_streak,
        reclassifying: flags.reclassifying,
        backfilling: flags.backfilling,
        dirty_days: flags.dirty_days,
        rules,
    })
}

/// The all-pass runs over `board` (active + paused rules), from the earliest
/// in-play day through yesterday.
fn all_pass_walk(
    board: &[&(RuleSpec, RuleScore, BTreeMap<String, DayState>)],
    as_of: &str,
) -> (i64, i64) {
    let floor = shift_date(as_of, -(MAX_LOOKBACK_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    let Some(yesterday) = shift_date(as_of, -1) else {
        return (0, 0);
    };
    let start = board
        .iter()
        .map(|(spec, _, _)| {
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
        for (_, _, states) in board {
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
/// `archived` wins over everything; then `paused`; then `finished` when the
/// challenge's inclusive `ends_on` is in the past; else `active`.
pub fn rule_phase(spec: &RuleSpec, as_of: &str) -> RulePhase {
    match spec.status {
        RuleStatus::Archived => RulePhase::Archived,
        RuleStatus::Paused => RulePhase::Paused,
        RuleStatus::Active => match spec.ends_on.as_deref() {
            Some(ends_on) if ends_on < as_of => RulePhase::Finished,
            _ => RulePhase::Active,
        },
    }
}

/// The earliest digest date a scoreboard can read.
///
/// Every scored day `D` looks back at most `min(400, window_days * 3)` calendar
/// days, and the streak history is clamped to `as_of - 399`, so the floor is
/// the earlier of the earliest `starts_on` and the strip start, minus that
/// lookback.
pub fn digest_load_floor(min_starts_on: &str, max_window_days: i64, as_of: &str) -> String {
    let cap = (max_window_days.clamp(1, MAX_WINDOW_DAYS) * 3).min(MAX_LOOKBACK_DAYS);
    let strip_start = shift_date(as_of, -(STRIP_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    let history_floor =
        shift_date(as_of, -(MAX_LOOKBACK_DAYS - 1)).unwrap_or_else(|| as_of.to_string());
    // The earliest day any window is evaluated for: never before the streak
    // floor, never after the strip start.
    let anchor = min_starts_on
        .max(history_floor.as_str())
        .min(strip_start.as_str());
    shift_date(anchor, -(cap - 1)).unwrap_or_else(|| as_of.to_string())
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
    pass: bool,
    /// Counted days, oldest first.
    counted: Vec<String>,
    /// Selector seconds over the counted days.
    total_seconds: i64,
    /// Days clearing `min_minutes`.
    hits: i64,
    /// Paired nights on counted days, oldest first.
    nights: Vec<Night>,
    /// Median of the metric (hours for duration, minutes for timing).
    median: f64,
    /// Sample stdev in minutes.
    stdev: f64,
}

struct Evaluator<'a> {
    spec: &'a RuleSpec,
    kind: RuleKind,
    revisions: Vec<ParsedRevision>,
    global_pauses: Vec<(String, String)>,
    rule_pauses: Vec<(String, String)>,
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
        catalog: &CategoryCatalog,
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

    /// The counted days of the window ending at `end`, oldest first.
    ///
    /// Walking backward from `end`: a day counts when it is not paused and
    /// passes the kind's own filter (for `category_daily_avg`, a selected
    /// weekday that is not travel-excluded). Paused and skipped days extend the
    /// window. The walk examines at most `min(window_days * 3, 400)` days.
    fn counted_days(&self, revision: &ParsedRevision, end: &str) -> Vec<String> {
        let window = revision.params.window_days().clamp(1, MAX_WINDOW_DAYS);
        let cap = (window * 3).min(MAX_LOOKBACK_DAYS) as usize;
        let mut out: Vec<String> = Vec::new();
        let mut cursor = Some(end.to_string());
        let mut examined = 0_usize;
        while let Some(date) = cursor {
            if examined >= cap {
                break;
            }
            examined += 1;
            if !self.is_paused(&date) && self.counts(revision, &date) {
                out.push(date.clone());
                if out.len() as i64 >= window {
                    break;
                }
            }
            cursor = shift_date(&date, -1);
        }
        out.reverse();
        out
    }

    /// Whether `date` would count toward this revision's window (ignoring
    /// pauses).
    fn counts(&self, revision: &ParsedRevision, date: &str) -> bool {
        match &revision.params {
            RuleParams::CategoryDailyAvg(params) => {
                weekday(date).is_some_and(|day| params.weekdays.contains(&day))
                    && !self.travel_excluded(revision, date)
            }
            _ => true,
        }
    }

    /// Evaluates the window ending at `end` with `revision`'s params.
    fn outcome(&self, revision: &ParsedRevision, end: &str) -> WindowOutcome {
        let counted = self.counted_days(revision, end);
        let mut outcome = WindowOutcome {
            counted: counted.clone(),
            ..WindowOutcome::default()
        };
        match &revision.params {
            RuleParams::CategoryTotal(params) => {
                outcome.total_seconds = counted
                    .iter()
                    .map(|date| self.selector_seconds(&revision.ids, date))
                    .sum();
                let hours = outcome.total_seconds as f64 / HOUR_SECS;
                outcome.pass = params.comparator.passes(hours, params.target_hours);
            }
            RuleParams::CategoryDays(params) => {
                let threshold = params.min_minutes * 60;
                outcome.hits = counted
                    .iter()
                    .filter(|date| self.selector_seconds(&revision.ids, date) >= threshold)
                    .count() as i64;
                outcome.pass = outcome.hits >= params.min_days;
            }
            RuleParams::CategoryDailyAvg(params) => {
                outcome.total_seconds = counted
                    .iter()
                    .map(|date| self.selector_seconds(&revision.ids, date))
                    .sum();
                let mean = if counted.is_empty() {
                    0.0
                } else {
                    outcome.total_seconds as f64 / HOUR_SECS / counted.len() as f64
                };
                outcome.median = mean;
                outcome.pass = mean >= params.target_hours;
            }
            RuleParams::SleepDuration(params) => {
                outcome.nights = self.nights_on(&counted);
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
                outcome.nights = self.nights_on(&counted);
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
        if self.outcome(revision, date).pass {
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

        let mut streak = 0_i64;
        let mut best_streak = 0_i64;
        let mut passes = 0_i64;
        let mut counted_completed = 0_i64;
        for (date, state) in &states {
            // A completed day: at or before the card's cursor and strictly
            // before the real today. Live, that is "through yesterday"; a
            // finished challenge scored at its `ends_on` counts that day too.
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
            on_track: outcome.pass,
            actual_label: self.actual_label(revision, &outcome),
            target_label: revision.params.target_label(),
            owed: self.owed(revision, &outcome),
            streak,
            best_streak,
            compliance: if counted_completed == 0 {
                None
            } else {
                Some(passes as f64 / counted_completed as f64)
            },
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
    fn owed(&self, revision: &ParsedRevision, outcome: &WindowOutcome) -> String {
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
                    self.spec.name
                )
            }
            RuleParams::CategoryDailyAvg(params) => {
                if outcome.pass {
                    return "on track".to_string();
                }
                let mut owed = format!("need +{:.1}h on the mean", params.target_hours - outcome.median);
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
    /// frozen final day must never be described as "today".
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

    fn evidence(&self, revision: &ParsedRevision, outcome: &WindowOutcome) -> Evidence {
        match &revision.params {
            RuleParams::CategoryTotal(_) => Evidence::CategoryTotal {
                points: self
                    .strip_dates()
                    .into_iter()
                    .map(|date| {
                        if self.is_paused(&date) {
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
                let threshold = params.min_minutes * 60;
                Evidence::CategoryDays {
                    days: outcome
                        .counted
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
                nights: self.evidence_nights(),
            },
            RuleParams::SleepTiming(_) => Evidence::SleepTiming {
                nights: self.evidence_nights(),
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

    /// The newest `EVIDENCE_NIGHTS` paired nights with `wake_date <=
    /// score_as_of`.
    fn evidence_nights(&self) -> Vec<EvidenceNight> {
        let mut nights: Vec<&Night> = self
            .digest
            .nights
            .iter()
            .filter(|night| {
                night.source == "paired" && night.wake_date.as_str() <= self.score_as_of
            })
            .collect();
        nights.sort_by(|left, right| {
            (&left.wake_date, &left.asleep_at).cmp(&(&right.wake_date, &right.asleep_at))
        });
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

    fn evidence_days(score: &RuleScore) -> Vec<String> {
        match &score.evidence {
            Evidence::CategoryDays { days } => days.iter().map(|day| day.date.clone()).collect(),
            other => panic!("expected category_days evidence, got {other:?}"),
        }
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
        let spec = spec("r-fitness", "Fitness", RuleKind::CategoryDays, EARLY);
        let revisions = [revision(EARLY, &days_params(&["fitness"], 60, 60, 12))];
        let window = days_ending(SUNDAY, 60);

        let mut digest = DigestSlice::default();
        for date in window.iter().take(12) {
            hours(&mut digest, date, "fitness", 1.0);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.owed, "on track");
        assert_eq!(score.actual_label, "12 days");
        assert_eq!(score.target_label, "12 days / 60d");
        assert_eq!(score.condition, "≥12 days of ≥1h / 60d");

        // One second short of the threshold is not a hit.
        let mut digest = DigestSlice::default();
        for date in window.iter().take(11) {
            hours(&mut digest, date, "fitness", 1.0);
        }
        digest.add(&window[11], "fitness", 3599);
        digest.add(&window[12], "fitness", 50 * 60);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert!(score.owed.contains("1 more day"), "{}", score.owed);
        assert_eq!(score.actual_label, "11 days");

        // A single day of exactly one hour does hit.
        let mut digest = DigestSlice::default();
        digest.add(&window[59], "fitness", 3600);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "1 days");
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

    // ── 3. pause extends the window ─────────────────────────────────────────

    #[test]
    fn a_paused_day_is_skipped_and_extends_the_window() {
        let catalog = catalog();
        let spec = spec("r-fitness", "Fitness", RuleKind::CategoryDays, EARLY);
        let revisions = [revision(EARLY, &days_params(&["fitness"], 60, 60, 12))];
        let window = days_ending(SUNDAY, 60);
        let paused_day = "2026-10-10";
        let just_before = shift_date(&window[0], -1).expect("date"); // 2026-08-19

        let mut digest = DigestSlice::default();
        // Eleven hits inside the naive window, plus the paused day (which would
        // have been a hit) and one hit just before the naive window.
        for date in window.iter().skip(10).take(11) {
            hours(&mut digest, date, "fitness", 1.0);
        }
        hours(&mut digest, paused_day, "fitness", 1.0);
        hours(&mut digest, &just_before, "fitness", 1.0);

        let pauses = [PausePeriod {
            rule_id: Some("r-fitness".to_string()),
            from_date: paused_day.to_string(),
            to_date: paused_day.to_string(),
        }];
        let score = score_rule(
            &spec, &revisions, &pauses, &catalog, &digest, SUNDAY, SUNDAY,
        )
        .expect("scores");
        assert!(score.on_track, "{score:?}");
        assert_eq!(cell(&score, paused_day), "paused");

        let counted = evidence_days(&score);
        assert_eq!(counted.len(), 60);
        assert!(!counted.contains(&paused_day.to_string()));
        // The window reached one day further back because of the pause.
        assert_eq!(counted.first(), Some(&just_before));
    }

    #[test]
    fn a_paused_day_does_not_break_a_streak() {
        let catalog = catalog();
        let spec = spec("r-fitness", "Fitness", RuleKind::CategoryDays, "2026-10-15");
        let revisions = [revision("2026-10-15", &days_params(&["fitness"], 1, 60, 1))];
        let mut digest = DigestSlice::default();
        hours(&mut digest, "2026-10-15", "fitness", 1.0);
        hours(&mut digest, "2026-10-17", "fitness", 1.0);
        let pauses = [PausePeriod {
            rule_id: Some("r-fitness".to_string()),
            from_date: "2026-10-16".to_string(),
            to_date: "2026-10-16".to_string(),
        }];
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
        let spec = spec("r-learning", "Learning", RuleKind::CategoryTotal, EARLY);
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let window = days_ending(SUNDAY, 7);

        let mut digest = DigestSlice::default();
        digest.add(&window[3], "learning", 4 * 3600);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "4.0h");
        assert_eq!(score.target_label, "≥4h / 7d");
        assert_eq!(score.condition, "≥4h / 7d");
        assert_eq!(evidence_points(&score).len(), 14);

        let mut digest = DigestSlice::default();
        digest.add(&window[3], "learning", 4 * 3600 - 1);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "need +0.0h");
        assert_eq!(score.actual_label, "4.0h");

        // `lte` fails when the sum is over the target.
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Lte, 1.0),
        )];
        let mut digest = DigestSlice::default();
        digest.add(&window[3], "learning", 2 * 3600);
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "need -1.0h");
        assert_eq!(score.condition, "≤1h / 7d");

        // A day outside the window does not count.
        let mut digest = DigestSlice::default();
        digest.add(&days_ending(SUNDAY, 8)[0], "learning", 4 * 3600);
        let revisions = [revision(
            EARLY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
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
        let mut digest = DigestSlice::default();
        for date in ["2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16"] {
            hours(&mut digest, date, "work", 7.0);
        }
        // A Saturday does not count (and is `off` in the strip).
        hours(&mut digest, "2026-10-17", "work", 10.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, "2026-10-17"), "off");
        assert_eq!(cell(&score, "2026-10-18"), "off");
        assert_eq!(cell(&score, MONDAY), "pending");
        // Monday (today) has no rows: it is an included weekday worth 0h, which
        // pulls the mean down to 28 / 5.
        assert_eq!(score.actual_label, "5.60h mean");
        assert!(!score.on_track);
        assert_eq!(score.owed, "need +1.4h on the mean today 0.0 / 7.0h · 7.0h left");
        assert_eq!(score.target_label, "≥7h weekday mean");
        assert_eq!(score.condition, "≥7h weekday mean / 5d");
    }

    #[test]
    fn travel_days_are_excluded_and_extend_the_window() {
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

        // Waking = 2h work + 3h travel → 60% → excluded, and the window reaches
        // back one more weekday to keep five included days.
        let mut digest = DigestSlice::default();
        hours(&mut digest, MONDAY, "work", 2.0);
        hours(&mut digest, MONDAY, "travel", 3.0);
        for date in ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16"] {
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

        // Exactly 50% is not "more than 50%".
        let mut digest = DigestSlice::default();
        hours(&mut digest, MONDAY, "work", 1.0);
        hours(&mut digest, MONDAY, "travel", 1.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "pending");
        assert_eq!(score.actual_label, "0.20h mean");

        // Waking 0 (unclassified only) is never excluded, and productive stays 0.
        let mut digest = DigestSlice::default();
        hours(&mut digest, MONDAY, "", 5.0);
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(cell(&score, MONDAY), "pending");
        assert_eq!(score.actual_label, "0.00h mean");
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

        let mut digest = DigestSlice::default();
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.actual_label, "6.50h median");
        assert_eq!(score.target_label, "≥6.5h median");
        assert_eq!(score.condition, "median ≥6.5h / 60d");
        assert_eq!(score.owed, "on track");

        // Thirteen nights is a fail, not a pass.
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
        for (index, date) in window.iter().enumerate() {
            let seconds = if index < 7 { 6 * 3600 } else { 7 * 3600 };
            digest.add_night(date, 630, 300, seconds);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert_eq!(score.actual_label, "6.50h median");
        assert!(score.on_track);

        // Too short: the deficit sentence.
        let mut digest = DigestSlice::default();
        for date in &window {
            digest.add_night(date, 630, 300, 6 * 3600);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(!score.on_track);
        assert_eq!(score.owed, "median is 0.50h short");
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
        for date in &window {
            digest.add_night(date, 630, 300, 23_400);
        }
        let score = run(&spec, &revisions, &catalog, &digest, SUNDAY);
        assert!(score.on_track);
        assert_eq!(score.condition, "median wake ≤05:00");

        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
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
        let mut digest = DigestSlice::default();
        digest.add(MONDAY, "work", 15_120); // 4.2h
        let score = run(&spec, &revisions, &catalog, &digest, MONDAY);
        assert_eq!(score.actual_label, "0.84h mean");
        assert!(score.owed.contains("today 4.2 / 7.0h"), "{}", score.owed);
        assert!(score.owed.contains("2.8h left"), "{}", score.owed);
        assert_eq!(score.owed, "need +6.2h on the mean today 4.2 / 7.0h · 2.8h left");
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
        let mut digest = DigestSlice::default();
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
                "reclassifying",
                "rules",
                "time_zone",
            ]
        );
        assert_eq!(value["as_of"], SUNDAY);
        assert_eq!(value["time_zone"], "Asia/Kolkata");
        assert_eq!(value["on_track"], 1);
        assert_eq!(value["active_rules"], 1);
        assert_eq!(value["all_pass_streak"], 0);
        assert_eq!(value["dirty_days"], 0);

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
            ]
        );
        assert_eq!(rule["kind"], "category_total");
        assert_eq!(rule["phase"], "active");
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
    }

    /// A fresh rule with no history: `compliance` must serialize as `null`.
    fn spec_json(catalog: &CategoryCatalog) -> serde_json::Value {
        let spec = spec("r-new", "Learning", RuleKind::CategoryTotal, SUNDAY);
        let revisions = [revision(
            SUNDAY,
            &total_params(ids(&["learning"]), 7, Comparator::Gte, 4.0),
        )];
        let score = run(&spec, &revisions, catalog, &DigestSlice::default(), SUNDAY);
        serde_json::to_value(&score).expect("serializes")
    }

    // ── load floor ──────────────────────────────────────────────────────────

    #[test]
    fn load_floor_reaches_the_whole_lookback() {
        // 60-day window → 180-day cap; the rule started 2026-06-01.
        assert_eq!(digest_load_floor("2026-06-01", 60, SUNDAY), "2025-12-04");
        // A rule created today still needs the strip's own windows.
        assert_eq!(digest_load_floor(SUNDAY, 1, SUNDAY), "2026-10-03");
        // A very old rule is clamped to the streak floor (as_of - 399 days).
        assert_eq!(digest_load_floor("2000-01-01", 366, SUNDAY), "2024-08-11");
    }
}


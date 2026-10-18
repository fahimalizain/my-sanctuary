//! Consistency-rule params: the serde shapes stored in
//! `rule_revisions.params_json`, their validation, and the pure condition /
//! label formatting that needs no database.
//!
//! # Shape
//!
//! One struct per [`RuleKind`]. `rules.kind` is the source of truth for the
//! kind, and the stored JSON **must** agree with it: a `kind` key inside the
//! params object is accepted but has to match (400 otherwise), and the
//! canonical JSON this module writes never carries one. Params are parsed with
//! `deny_unknown_fields`, so a typo is a 400 rather than a silently ignored
//! field.
//!
//! # Canonical JSON
//!
//! [`RuleParams::to_json`] is the canonical form: `weekdays` are deduped and
//! sorted, and an absent `exclude_if` is omitted. Revisions compare canonical
//! strings, so `{"exclude_if":null}` and an omitted field are the same params.

use std::collections::BTreeSet;

use serde::de::Error as DeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::digest::SYNTHETIC_SLEEP_CATEGORY_ID;

use super::RulesError;

/// Longest accepted rule name.
pub const MAX_NAME_LEN: usize = 80;
/// Longest accepted icon string.
pub const MAX_ICON_LEN: usize = 32;
/// Longest accepted pause reason.
pub const MAX_REASON_LEN: usize = 200;
/// `window_days` upper bound.
pub const MAX_WINDOW_DAYS: i64 = 366;

// ──────────────────────────────────────────
// Enums
// ──────────────────────────────────────────

/// `rules.kind` — the scoring family, immutable after create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleKind {
    CategoryTotal,
    CategoryDays,
    CategoryDailyAvg,
    SleepDuration,
    SleepTiming,
}

impl RuleKind {
    /// Every kind, in a stable order.
    pub const ALL: [RuleKind; 5] = [
        RuleKind::CategoryTotal,
        RuleKind::CategoryDays,
        RuleKind::CategoryDailyAvg,
        RuleKind::SleepDuration,
        RuleKind::SleepTiming,
    ];

    /// The stored `rules.kind` string.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleKind::CategoryTotal => "category_total",
            RuleKind::CategoryDays => "category_days",
            RuleKind::CategoryDailyAvg => "category_daily_avg",
            RuleKind::SleepDuration => "sleep_duration",
            RuleKind::SleepTiming => "sleep_timing",
        }
    }

    /// Parses a stored kind string.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// Whether this kind scores a category selector.
    pub fn uses_categories(self) -> bool {
        matches!(
            self,
            RuleKind::CategoryTotal | RuleKind::CategoryDays | RuleKind::CategoryDailyAvg
        )
    }
}

impl std::fmt::Display for RuleKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RuleKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RuleKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        RuleKind::parse(&value).ok_or_else(|| {
            D::Error::custom(format!(
                "kind must be one of category_total, category_days, category_daily_avg, \
                 sleep_duration, sleep_timing (got {value:?})"
            ))
        })
    }
}

/// `rules.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleStatus {
    Active,
    Paused,
    Archived,
}

impl RuleStatus {
    /// The stored `rules.status` string.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleStatus::Active => "active",
            RuleStatus::Paused => "paused",
            RuleStatus::Archived => "archived",
        }
    }

    /// Parses a stored status string.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(RuleStatus::Active),
            "paused" => Some(RuleStatus::Paused),
            "archived" => Some(RuleStatus::Archived),
            _ => None,
        }
    }
}

impl Serialize for RuleStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RuleStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        RuleStatus::parse(&value)
            .ok_or_else(|| D::Error::custom(format!("status must be active, paused or archived (got {value:?})")))
    }
}

/// The derived phase of a rule (never stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RulePhase {
    Active,
    Paused,
    Finished,
    Archived,
}

impl RulePhase {
    /// The JSON phase string.
    pub fn as_str(self) -> &'static str {
        match self {
            RulePhase::Active => "active",
            RulePhase::Paused => "paused",
            RulePhase::Finished => "finished",
            RulePhase::Archived => "archived",
        }
    }

    /// Whether the rule is on the scoreboard (active or paused).
    pub fn is_on_board(self) -> bool {
        matches!(self, RulePhase::Active | RulePhase::Paused)
    }
}

impl Serialize for RulePhase {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// `gte` / `lte` — which side of the target passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Comparator {
    Gte,
    Lte,
}

impl Comparator {
    /// `≥` / `≤`.
    pub fn symbol(self) -> &'static str {
        match self {
            Comparator::Gte => "≥",
            Comparator::Lte => "≤",
        }
    }

    /// Whether `value` passes against `target`.
    pub fn passes(self, value: f64, target: f64) -> bool {
        match self {
            Comparator::Gte => value >= target,
            Comparator::Lte => value <= target,
        }
    }
}

/// `sleep_timing.metric`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepMetric {
    BedtimeMedian,
    BedtimeStdev,
    WakeMedian,
    WakeStdev,
}

impl SleepMetric {
    /// Whether the metric is a spread (stdev in minutes) rather than a clock
    /// time.
    pub fn is_spread(self) -> bool {
        matches!(self, SleepMetric::BedtimeStdev | SleepMetric::WakeStdev)
    }

    /// Whether the metric reads `bed_min_past_noon` (else
    /// `wake_min_past_midnight`).
    pub fn is_bedtime(self) -> bool {
        matches!(self, SleepMetric::BedtimeMedian | SleepMetric::BedtimeStdev)
    }
}

// ──────────────────────────────────────────
// Category selector
// ──────────────────────────────────────────

/// The literal `categories` tag meaning "every productive category".
pub const ALL_PRODUCTIVE: &str = "all_productive";

/// Which categories a rule counts: the productive set, or an explicit id list
/// with optional one-level children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategorySelector {
    /// The string `"all_productive"`.
    AllProductive,
    /// `{"ids":[...],"include_children":bool}`.
    Ids {
        ids: Vec<String>,
        include_children: bool,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSelector {
    Tag(String),
    Ids(RawIds),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIds {
    ids: Vec<String>,
    #[serde(default)]
    include_children: bool,
}

#[derive(Serialize)]
struct IdsOut<'a> {
    ids: &'a [String],
    include_children: bool,
}

impl<'de> Deserialize<'de> for CategorySelector {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match RawSelector::deserialize(deserializer)? {
            RawSelector::Tag(tag) if tag == ALL_PRODUCTIVE => Ok(CategorySelector::AllProductive),
            RawSelector::Tag(other) => Err(D::Error::custom(format!(
                "categories must be {ALL_PRODUCTIVE:?} or {{ids, include_children}} (got {other:?})"
            ))),
            RawSelector::Ids(raw) => Ok(CategorySelector::Ids {
                ids: raw.ids,
                include_children: raw.include_children,
            }),
        }
    }
}

impl Serialize for CategorySelector {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            CategorySelector::AllProductive => serializer.serialize_str(ALL_PRODUCTIVE),
            CategorySelector::Ids {
                ids,
                include_children,
            } => IdsOut {
                ids,
                include_children: *include_children,
            }
            .serialize(serializer),
        }
    }
}

/// The daily-average travel escape hatch: skip a day when too much of its
/// waking time sits in the excluded categories.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludeIf {
    pub categories: CategorySelector,
    /// `1..=100`; a day is excluded when the share is **strictly greater**.
    pub percent_of_waking: f64,
}

// ──────────────────────────────────────────
// Per-kind params
// ──────────────────────────────────────────

/// `category_total`: sum the selector over the window, compare to
/// `target_hours`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryTotalParams {
    pub categories: CategorySelector,
    pub window_days: i64,
    pub comparator: Comparator,
    pub target_hours: f64,
}

/// `category_days`: count the days that clear `min_minutes`, compare to
/// `min_days`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryDaysParams {
    pub categories: CategorySelector,
    pub window_days: i64,
    pub min_minutes: i64,
    pub min_days: i64,
}

/// `category_daily_avg`: mean of the selector over the selected weekdays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryDailyAvgParams {
    pub categories: CategorySelector,
    pub window_days: i64,
    /// `0 = Monday … 6 = Sunday` (`num_days_from_monday`).
    pub weekdays: Vec<u32>,
    pub target_hours: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_if: Option<ExcludeIf>,
}

/// `sleep_duration`: median of the paired nights' lengths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SleepDurationParams {
    pub window_days: i64,
    pub min_nights: i64,
    pub target_hours: f64,
}

/// `sleep_timing`: median / sample stdev of the bedtime or wake clock times.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SleepTimingParams {
    pub window_days: i64,
    pub min_nights: i64,
    pub metric: SleepMetric,
    pub comparator: Comparator,
    pub target_minutes: i64,
}

/// A rule's scoring params, tagged by kind.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleParams {
    CategoryTotal(CategoryTotalParams),
    CategoryDays(CategoryDaysParams),
    CategoryDailyAvg(CategoryDailyAvgParams),
    SleepDuration(SleepDurationParams),
    SleepTiming(SleepTimingParams),
}

impl RuleParams {
    /// The kind these params belong to.
    pub fn kind(&self) -> RuleKind {
        match self {
            RuleParams::CategoryTotal(_) => RuleKind::CategoryTotal,
            RuleParams::CategoryDays(_) => RuleKind::CategoryDays,
            RuleParams::CategoryDailyAvg(_) => RuleKind::CategoryDailyAvg,
            RuleParams::SleepDuration(_) => RuleKind::SleepDuration,
            RuleParams::SleepTiming(_) => RuleKind::SleepTiming,
        }
    }

    /// The canonical JSON stored in `rule_revisions.params_json`.
    pub fn to_json(&self) -> String {
        let serialized = match self {
            RuleParams::CategoryTotal(params) => serde_json::to_string(params),
            RuleParams::CategoryDays(params) => serde_json::to_string(params),
            RuleParams::CategoryDailyAvg(params) => serde_json::to_string(params),
            RuleParams::SleepDuration(params) => serde_json::to_string(params),
            RuleParams::SleepTiming(params) => serde_json::to_string(params),
        };
        serialized.expect("rule params always serialize")
    }

    /// Parses stored JSON for a known kind.
    pub fn from_json(kind: RuleKind, json: &str) -> Result<Self, RulesError> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|err| RulesError::Invalid(format!("invalid params: {err}")))?;
        Self::from_value(kind, value)
    }

    /// Parses a request body's `params` value for a known kind. A `kind` key
    /// inside the object is accepted when it agrees with `kind`.
    pub fn from_value(kind: RuleKind, mut value: serde_json::Value) -> Result<Self, RulesError> {
        let object = value
            .as_object_mut()
            .ok_or_else(|| RulesError::Invalid("params must be a JSON object".to_string()))?;
        if let Some(tag) = object.remove("kind") {
            let tag = tag
                .as_str()
                .ok_or_else(|| RulesError::Invalid("params.kind must be a string".to_string()))?;
            if RuleKind::parse(tag) != Some(kind) {
                return Err(RulesError::Invalid(format!(
                    "params.kind {tag:?} does not match the rule kind {kind}"
                )));
            }
        }
        let parsed = match kind {
            RuleKind::CategoryTotal => Self::CategoryTotal(from_value(value)?),
            RuleKind::CategoryDays => Self::CategoryDays(from_value(value)?),
            RuleKind::CategoryDailyAvg => Self::CategoryDailyAvg(from_value(value)?),
            RuleKind::SleepDuration => Self::SleepDuration(from_value(value)?),
            RuleKind::SleepTiming => Self::SleepTiming(from_value(value)?),
        };
        let mut parsed = parsed;
        parsed.normalize();
        Ok(parsed)
    }

    /// Dedupes and sorts `weekdays` so the canonical JSON is stable.
    fn normalize(&mut self) {
        if let RuleParams::CategoryDailyAvg(params) = self {
            params.weekdays.sort_unstable();
            params.weekdays.dedup();
        }
    }

    /// The window length in civil days.
    pub fn window_days(&self) -> i64 {
        match self {
            RuleParams::CategoryTotal(params) => params.window_days,
            RuleParams::CategoryDays(params) => params.window_days,
            RuleParams::CategoryDailyAvg(params) => params.window_days,
            RuleParams::SleepDuration(params) => params.window_days,
            RuleParams::SleepTiming(params) => params.window_days,
        }
    }

    /// The category selector, when the kind has one.
    pub fn selector(&self) -> Option<&CategorySelector> {
        match self {
            RuleParams::CategoryTotal(params) => Some(&params.categories),
            RuleParams::CategoryDays(params) => Some(&params.categories),
            RuleParams::CategoryDailyAvg(params) => Some(&params.categories),
            _ => None,
        }
    }

    /// Validates every bound and every category reference.
    pub fn validate(&self, catalog: &CategoryCatalog) -> Result<(), RulesError> {
        let window = self.window_days();
        if !(1..=MAX_WINDOW_DAYS).contains(&window) {
            return Err(RulesError::Invalid(format!(
                "window_days must be between 1 and {MAX_WINDOW_DAYS}"
            )));
        }
        match self {
            RuleParams::CategoryTotal(params) => {
                validate_selector(&params.categories, catalog)?;
                validate_hours("target_hours", params.target_hours)?;
            }
            RuleParams::CategoryDays(params) => {
                validate_selector(&params.categories, catalog)?;
                if params.min_minutes < 1 {
                    return Err(RulesError::Invalid("min_minutes must be at least 1".to_string()));
                }
                if params.min_days < 1 {
                    return Err(RulesError::Invalid("min_days must be at least 1".to_string()));
                }
            }
            RuleParams::CategoryDailyAvg(params) => {
                validate_selector(&params.categories, catalog)?;
                validate_hours("target_hours", params.target_hours)?;
                if params.weekdays.is_empty() {
                    return Err(RulesError::Invalid("weekdays must not be empty".to_string()));
                }
                if params.weekdays.iter().any(|day| *day > 6) {
                    return Err(RulesError::Invalid(
                        "weekdays must be 0 (Monday) through 6 (Sunday)".to_string(),
                    ));
                }
                if let Some(exclude) = &params.exclude_if {
                    validate_selector(&exclude.categories, catalog)?;
                    if !(exclude.percent_of_waking >= 1.0 && exclude.percent_of_waking <= 100.0) {
                        return Err(RulesError::Invalid(
                            "exclude_if.percent_of_waking must be between 1 and 100".to_string(),
                        ));
                    }
                }
            }
            RuleParams::SleepDuration(params) => {
                if params.min_nights < 1 {
                    return Err(RulesError::Invalid("min_nights must be at least 1".to_string()));
                }
                validate_hours("target_hours", params.target_hours)?;
            }
            RuleParams::SleepTiming(params) => {
                if params.min_nights < 1 {
                    return Err(RulesError::Invalid("min_nights must be at least 1".to_string()));
                }
                if params.target_minutes < 0 {
                    return Err(RulesError::Invalid(
                        "target_minutes must not be negative".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The stable one-liner for the card's target side (e.g. `≥4h / 7d`).
    pub fn condition(&self) -> String {
        match self {
            RuleParams::CategoryTotal(params) => format!(
                "{}{} / {}d",
                params.comparator.symbol(),
                compact_hours(params.target_hours),
                params.window_days
            ),
            RuleParams::CategoryDays(params) => format!(
                "≥{} days of ≥{} / {}d",
                params.min_days,
                compact_minutes(params.min_minutes),
                params.window_days
            ),
            RuleParams::CategoryDailyAvg(params) => {
                format!("≥{} weekday mean / {}d", compact_hours(params.target_hours), params.window_days)
            }
            RuleParams::SleepDuration(params) => format!(
                "median ≥{} / {}d",
                compact_hours(params.target_hours),
                params.window_days
            ),
            RuleParams::SleepTiming(params) => {
                let target = match params.metric {
                    SleepMetric::BedtimeMedian => format_bedtime(params.target_minutes as f64),
                    SleepMetric::WakeMedian => format_wake(params.target_minutes as f64),
                    SleepMetric::BedtimeStdev | SleepMetric::WakeStdev => {
                        format!("{}m", params.target_minutes)
                    }
                };
                let subject = match params.metric {
                    SleepMetric::BedtimeMedian => "median bedtime".to_string(),
                    SleepMetric::WakeMedian => "median wake".to_string(),
                    SleepMetric::BedtimeStdev => "bedtime spread".to_string(),
                    SleepMetric::WakeStdev => "wake spread".to_string(),
                };
                format!("{subject} {}{target}", params.comparator.symbol())
            }
        }
    }

    /// The target side of the card's label pair (e.g. `≥4h / 7d`).
    pub fn target_label(&self) -> String {
        match self {
            RuleParams::CategoryTotal(params) => format!(
                "{}{} / {}d",
                params.comparator.symbol(),
                compact_hours(params.target_hours),
                params.window_days
            ),
            RuleParams::CategoryDays(params) => {
                format!("{} days / {}d", params.min_days, params.window_days)
            }
            RuleParams::CategoryDailyAvg(params) => format!(
                "{}{} weekday mean",
                Comparator::Gte.symbol(),
                compact_hours(params.target_hours)
            ),
            RuleParams::SleepDuration(params) => format!(
                "{}{} median",
                Comparator::Gte.symbol(),
                compact_hours(params.target_hours)
            ),
            RuleParams::SleepTiming(params) => match params.metric {
                SleepMetric::BedtimeMedian => {
                    format!("{}{}", params.comparator.symbol(), format_bedtime(params.target_minutes as f64))
                }
                SleepMetric::WakeMedian => {
                    format!("{}{}", params.comparator.symbol(), format_wake(params.target_minutes as f64))
                }
                SleepMetric::BedtimeStdev | SleepMetric::WakeStdev => {
                    format!("{}{}m", params.comparator.symbol(), params.target_minutes)
                }
            },
        }
    }
}

fn from_value<T: for<'de> Deserialize<'de>>(value: serde_json::Value) -> Result<T, RulesError> {
    serde_json::from_value(value)
        .map_err(|err| RulesError::Invalid(format!("invalid params: {err}")))
}

fn validate_selector(selector: &CategorySelector, catalog: &CategoryCatalog) -> Result<(), RulesError> {
    let CategorySelector::Ids { ids, .. } = selector else {
        return Ok(());
    };
    if ids.is_empty() {
        return Err(RulesError::Invalid("categories.ids must not be empty".to_string()));
    }
    for id in ids {
        if !catalog.contains(id) {
            return Err(RulesError::Invalid(format!("unknown category {id:?}")));
        }
    }
    Ok(())
}

fn validate_hours(field: &str, hours: f64) -> Result<(), RulesError> {
    if !hours.is_finite() || hours < 0.0 {
        return Err(RulesError::Invalid(format!("{field} must be a finite number ≥ 0")));
    }
    Ok(())
}

// ──────────────────────────────────────────
// Formatting helpers
// ──────────────────────────────────────────

/// `4` → `4h`, `6.5` → `6.5h`.
pub fn compact_hours(hours: f64) -> String {
    if hours.fract() == 0.0 {
        format!("{}h", hours as i64)
    } else {
        format!("{hours}h")
    }
}

/// `60` → `1h`, `90` → `90m`.
pub fn compact_minutes(minutes: i64) -> String {
    if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// Minutes past noon back to a wall clock `HH:MM` (rounded to the minute).
pub fn format_bedtime(minutes: f64) -> String {
    clock(((minutes.round() as i64) + 12 * 60).rem_euclid(24 * 60))
}

/// Minutes past midnight as a wall clock `HH:MM` (rounded to the minute).
pub fn format_wake(minutes: f64) -> String {
    clock((minutes.round() as i64).rem_euclid(24 * 60))
}

fn clock(total_minutes: i64) -> String {
    format!("{:02}:{:02}", total_minutes / 60, total_minutes % 60)
}

// ──────────────────────────────────────────
// Category catalog
// ──────────────────────────────────────────

/// One living category, as rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cat {
    pub id: String,
    pub parent_id: Option<String>,
    pub is_productive: bool,
    pub slug: String,
    pub title: String,
}

impl Cat {
    /// A convenience constructor for tests and seeds.
    pub fn new(
        id: &str,
        parent_id: Option<&str>,
        is_productive: bool,
        slug: &str,
        title: &str,
    ) -> Self {
        Self {
            id: id.to_string(),
            parent_id: parent_id.map(|parent| parent.to_string()),
            is_productive,
            slug: slug.to_string(),
            title: title.to_string(),
        }
    }
}

/// The user's living categories (a one-level tree), in the order the caller
/// loaded them — [`CategoryCatalog::resolve`] depends on that order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryCatalog {
    cats: Vec<Cat>,
}

impl CategoryCatalog {
    /// Builds a catalog from rows.
    pub fn new(cats: Vec<Cat>) -> Self {
        Self { cats }
    }

    /// Whether the user has no living categories.
    pub fn is_empty(&self) -> bool {
        self.cats.is_empty()
    }

    /// The number of living categories.
    pub fn len(&self) -> usize {
        self.cats.len()
    }

    /// Every living category.
    pub fn cats(&self) -> &[Cat] {
        &self.cats
    }

    /// Looks a category up by id.
    pub fn get(&self, id: &str) -> Option<&Cat> {
        self.cats.iter().find(|cat| cat.id == id)
    }

    /// Whether the id is a living category of this user.
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// The category's own `is_productive`, or its parent's — one level only.
    /// This is how a leaf stored in the digest inherits a productive root.
    pub fn productive(&self, id: &str) -> bool {
        let Some(cat) = self.get(id) else {
            return false;
        };
        if cat.is_productive {
            return true;
        }
        cat.parent_id
            .as_deref()
            .and_then(|parent| self.get(parent))
            .is_some_and(|parent| parent.is_productive)
    }

    /// The synthetic-sleep sentinel plus every living category whose slug or
    /// title is `sleep` (case-insensitive).
    pub fn sleep_ids(&self) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        out.insert(SYNTHETIC_SLEEP_CATEGORY_ID.to_string());
        for cat in &self.cats {
            if cat.slug.eq_ignore_ascii_case("sleep") || cat.title.eq_ignore_ascii_case("sleep") {
                out.insert(cat.id.clone());
            }
        }
        out
    }

    /// Expands a selector to concrete category ids.
    ///
    /// `all_productive` → every living productive id. An id list → those ids
    /// (living only), plus their living children when `include_children` is
    /// set (one level: a child of a listed child is never pulled in).
    pub fn expand(&self, selector: &CategorySelector) -> BTreeSet<String> {
        match selector {
            CategorySelector::AllProductive => self
                .cats
                .iter()
                .filter(|cat| self.productive(&cat.id))
                .map(|cat| cat.id.clone())
                .collect(),
            CategorySelector::Ids {
                ids,
                include_children,
            } => {
                let parents: BTreeSet<String> = ids
                    .iter()
                    .filter(|id| self.contains(id))
                    .cloned()
                    .collect();
                let mut out = parents.clone();
                if *include_children {
                    for cat in &self.cats {
                        if cat
                            .parent_id
                            .as_deref()
                            .is_some_and(|parent| parents.contains(parent))
                        {
                            out.insert(cat.id.clone());
                        }
                    }
                }
                out
            }
        }
    }

    /// Resolves a category by slug **or** title, case-insensitively. First
    /// match in load order wins.
    pub fn resolve(&self, needle: &str) -> Option<&Cat> {
        self.cats.iter().find(|cat| {
            cat.slug.eq_ignore_ascii_case(needle) || cat.title.eq_ignore_ascii_case(needle)
        })
    }
}

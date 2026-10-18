//! The rules service: CRUD, seeding, digest loading, and the read models the
//! Worker hands straight to `Response::from_json`.
//!
//! Everything the scorer needs is loaded in a handful of queries (rules,
//! revisions, pauses, the living category tree, and one range scan each over
//! `daily_category_minutes` and `sleep_nights`), then folded by the pure
//! [`super::score`] functions. The load range is derived from the rules
//! themselves by [`digest_load_floor`].

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::digest::{digest_status, shift_date, DigestRepo};
use crate::time::{civil_date_in_zone, unix_secs_to_rfc3339};

use super::params::{
    Cat, CategoryCatalog, CategoryDailyAvgParams, CategoryDaysParams, CategorySelector,
    CategoryTotalParams, ExcludeIf, RuleKind, RuleParams, RulePhase, RuleStatus, SleepDurationParams,
    SleepMetric, SleepTimingParams, MAX_ICON_LEN, MAX_NAME_LEN, MAX_REASON_LEN, MAX_WINDOW_DAYS,
};
use super::repo::{
    CategoryRow, NewRuleRow, PauseRow, RuleRepo, RuleRow, SleepNightRow,
};
use super::score::{
    digest_load_floor, rule_phase, score_board, score_rule, ArchiveResponse, BoardInput,
    DigestFlags, DigestSlice, Night, PausePeriod, Revision, RuleScore, RuleSpec, Scoreboard,
};
use super::RulesError;

/// How far back a freshly seeded rule starts, so a 60-day window and a 14-day
/// strip both have history.
pub const SEED_HISTORY_DAYS: i64 = 120;
/// `exclude_if.percent_of_waking` for the seeded productive-weekday rule.
const SEED_TRAVEL_PERCENT: f64 = 50.0;

// ──────────────────────────────────────────
// Request bodies
// ──────────────────────────────────────────

/// `POST /api/rules`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewRuleInput {
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub kind: RuleKind,
    /// Defaults to `as_of`.
    #[serde(default)]
    pub starts_on: Option<String>,
    #[serde(default)]
    pub ends_on: Option<String>,
    pub params: serde_json::Value,
}

/// `PATCH /api/rules/:id`. Every field optional; `kind` may only repeat the
/// stored kind.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRuleInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub kind: Option<RuleKind>,
    #[serde(default)]
    pub starts_on: Option<String>,
    /// `null` clears the challenge end; a missing key leaves it.
    #[serde(default, deserialize_with = "de_optional_option")]
    pub ends_on: Option<Option<String>>,
    #[serde(default)]
    pub sort_order: Option<i64>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

/// `POST /api/rules/:id/pause`. No `from`/`to` = open-ended pause.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PauseRuleInput {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// `POST /api/pauses` — a global (every rule) period.
#[derive(Debug, Clone, Deserialize)]
pub struct NewPauseInput {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /api/rules/reorder`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReorderInput {
    pub ids: Vec<String>,
}

/// Distinguishes "key absent" (`None`) from "key present and null"
/// (`Some(None)`), so `PATCH` can clear `ends_on`.
fn de_optional_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

// ──────────────────────────────────────────
// Response shapes
// ──────────────────────────────────────────

/// One `pause_periods` row as the API renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PauseView {
    pub id: String,
    /// `null` = global.
    pub rule_id: Option<String>,
    pub from: String,
    pub to: String,
    pub reason: String,
    pub created_at: String,
}

impl From<&PauseRow> for PauseView {
    fn from(row: &PauseRow) -> Self {
        Self {
            id: row.id.clone(),
            rule_id: row.rule_id.clone(),
            from: row.from_date.clone(),
            to: row.to_date.clone(),
            reason: row.reason.clone(),
            created_at: row.created_at.clone(),
        }
    }
}

/// `GET /api/pauses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PausesResponse {
    pub pauses: Vec<PauseView>,
}

/// `POST /api/pauses`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PauseResponse {
    pub pause: PauseView,
}

// ──────────────────────────────────────────
// Reads
// ──────────────────────────────────────────

/// "Today" in the user's primary calendar zone (unknown/empty → UTC), plus the
/// zone name that was used.
pub async fn today(
    digest_repo: &dyn DigestRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<(String, String), RulesError> {
    let stored = digest_repo.primary_time_zone(user_id).await?.unwrap_or_default();
    let time_zone = if crate::time::parse_iana_tz(&stored).is_some() {
        stored
    } else {
        "UTC".to_string()
    };
    Ok((civil_date_in_zone(now_unix, &time_zone), time_zone))
}

/// `GET /api/rules/scoreboard`.
pub async fn scoreboard(
    repo: &dyn RuleRepo,
    digest_repo: &dyn DigestRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<Scoreboard, RulesError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let (as_of, time_zone) = today(digest_repo, user_id, now_unix).await?;
    ensure_default_rules(repo, user_id, &as_of, &now_rfc3339).await?;
    // Opening the page also queues history: the rebuild itself belongs to the
    // cron/queue tick and is never awaited here.
    let status = digest_status(digest_repo, user_id, now_unix).await?;
    let rows = repo.list_rules(user_id).await?;
    let loaded = load_inputs(repo, user_id, &as_of, &as_of, &rows).await?;
    let specs = rows
        .iter()
        .filter(|row| has_revisions(&loaded, &row.id))
        .map(RuleRow::spec)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(score_board(BoardInput {
        as_of: &as_of,
        time_zone: &time_zone,
        rules: specs,
        revisions: loaded.revisions,
        pauses: loaded.pauses,
        catalog: &loaded.catalog,
        digest: &loaded.digest,
        flags: DigestFlags {
            reclassifying: status.reclassifying,
            backfilling: status.backfilling,
            dirty_days: status.dirty_days,
        },
    })?)
}

/// `GET /api/rules/archive` — finished and archived rules.
///
/// A finished rule is scored with its cursor at `ends_on`, so its last day is
/// a real verdict and counts; `today` stays the civil today, so the phase is
/// `finished` and nothing is left pending. Archived-but-unfinished rules are
/// scored live at today.
pub async fn archive_rules(
    repo: &dyn RuleRepo,
    digest_repo: &dyn DigestRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<ArchiveResponse, RulesError> {
    let now_rfc3339 = unix_secs_to_rfc3339(now_unix);
    let (today, _time_zone) = today(digest_repo, user_id, now_unix).await?;
    ensure_default_rules(repo, user_id, &today, &now_rfc3339).await?;
    let rows = repo.list_rules(user_id).await?;

    let mut targets: Vec<(RuleSpec, String)> = Vec::new();
    for row in &rows {
        let spec = row.spec()?;
        match rule_phase(&spec, &today) {
            RulePhase::Finished => {
                let at = spec
                    .ends_on
                    .clone()
                    .filter(|end| end.as_str() < today.as_str())
                    .unwrap_or_else(|| today.clone());
                targets.push((spec, at));
            }
            RulePhase::Archived => targets.push((spec, today.clone())),
            RulePhase::Active | RulePhase::Paused => {}
        }
    }
    if targets.is_empty() {
        return Ok(ArchiveResponse { rules: Vec::new() });
    }

    let earliest = targets
        .iter()
        .map(|(_, at)| at.clone())
        .min()
        .unwrap_or_else(|| today.clone());
    let loaded = load_inputs(repo, user_id, &earliest, &today, &rows).await?;
    let mut rules = Vec::with_capacity(targets.len());
    for (spec, at) in &targets {
        if !has_revisions(&loaded, &spec.id) {
            continue;
        }
        let revisions = loaded.revisions.get(&spec.id).cloned().unwrap_or_default();
        rules.push(score_rule(
            spec,
            &revisions,
            &loaded.pauses,
            &loaded.catalog,
            &loaded.digest,
            at,
            &today,
        )?);
    }
    Ok(ArchiveResponse { rules })
}

/// Whether a rule can be scored at all.
///
/// The seed (and `create_rule`) writes the rule and its first revision in two
/// statements, so a failure in between can leave a revision-less rule behind.
/// Such a rule is invisible on the board instead of taking the whole page down;
/// a `PATCH` that sends `params` gives it a revision and it reappears.
fn has_revisions(loaded: &Loaded, rule_id: &str) -> bool {
    loaded
        .revisions
        .get(rule_id)
        .is_some_and(|revisions| !revisions.is_empty())
}

/// The loaded inputs a scoring pass shares across rules.
struct Loaded {
    catalog: CategoryCatalog,
    digest: DigestSlice,
    pauses: Vec<PausePeriod>,
    revisions: BTreeMap<String, Vec<Revision>>,
}

/// Loads rules-adjacent rows and the digest range the rules need.
///
/// `earliest_as_of` is the earliest date any rule is scored at (the archive
/// scores finished rules at their `ends_on`), which is what the digest floor
/// has to reach.
async fn load_inputs(
    repo: &dyn RuleRepo,
    user_id: &str,
    earliest_as_of: &str,
    as_of: &str,
    rows: &[RuleRow],
) -> Result<Loaded, RulesError> {
    let mut revisions: BTreeMap<String, Vec<Revision>> = BTreeMap::new();
    let mut max_window = 1_i64;
    let mut min_starts_on: Option<String> = None;
    for row in rows {
        let kind = row.spec()?.kind;
        let stored = repo.list_revisions(&row.id).await?;
        for revision in &stored {
            let params = RuleParams::from_json(kind, &revision.params_json)?;
            max_window = max_window.max(params.window_days().clamp(1, MAX_WINDOW_DAYS));
        }
        min_starts_on = Some(match min_starts_on {
            Some(current) if current <= row.starts_on => current,
            _ => row.starts_on.clone(),
        });
        revisions.insert(
            row.id.clone(),
            stored
                .into_iter()
                .map(|row| Revision {
                    id: row.id,
                    effective_from: row.effective_from,
                    params_json: row.params_json,
                    created_at: row.created_at,
                })
                .collect(),
        );
    }

    let catalog = load_catalog(repo, user_id).await?;
    let pauses = repo
        .list_pauses(user_id)
        .await?
        .iter()
        .map(|row| PausePeriod {
            rule_id: row.rule_id.clone(),
            from_date: row.from_date.clone(),
            to_date: row.to_date.clone(),
        })
        .collect();
    let floor = match min_starts_on {
        Some(starts_on) => digest_load_floor(&starts_on, max_window, earliest_as_of),
        None => digest_load_floor(as_of, max_window, earliest_as_of),
    };
    let digest = load_digest(repo, user_id, &floor, as_of).await?;

    Ok(Loaded {
        catalog,
        digest,
        pauses,
        revisions,
    })
}

/// The user's living category tree.
async fn load_catalog(repo: &dyn RuleRepo, user_id: &str) -> Result<CategoryCatalog, RulesError> {
    let rows: Vec<CategoryRow> = repo.load_categories(user_id).await?;
    Ok(CategoryCatalog::new(rows.into_iter().map(Cat::from).collect()))
}

/// The digest slice over the inclusive `[from, to]` date range.
async fn load_digest(
    repo: &dyn RuleRepo,
    user_id: &str,
    from: &str,
    to: &str,
) -> Result<DigestSlice, RulesError> {
    let mut digest = DigestSlice::default();
    for row in repo.load_category_seconds(user_id, from, to).await? {
        *digest
            .category_seconds
            .entry(row.local_date)
            .or_default()
            .entry(row.category_id)
            .or_insert(0) += row.seconds;
    }
    digest.nights = repo
        .load_sleep_nights(user_id, from, to)
        .await?
        .into_iter()
        .map(|row: SleepNightRow| Night {
            wake_date: row.wake_date,
            asleep_at: row.asleep_at,
            bed_min_past_noon: row.bed_min_past_noon,
            wake_min_past_midnight: row.wake_min_past_midnight,
            seconds: row.seconds,
            source: row.source,
        })
        .collect();
    Ok(digest)
}

/// Scores one rule after a write, so every mutating endpoint answers with the
/// same card shape the scoreboard uses.
async fn score_one(
    repo: &dyn RuleRepo,
    user_id: &str,
    as_of: &str,
    rule: &RuleRow,
) -> Result<RuleScore, RulesError> {
    let loaded = load_inputs(repo, user_id, as_of, as_of, std::slice::from_ref(rule)).await?;
    let spec = rule.spec()?;
    let revisions = loaded.revisions.get(&spec.id).cloned().unwrap_or_default();
    // A write's card is live: its cursor is today, and today stays pending.
    Ok(score_rule(
        &spec,
        &revisions,
        &loaded.pauses,
        &loaded.catalog,
        &loaded.digest,
        as_of,
        as_of,
    )?)
}

// ──────────────────────────────────────────
// Seeding
// ──────────────────────────────────────────

/// The categories a seed rule may need, resolved by slug **or** title.
#[derive(Debug, Clone, Default)]
struct SeedCats {
    fitness: Option<String>,
    learning: Option<String>,
    trip: Option<String>,
    travel: Option<String>,
}

/// One seeded rule: skipped (returning `None`) when a required category is
/// missing.
struct SeedRule {
    name: &'static str,
    icon: &'static str,
    kind: RuleKind,
    params: fn(&SeedCats) -> Option<RuleParams>,
}

/// The eight enforcement rules, in `sort_order` order.
fn seed_table() -> [SeedRule; 8] {
    [
        SeedRule {
            name: "Fitness",
            icon: "fitness",
            kind: RuleKind::CategoryDays,
            params: |cats| {
                Some(RuleParams::CategoryDays(CategoryDaysParams {
                    categories: CategorySelector::Ids {
                        ids: vec![cats.fitness.clone()?],
                        include_children: true,
                    },
                    window_days: 60,
                    min_minutes: 60,
                    min_days: 12,
                }))
            },
        },
        SeedRule {
            name: "Productive weekdays",
            icon: "productive",
            kind: RuleKind::CategoryDailyAvg,
            params: |cats| {
                let mut ids: Vec<String> = Vec::new();
                if let Some(trip) = &cats.trip {
                    ids.push(trip.clone());
                }
                if let Some(travel) = &cats.travel {
                    ids.push(travel.clone());
                }
                // Neither trip nor travel exists → no escape hatch at all.
                let exclude_if = if ids.is_empty() {
                    None
                } else {
                    Some(ExcludeIf {
                        categories: CategorySelector::Ids {
                            ids,
                            include_children: true,
                        },
                        percent_of_waking: SEED_TRAVEL_PERCENT,
                    })
                };
                Some(RuleParams::CategoryDailyAvg(CategoryDailyAvgParams {
                    categories: CategorySelector::AllProductive,
                    window_days: 60,
                    weekdays: vec![0, 1, 2, 3, 4],
                    target_hours: 7.0,
                    exclude_if,
                }))
            },
        },
        SeedRule {
            name: "Learning",
            icon: "learning",
            kind: RuleKind::CategoryTotal,
            params: |cats| {
                Some(RuleParams::CategoryTotal(CategoryTotalParams {
                    categories: CategorySelector::Ids {
                        ids: vec![cats.learning.clone()?],
                        include_children: true,
                    },
                    window_days: 7,
                    comparator: super::params::Comparator::Gte,
                    target_hours: 4.0,
                }))
            },
        },
        SeedRule {
            name: "Sleep duration",
            icon: "sleep",
            kind: RuleKind::SleepDuration,
            params: |_| {
                Some(RuleParams::SleepDuration(SleepDurationParams {
                    window_days: 60,
                    min_nights: 14,
                    target_hours: 6.5,
                }))
            },
        },
        SeedRule {
            name: "Bedtime",
            icon: "bedtime",
            kind: RuleKind::SleepTiming,
            params: |_| {
                Some(RuleParams::SleepTiming(SleepTimingParams {
                    window_days: 60,
                    min_nights: 14,
                    metric: SleepMetric::BedtimeMedian,
                    comparator: super::params::Comparator::Lte,
                    target_minutes: 630,
                }))
            },
        },
        SeedRule {
            name: "Bedtime spread",
            icon: "bedtime",
            kind: RuleKind::SleepTiming,
            params: |_| {
                Some(RuleParams::SleepTiming(SleepTimingParams {
                    window_days: 60,
                    min_nights: 14,
                    metric: SleepMetric::BedtimeStdev,
                    comparator: super::params::Comparator::Lte,
                    target_minutes: 45,
                }))
            },
        },
        SeedRule {
            name: "Wake",
            icon: "wake",
            kind: RuleKind::SleepTiming,
            params: |_| {
                Some(RuleParams::SleepTiming(SleepTimingParams {
                    window_days: 60,
                    min_nights: 14,
                    metric: SleepMetric::WakeMedian,
                    comparator: super::params::Comparator::Lte,
                    target_minutes: 300,
                }))
            },
        },
        SeedRule {
            name: "Wake spread",
            icon: "wake",
            kind: RuleKind::SleepTiming,
            params: |_| {
                Some(RuleParams::SleepTiming(SleepTimingParams {
                    window_days: 60,
                    min_nights: 14,
                    metric: SleepMetric::WakeStdev,
                    comparator: super::params::Comparator::Lte,
                    target_minutes: 60,
                }))
            },
        },
    ]
}

/// Seeds the eight enforcement rules when the user has **zero** rules rows
/// (archived ones count — this never reseeds). Returns how many were inserted.
///
/// A rule whose required category is missing is skipped, and the remaining
/// rules keep compact `sort_order`s in table order.
pub async fn ensure_default_rules(
    repo: &dyn RuleRepo,
    user_id: &str,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<usize, RulesError> {
    if repo.count_rules(user_id).await? > 0 {
        return Ok(0);
    }
    let catalog = load_catalog(repo, user_id).await?;
    let cats = SeedCats {
        fitness: catalog.resolve("fitness").map(|cat| cat.id.clone()),
        learning: catalog.resolve("learning").map(|cat| cat.id.clone()),
        trip: catalog.resolve("trip").map(|cat| cat.id.clone()),
        travel: catalog.resolve("travel").map(|cat| cat.id.clone()),
    };
    let starts_on = shift_date(as_of, -SEED_HISTORY_DAYS).unwrap_or_else(|| as_of.to_string());

    let mut inserted = 0_usize;
    for seed in seed_table() {
        let Some(params) = (seed.params)(&cats) else {
            continue;
        };
        let rule = repo
            .insert_rule(
                &NewRuleRow {
                    user_id: user_id.to_string(),
                    name: seed.name.to_string(),
                    icon: seed.icon.to_string(),
                    kind: seed.kind.as_str().to_string(),
                    status: RuleStatus::Active.as_str().to_string(),
                    starts_on: starts_on.clone(),
                    ends_on: None,
                    paused_on: None,
                    sort_order: inserted as i64,
                },
                now_rfc3339,
            )
            .await?;
        repo.insert_revision(&rule.id, &starts_on, &params.to_json(), now_rfc3339)
            .await?;
        inserted += 1;
    }
    Ok(inserted)
}

// ──────────────────────────────────────────
// CRUD
// ──────────────────────────────────────────

/// `POST /api/rules`.
pub async fn create_rule(
    repo: &dyn RuleRepo,
    user_id: &str,
    input: &NewRuleInput,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<RuleScore, RulesError> {
    validate_name(&input.name)?;
    let icon = input.icon.clone().unwrap_or_default();
    validate_icon(&icon)?;

    let catalog = load_catalog(repo, user_id).await?;
    let params = RuleParams::from_value(input.kind, input.params.clone())?;
    if params.kind() != input.kind {
        return Err(RulesError::Invalid(
            "params do not match the rule kind".to_string(),
        ));
    }
    params.validate(&catalog)?;

    let starts_on = match &input.starts_on {
        Some(starts_on) => {
            validate_date("starts_on", starts_on)?;
            starts_on.clone()
        }
        None => as_of.to_string(),
    };
    let ends_on = match &input.ends_on {
        Some(ends_on) => {
            validate_date("ends_on", ends_on)?;
            Some(ends_on.clone())
        }
        None => None,
    };
    if let Some(ends_on) = &ends_on {
        if ends_on.as_str() < starts_on.as_str() {
            return Err(RulesError::Invalid(
                "ends_on must not be before starts_on".to_string(),
            ));
        }
    }

    let rule = repo
        .insert_rule(
            &NewRuleRow {
                user_id: user_id.to_string(),
                name: input.name.trim().to_string(),
                icon,
                kind: input.kind.as_str().to_string(),
                status: RuleStatus::Active.as_str().to_string(),
                starts_on: starts_on.clone(),
                ends_on,
                paused_on: None,
                sort_order: next_sort_order(repo, user_id).await?,
            },
            now_rfc3339,
        )
        .await?;
    // The first revision is effective from `starts_on` (not "today"), so the
    // original params apply from the beginning.
    repo.insert_revision(&rule.id, &starts_on, &params.to_json(), now_rfc3339)
        .await?;
    score_one(repo, user_id, as_of, &rule).await
}

/// `PATCH /api/rules/:id`.
pub async fn update_rule(
    repo: &dyn RuleRepo,
    user_id: &str,
    id: &str,
    input: &UpdateRuleInput,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<RuleScore, RulesError> {
    if input.name.is_none()
        && input.icon.is_none()
        && input.kind.is_none()
        && input.starts_on.is_none()
        && input.ends_on.is_none()
        && input.sort_order.is_none()
        && input.params.is_none()
    {
        return Err(RulesError::Invalid("nothing to update".to_string()));
    }
    let Some(mut rule) = repo.get_rule(user_id, id).await? else {
        return Err(RulesError::RuleNotFound);
    };
    let spec = rule.spec()?;

    if let Some(kind) = input.kind {
        if kind != spec.kind {
            return Err(RulesError::Invalid("kind cannot change".to_string()));
        }
    }
    if let Some(name) = &input.name {
        validate_name(name)?;
        rule.name = name.trim().to_string();
    }
    if let Some(icon) = &input.icon {
        validate_icon(icon)?;
        rule.icon = icon.clone();
    }
    if let Some(sort_order) = input.sort_order {
        rule.sort_order = sort_order;
    }
    if let Some(starts_on) = &input.starts_on {
        validate_date("starts_on", starts_on)?;
        rule.starts_on = starts_on.clone();
    }
    if let Some(ends_on) = &input.ends_on {
        match ends_on {
            Some(ends_on) => {
                validate_date("ends_on", ends_on)?;
                rule.ends_on = Some(ends_on.clone());
            }
            None => rule.ends_on = None,
        }
    }
    if let Some(ends_on) = &rule.ends_on {
        if ends_on.as_str() < rule.starts_on.as_str() {
            return Err(RulesError::Invalid(
                "ends_on must not be before starts_on".to_string(),
            ));
        }
    }

    // Only a real params change forks a revision; the day before `as_of` keeps
    // the previous one.
    if let Some(value) = &input.params {
        let catalog = load_catalog(repo, user_id).await?;
        let params = RuleParams::from_value(spec.kind, value.clone())?;
        params.validate(&catalog)?;
        let stored = repo.list_revisions(&rule.id).await?;
        let current = stored
            .iter()
            .rev()
            .find(|revision| revision.effective_from.as_str() <= as_of)
            .or_else(|| stored.first())
            .map(|revision| RuleParams::from_json(spec.kind, &revision.params_json))
            .transpose()?;
        if current.as_ref() != Some(&params) {
            repo.insert_revision(&rule.id, as_of, &params.to_json(), now_rfc3339)
                .await?;
        }
    }

    repo.update_rule(&rule, now_rfc3339).await?;
    score_one(repo, user_id, as_of, &rule).await
}

/// `POST /api/rules/:id/pause`.
///
/// No `from`/`to` → open-ended pause (`status = paused`, `paused_on = as_of`).
/// Both → a scheduled closed per-rule period; `status` is untouched.
pub async fn pause_rule(
    repo: &dyn RuleRepo,
    user_id: &str,
    id: &str,
    input: &PauseRuleInput,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<RuleScore, RulesError> {
    let Some(mut rule) = repo.get_rule(user_id, id).await? else {
        return Err(RulesError::RuleNotFound);
    };
    match (&input.from, &input.to) {
        (None, None) => {
            let spec = rule.spec()?;
            if rule_phase(&spec, as_of) != RulePhase::Active {
                return Err(RulesError::Conflict("rule is not active".to_string()));
            }
            rule.status = RuleStatus::Paused.as_str().to_string();
            rule.paused_on = Some(as_of.to_string());
            repo.update_rule(&rule, now_rfc3339).await?;
        }
        (Some(from), Some(to)) => {
            validate_date("from", from)?;
            validate_date("to", to)?;
            if to.as_str() < from.as_str() {
                return Err(RulesError::Invalid("to must not be before from".to_string()));
            }
            repo.insert_pause(user_id, Some(id), from, to, "", now_rfc3339)
                .await?;
        }
        _ => {
            return Err(RulesError::Invalid(
                "pause needs both from and to, or neither".to_string(),
            ))
        }
    }
    score_one(repo, user_id, as_of, &rule).await
}

/// `POST /api/rules/:id/resume`. Closes the open pause (if it started before
/// today) into a per-rule period, then reactivates.
pub async fn resume_rule(
    repo: &dyn RuleRepo,
    user_id: &str,
    id: &str,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<RuleScore, RulesError> {
    let Some(mut rule) = repo.get_rule(user_id, id).await? else {
        return Err(RulesError::RuleNotFound);
    };
    if rule.status != RuleStatus::Paused.as_str() {
        return Err(RulesError::Conflict("rule is not paused".to_string()));
    }
    if let Some(paused_on) = rule.paused_on.clone() {
        if paused_on.as_str() < as_of {
            if let Some(close) = shift_date(as_of, -1) {
                // Open-ended pauses carry no reason of their own; the closed
                // period records an empty one.
                repo.insert_pause(user_id, Some(id), &paused_on, &close, "", now_rfc3339)
                    .await?;
            }
        }
    }
    rule.status = RuleStatus::Active.as_str().to_string();
    rule.paused_on = None;
    repo.update_rule(&rule, now_rfc3339).await?;
    score_one(repo, user_id, as_of, &rule).await
}

/// `POST /api/rules/:id/archive`. Closes an open pause first, then archives.
/// Idempotent.
pub async fn set_rule_archived(
    repo: &dyn RuleRepo,
    user_id: &str,
    id: &str,
    archived: bool,
    as_of: &str,
    now_rfc3339: &str,
) -> Result<RuleScore, RulesError> {
    let Some(mut rule) = repo.get_rule(user_id, id).await? else {
        return Err(RulesError::RuleNotFound);
    };
    if archived {
        if rule.status == RuleStatus::Paused.as_str() {
            if let Some(paused_on) = rule.paused_on.clone() {
                if paused_on.as_str() < as_of {
                    if let Some(close) = shift_date(as_of, -1) {
                        repo.insert_pause(user_id, Some(id), &paused_on, &close, "", now_rfc3339)
                            .await?;
                    }
                }
            }
        }
        rule.status = RuleStatus::Archived.as_str().to_string();
        rule.paused_on = None;
    } else {
        // Restore reactivates but deliberately keeps `ends_on`: a restored
        // finished challenge stays finished until the client clears or moves
        // `ends_on`.
        rule.status = RuleStatus::Active.as_str().to_string();
        rule.paused_on = None;
    }
    repo.update_rule(&rule, now_rfc3339).await?;
    score_one(repo, user_id, as_of, &rule).await
}

/// `POST /api/rules/reorder`. The body must list **every** living rule of the
/// user exactly once; `sort_order` becomes the vec index.
pub async fn reorder_rules(
    repo: &dyn RuleRepo,
    user_id: &str,
    input: &ReorderInput,
    now_rfc3339: &str,
) -> Result<(), RulesError> {
    let rows = repo.list_rules(user_id).await?;
    let known: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.id.as_str()).collect();
    let listed: std::collections::BTreeSet<&str> =
        input.ids.iter().map(String::as_str).collect();
    if listed.len() != input.ids.len() || listed != known {
        return Err(RulesError::Invalid(
            "ids must list every rule of this user exactly once".to_string(),
        ));
    }
    for (index, id) in input.ids.iter().enumerate() {
        repo.set_sort_order(user_id, id, index as i64, now_rfc3339)
            .await?;
    }
    Ok(())
}

// ──────────────────────────────────────────
// Pauses
// ──────────────────────────────────────────

/// `GET /api/pauses`.
pub async fn list_pauses(
    repo: &dyn RuleRepo,
    user_id: &str,
) -> Result<PausesResponse, RulesError> {
    let pauses = repo
        .list_pauses(user_id)
        .await?
        .iter()
        .map(PauseView::from)
        .collect();
    Ok(PausesResponse { pauses })
}

/// `POST /api/pauses` — a global period (every rule).
pub async fn create_global_pause(
    repo: &dyn RuleRepo,
    user_id: &str,
    input: &NewPauseInput,
    now_rfc3339: &str,
) -> Result<PauseResponse, RulesError> {
    validate_date("from", &input.from)?;
    validate_date("to", &input.to)?;
    if input.to.as_str() < input.from.as_str() {
        return Err(RulesError::Invalid("to must not be before from".to_string()));
    }
    let reason = input.reason.clone().unwrap_or_default();
    if reason.chars().count() > MAX_REASON_LEN {
        return Err(RulesError::Invalid(format!(
            "reason must be at most {MAX_REASON_LEN} characters"
        )));
    }
    let row = repo
        .insert_pause(user_id, None, &input.from, &input.to, &reason, now_rfc3339)
        .await?;
    Ok(PauseResponse {
        pause: PauseView::from(&row),
    })
}

/// `DELETE /api/pauses/:id`.
pub async fn delete_pause(
    repo: &dyn RuleRepo,
    user_id: &str,
    id: &str,
) -> Result<(), RulesError> {
    if !repo.list_pauses(user_id).await?.iter().any(|row| row.id == id) {
        return Err(RulesError::PauseNotFound);
    }
    repo.delete_pause(user_id, id).await?;
    Ok(())
}

// ──────────────────────────────────────────
// Small helpers
// ──────────────────────────────────────────

async fn next_sort_order(repo: &dyn RuleRepo, user_id: &str) -> Result<i64, RulesError> {
    let rows = repo.list_rules(user_id).await?;
    Ok(rows
        .iter()
        .map(|row| row.sort_order)
        .max()
        .map(|max| max + 1)
        .unwrap_or(0))
}

fn validate_name(name: &str) -> Result<(), RulesError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_LEN {
        return Err(RulesError::Invalid(format!(
            "name must be 1 to {MAX_NAME_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_icon(icon: &str) -> Result<(), RulesError> {
    if icon.chars().count() > MAX_ICON_LEN {
        return Err(RulesError::Invalid(format!(
            "icon must be at most {MAX_ICON_LEN} characters"
        )));
    }
    Ok(())
}

/// A civil date is valid when it round-trips through `chrono`.
fn validate_date(field: &str, value: &str) -> Result<(), RulesError> {
    if shift_date(value, 0).is_none() {
        return Err(RulesError::Invalid(format!(
            "{field} must be a YYYY-MM-DD date"
        )));
    }
    Ok(())
}

// ──────────────────────────────────────────
// Tests
// ──────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::repo::RepoError;
    use crate::rules::repo::{CategorySecondsRow, RevisionRow, SleepNightRow};

    const AS_OF: &str = "2026-10-18";
    const NOW: &str = "2026-10-18T06:00:00Z";

    /// An in-memory [`RuleRepo`] — no D1, no rusqlite.
    #[derive(Default)]
    struct FakeRuleRepo {
        state: Mutex<FakeState>,
    }

    #[derive(Default)]
    struct FakeState {
        rules: Vec<RuleRow>,
        revisions: Vec<RevisionRow>,
        pauses: Vec<PauseRow>,
        categories: Vec<CategoryRow>,
        next_rule: u64,
        next_revision: u64,
        next_pause: u64,
    }

    impl FakeRuleRepo {
        fn with_categories(slugs: &[(&str, &str)]) -> Self {
            let repo = FakeRuleRepo::default();
            {
                let mut state = repo.state.lock().unwrap();
                state.categories = slugs
                    .iter()
                    .map(|(slug, title)| CategoryRow {
                        id: format!("cat-{slug}"),
                        parent_id: None,
                        is_productive: *slug == "work",
                        slug: slug.to_string(),
                        title: title.to_string(),
                    })
                    .collect();
            }
            repo
        }

        /// Rules in board order, like `list_rules`.
        fn rules(&self) -> Vec<RuleRow> {
            let mut rows = self.state.lock().unwrap().rules.clone();
            rows.sort_by(|left, right| {
                left.sort_order
                    .cmp(&right.sort_order)
                    .then_with(|| left.name.cmp(&right.name))
            });
            rows
        }

        fn revision_params(&self, rule_id: &str) -> Vec<serde_json::Value> {
            self.state
                .lock()
                .unwrap()
                .revisions
                .iter()
                .filter(|revision| revision.rule_id == rule_id)
                .map(|revision| serde_json::from_str(&revision.params_json).expect("valid json"))
                .collect()
        }

        fn push_rule(&self, id: &str, name: &str, kind: RuleKind, starts_on: &str) {
            let mut state = self.state.lock().unwrap();
            let sort_order = state.rules.len() as i64;
            state.rules.push(RuleRow {
                id: id.to_string(),
                user_id: "u-1".to_string(),
                name: name.to_string(),
                icon: String::new(),
                kind: kind.as_str().to_string(),
                status: RuleStatus::Active.as_str().to_string(),
                starts_on: starts_on.to_string(),
                ends_on: None,
                paused_on: None,
                sort_order,
                created_at: NOW.to_string(),
                updated_at: NOW.to_string(),
            });
        }
    }

    fn backend(message: &str) -> RepoError {
        RepoError::Backend(message.to_string())
    }

    #[async_trait::async_trait(?Send)]
    impl RuleRepo for FakeRuleRepo {
        async fn list_rules(&self, user_id: &str) -> Result<Vec<RuleRow>, RepoError> {
            let state = self.state.lock().unwrap();
            let mut rows: Vec<RuleRow> = state
                .rules
                .iter()
                .filter(|rule| rule.user_id == user_id)
                .cloned()
                .collect();
            rows.sort_by(|left, right| {
                left.sort_order
                    .cmp(&right.sort_order)
                    .then_with(|| left.name.cmp(&right.name))
            });
            Ok(rows)
        }

        async fn get_rule(&self, user_id: &str, id: &str) -> Result<Option<RuleRow>, RepoError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .rules
                .iter()
                .find(|rule| rule.user_id == user_id && rule.id == id)
                .cloned())
        }

        async fn count_rules(&self, user_id: &str) -> Result<i64, RepoError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .rules
                .iter()
                .filter(|rule| rule.user_id == user_id)
                .count() as i64)
        }

        async fn insert_rule(
            &self,
            rule: &NewRuleRow,
            now_rfc3339: &str,
        ) -> Result<RuleRow, RepoError> {
            let mut state = self.state.lock().unwrap();
            state.next_rule += 1;
            let id = format!("rule-{}", state.next_rule);
            let row = RuleRow {
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
            };
            state.rules.push(row.clone());
            Ok(row)
        }

        async fn update_rule(&self, rule: &RuleRow, now_rfc3339: &str) -> Result<(), RepoError> {
            let mut state = self.state.lock().unwrap();
            let Some(slot) = state.rules.iter_mut().find(|row| row.id == rule.id) else {
                return Err(backend("rule missing"));
            };
            *slot = rule.clone();
            slot.updated_at = now_rfc3339.to_string();
            Ok(())
        }

        async fn set_sort_order(
            &self,
            user_id: &str,
            id: &str,
            sort_order: i64,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut state = self.state.lock().unwrap();
            let Some(slot) = state
                .rules
                .iter_mut()
                .find(|row| row.user_id == user_id && row.id == id)
            else {
                return Err(backend("rule missing"));
            };
            slot.sort_order = sort_order;
            slot.updated_at = now_rfc3339.to_string();
            Ok(())
        }

        async fn list_revisions(&self, rule_id: &str) -> Result<Vec<RevisionRow>, RepoError> {
            let state = self.state.lock().unwrap();
            let mut rows: Vec<RevisionRow> = state
                .revisions
                .iter()
                .filter(|revision| revision.rule_id == rule_id)
                .cloned()
                .collect();
            rows.sort_by(|left, right| {
                (&left.effective_from, &left.created_at, &left.id).cmp(&(
                    &right.effective_from,
                    &right.created_at,
                    &right.id,
                ))
            });
            Ok(rows)
        }

        async fn insert_revision(
            &self,
            rule_id: &str,
            effective_from: &str,
            params_json: &str,
            now_rfc3339: &str,
        ) -> Result<(), RepoError> {
            let mut state = self.state.lock().unwrap();
            state.next_revision += 1;
            let id = format!("rev-{}", state.next_revision);
            state.revisions.push(RevisionRow {
                id,
                rule_id: rule_id.to_string(),
                effective_from: effective_from.to_string(),
                params_json: params_json.to_string(),
                created_at: now_rfc3339.to_string(),
            });
            Ok(())
        }

        async fn list_pauses(&self, user_id: &str) -> Result<Vec<PauseRow>, RepoError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .pauses
                .iter()
                .filter(|pause| pause.user_id == user_id)
                .cloned()
                .collect())
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
            let mut state = self.state.lock().unwrap();
            state.next_pause += 1;
            let id = format!("pause-{}", state.next_pause);
            let row = PauseRow {
                id,
                user_id: user_id.to_string(),
                rule_id: rule_id.map(|id| id.to_string()),
                from_date: from_date.to_string(),
                to_date: to_date.to_string(),
                reason: reason.to_string(),
                created_at: now_rfc3339.to_string(),
            };
            state.pauses.push(row.clone());
            Ok(row)
        }

        async fn delete_pause(&self, user_id: &str, id: &str) -> Result<(), RepoError> {
            let mut state = self.state.lock().unwrap();
            state
                .pauses
                .retain(|pause| !(pause.user_id == user_id && pause.id == id));
            Ok(())
        }

        async fn load_category_seconds(
            &self,
            _user_id: &str,
            _from_date: &str,
            _to_date: &str,
        ) -> Result<Vec<CategorySecondsRow>, RepoError> {
            Ok(Vec::new())
        }

        async fn load_sleep_nights(
            &self,
            _user_id: &str,
            _from_date: &str,
            _to_date: &str,
        ) -> Result<Vec<SleepNightRow>, RepoError> {
            Ok(Vec::new())
        }

        async fn load_categories(&self, _user_id: &str) -> Result<Vec<CategoryRow>, RepoError> {
            Ok(self.state.lock().unwrap().categories.clone())
        }
    }

    fn params_json(value: &str) -> serde_json::Value {
        serde_json::from_str(value).expect("valid json")
    }

    // ── seeding ─────────────────────────────────────────────────────────────

    #[test]
    fn seed_inserts_the_eight_rules_with_history()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[
                ("fitness", "Fitness"),
                ("learning", "Learning"),
                ("trip", "Trip"),
                ("travel", "Travel"),
            ]);
            let inserted = ensure_default_rules(&repo, "u-1", AS_OF, NOW).await.expect("seeds");
            assert_eq!(inserted, 8);

            let rules = repo.rules();
            assert_eq!(rules.len(), 8);
            let names: Vec<&str> = rules.iter().map(|rule| rule.name.as_str()).collect();
            assert_eq!(
                names,
                vec![
                    "Fitness",
                    "Productive weekdays",
                    "Learning",
                    "Sleep duration",
                    "Bedtime",
                    "Bedtime spread",
                    "Wake",
                    "Wake spread",
                ]
            );
            for (index, rule) in rules.iter().enumerate() {
                assert_eq!(rule.sort_order, index as i64);
                assert_eq!(rule.starts_on, "2026-06-20", "120 days before as_of");
                assert_eq!(rule.status, "active");
                assert!(rule.ends_on.is_none());
            }

            let fitness = repo.revision_params("rule-1");
            assert_eq!(fitness.len(), 1);
            assert_eq!(fitness[0]["min_days"], 12);
            assert_eq!(fitness[0]["min_minutes"], 60);
            assert_eq!(fitness[0]["window_days"], 60);
            assert_eq!(fitness[0]["categories"]["ids"][0], "cat-fitness");
            assert_eq!(fitness[0]["categories"]["include_children"], true);
            assert_eq!(
                repo.state.lock().unwrap().revisions[0].effective_from,
                "2026-06-20"
            );

            let productive = repo.revision_params("rule-2");
            assert_eq!(productive[0]["categories"], "all_productive");
            assert_eq!(productive[0]["weekdays"], serde_json::json!([0, 1, 2, 3, 4]));
            assert_eq!(productive[0]["exclude_if"]["percent_of_waking"], 50.0);
            assert_eq!(
                productive[0]["exclude_if"]["categories"]["ids"],
                serde_json::json!(["cat-trip", "cat-travel"])
            );

            let sleep = repo.revision_params("rule-4");
            assert_eq!(sleep[0]["min_nights"], 14);
            assert_eq!(sleep[0]["target_hours"], 6.5);

            // A second call never reseeds.
            let again = ensure_default_rules(&repo, "u-1", AS_OF, NOW).await.expect("seeds");
            assert_eq!(again, 0);
            assert_eq!(repo.rules().len(), 8);
            });
    }

    #[test]
    fn seed_skips_rules_whose_category_is_missing()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[
                ("fitness", "Fitness"),
                ("trip", "Trip"),
                ("travel", "Travel"),
            ]);
            let inserted = ensure_default_rules(&repo, "u-1", AS_OF, NOW).await.expect("seeds");
            assert_eq!(inserted, 7);
            let rules = repo.rules();
            assert!(!rules.iter().any(|rule| rule.name == "Learning"));
            // The remaining rules keep compact sort orders in table order.
            for (index, rule) in rules.iter().enumerate() {
                assert_eq!(rule.sort_order, index as i64);
            }
            assert_eq!(rules[2].name, "Sleep duration");

            // With no categories at all, the two category rules are skipped.
            let bare = FakeRuleRepo::default();
            let inserted = ensure_default_rules(&bare, "u-1", AS_OF, NOW).await.expect("seeds");
            assert_eq!(inserted, 6, "only Fitness and Learning need a category");
            assert_eq!(bare.rules().len(), 6);
            assert_eq!(bare.rules()[0].name, "Productive weekdays");
            // Neither trip nor travel exists → no escape hatch.
            assert!(bare.revision_params("rule-1")[0].get("exclude_if").is_none());
            });
    }

    // ── params validation ───────────────────────────────────────────────────

    #[test]
    fn params_reject_out_of_range_and_unknown_values() {
        let catalog = CategoryCatalog::new(vec![Cat::new(
            "cat-fitness",
            None,
            false,
            "fitness",
            "Fitness",
        )]);
        let valid = params_json(
            r#"{"categories":{"ids":["cat-fitness"],"include_children":true},
                "window_days":60,"min_minutes":60,"min_days":12}"#,
        );
        let params = RuleParams::from_value(RuleKind::CategoryDays, valid.clone()).expect("valid");
        assert!(params.validate(&catalog).is_ok());

        for (patch, expected) in [
            (r#"{"window_days":0}"#, "window_days"),
            (r#"{"window_days":367}"#, "window_days"),
            (r#"{"min_days":0}"#, "min_days"),
            (r#"{"min_minutes":0}"#, "min_minutes"),
            (r#"{"categories":{"ids":[],"include_children":true}}"#, "ids must not be empty"),
            (r#"{"categories":{"ids":["nope"],"include_children":true}}"#, "unknown category"),
        ] {
            let mut value = valid.clone();
            let patch = params_json(patch);
            for (key, patch_value) in patch.as_object().expect("object") {
                value[key] = patch_value.clone();
            }
            let params =
                RuleParams::from_value(RuleKind::CategoryDays, value).expect("parses");
            let error = params.validate(&catalog).expect_err("rejects");
            assert!(
                error.to_string().contains(expected),
                "{error} should mention {expected}"
            );
        }

        // An unknown field is rejected outright.
        let error = RuleParams::from_value(
            RuleKind::CategoryDays,
            params_json(
                r#"{"categories":{"ids":["cat-fitness"],"include_children":true},
                    "window_days":60,"min_minutes":60,"min_days":12,"oops":1}"#,
            ),
        )
        .expect_err("rejects unknown fields");
        assert!(error.to_string().contains("unknown field"), "{error}");

        // A `kind` inside the params has to agree with the rule kind.
        let error = RuleParams::from_value(
            RuleKind::CategoryDays,
            params_json(
                r#"{"kind":"category_total","categories":{"ids":["cat-fitness"],"include_children":true},
                    "window_days":60,"min_minutes":60,"min_days":12}"#,
            ),
        )
        .expect_err("rejects a kind mismatch");
        assert!(error.to_string().contains("does not match"), "{error}");
        let ok = RuleParams::from_value(
            RuleKind::CategoryDays,
            params_json(
                r#"{"kind":"category_days","categories":{"ids":["cat-fitness"],"include_children":true},
                    "window_days":60,"min_minutes":60,"min_days":12}"#,
            ),
        )
        .expect("accepts a matching kind");
        assert_eq!(ok.kind(), RuleKind::CategoryDays);

        // Weekdays: deduped and sorted on the way in, empty and out-of-range rejected.
        let params = RuleParams::from_value(
            RuleKind::CategoryDailyAvg,
            params_json(
                r#"{"categories":"all_productive","window_days":60,
                    "weekdays":[4,0,4,2],"target_hours":7.0}"#,
            ),
        )
        .expect("parses");
        assert_eq!(
            params.to_json(),
            r#"{"categories":"all_productive","window_days":60,"weekdays":[0,2,4],"target_hours":7.0}"#
        );
        for bad in [r#""weekdays":[]"#, r#""weekdays":[7]"#] {
            let json = format!(
                r#"{{"categories":"all_productive","window_days":60,{bad},"target_hours":7.0}}"#
            );
            let params = RuleParams::from_value(RuleKind::CategoryDailyAvg, params_json(&json))
                .expect("parses");
            assert!(params.validate(&catalog).is_err(), "{bad} should be rejected");
        }

        // `percent_of_waking` is bounded, and hours must be finite and >= 0.
        let params = RuleParams::from_value(
            RuleKind::CategoryDailyAvg,
            params_json(
                r#"{"categories":"all_productive","window_days":60,"weekdays":[0],
                    "target_hours":7.0,
                    "exclude_if":{"categories":"all_productive","percent_of_waking":0}}"#,
            ),
        )
        .expect("parses");
        assert!(params.validate(&catalog).is_err());
        let params = RuleParams::from_value(
            RuleKind::CategoryTotal,
            params_json(
                r#"{"categories":"all_productive","window_days":7,"comparator":"gte","target_hours":-1}"#,
            ),
        )
        .expect("parses");
        assert!(params.validate(&catalog).is_err());

        // `categories` must be the tag or the object shape.
        assert!(RuleParams::from_value(RuleKind::CategoryTotal, params_json(r#""nope""#)).is_err());
        assert!(RuleParams::from_value(RuleKind::CategoryTotal, params_json(r#"{"categories":5,"window_days":7,"comparator":"gte","target_hours":1}"#)).is_err());
    }

    // ── CRUD ────────────────────────────────────────────────────────────────

    #[test]
    fn create_rule_writes_one_revision_effective_from_starts_on()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[("learning", "Learning")]);
            let input = NewRuleInput {
                name: "Study".to_string(),
                icon: Some("learning".to_string()),
                kind: RuleKind::CategoryTotal,
                starts_on: Some("2026-09-01".to_string()),
                ends_on: None,
                params: params_json(
                    r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                        "window_days":7,"comparator":"gte","target_hours":4.0}"#,
                ),
            };
            let score = create_rule(&repo, "u-1", &input, AS_OF, NOW).await.expect("creates");
            assert_eq!(score.name, "Study");
            assert_eq!(score.phase, "active");
            assert_eq!(score.condition, "≥4h / 7d");
            assert_eq!(score.starts_on, "2026-09-01");

            let revisions = repo.list_revisions(&score.id).await.expect("revisions");
            assert_eq!(revisions.len(), 1);
            assert_eq!(revisions[0].effective_from, "2026-09-01");

            // Defaults: starts_on = as_of when omitted.
            let mut input = input.clone();
            input.starts_on = None;
            input.name = "Second".to_string();
            let score = create_rule(&repo, "u-1", &input, AS_OF, NOW).await.expect("creates");
            assert_eq!(score.starts_on, AS_OF);
            assert_eq!(score.sort_order, 1);

            // An unknown category is a 400.
            let mut bad = input.clone();
            bad.params = params_json(
                r#"{"categories":{"ids":["nope"],"include_children":true},
                    "window_days":7,"comparator":"gte","target_hours":4.0}"#,
            );
            assert!(matches!(
                create_rule(&repo, "u-1", &bad, AS_OF, NOW).await,
                Err(RulesError::Invalid(_))
            ));

            // An empty name is a 400.
            let mut bad = input.clone();
            bad.name = "  ".to_string();
            assert!(matches!(
                create_rule(&repo, "u-1", &bad, AS_OF, NOW).await,
                Err(RulesError::Invalid(_))
            ));

            // ends_on before starts_on is a 400.
            let mut bad = input;
            bad.ends_on = Some("2026-08-01".to_string());
            assert!(matches!(
                create_rule(&repo, "u-1", &bad, AS_OF, NOW).await,
                Err(RulesError::Invalid(_))
            ));
            });
    }

    #[test]
    fn update_forks_a_revision_only_when_params_change()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[("learning", "Learning")]);
            repo.push_rule("rule-1", "Study", RuleKind::CategoryTotal, "2026-09-01");
            repo.insert_revision(
                "rule-1",
                "2026-09-01",
                &params_json(
                    r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                        "window_days":7,"comparator":"gte","target_hours":4.0}"#,
                )
                .to_string(),
                NOW,
            )
            .await
            .expect("revision");

            // A cosmetic edit does not fork params.
            let cosmetic = UpdateRuleInput {
                name: Some("Study more".to_string()),
                icon: Some("book".to_string()),
                sort_order: Some(3),
                ..UpdateRuleInput::default()
            };
            let score = update_rule(&repo, "u-1", "rule-1", &cosmetic, AS_OF, NOW)
                .await
                .expect("updates");
            assert_eq!(score.name, "Study more");
            assert_eq!(score.icon, "book");
            assert_eq!(score.sort_order, 3);
            assert_eq!(repo.list_revisions("rule-1").await.expect("revisions").len(), 1);

            // Re-sending the same params is not a change either.
            let same = UpdateRuleInput {
                params: Some(params_json(
                    r#"{"kind":"category_total","categories":{"ids":["cat-learning"],"include_children":true},
                        "window_days":7,"comparator":"gte","target_hours":4.0}"#,
                )),
                ..UpdateRuleInput::default()
            };
            update_rule(&repo, "u-1", "rule-1", &same, AS_OF, NOW)
                .await
                .expect("updates");
            assert_eq!(repo.list_revisions("rule-1").await.expect("revisions").len(), 1);

            // A real change appends a revision effective from as_of.
            let changed = UpdateRuleInput {
                params: Some(params_json(
                    r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                        "window_days":7,"comparator":"gte","target_hours":6.0}"#,
                )),
                ..UpdateRuleInput::default()
            };
            let score = update_rule(&repo, "u-1", "rule-1", &changed, AS_OF, NOW)
                .await
                .expect("updates");
            assert_eq!(score.target_label, "≥6h / 7d");
            let revisions = repo.list_revisions("rule-1").await.expect("revisions");
            assert_eq!(revisions.len(), 2);
            assert_eq!(revisions[1].effective_from, AS_OF);

            // `kind` cannot change, and an empty patch is a 400.
            let error = update_rule(
                &repo,
                "u-1",
                "rule-1",
                &UpdateRuleInput {
                    kind: Some(RuleKind::CategoryDays),
                    ..UpdateRuleInput::default()
                },
                AS_OF,
                NOW,
            )
            .await
            .expect_err("rejects a kind change");
            assert_eq!(error, RulesError::Invalid("kind cannot change".to_string()));
            assert!(matches!(
                update_rule(&repo, "u-1", "rule-1", &UpdateRuleInput::default(), AS_OF, NOW).await,
                Err(RulesError::Invalid(_))
            ));
            assert_eq!(
                update_rule(&repo, "u-1", "nope", &cosmetic, AS_OF, NOW).await,
                Err(RulesError::RuleNotFound)
            );

            // `ends_on: null` clears the challenge end.
            let clear = UpdateRuleInput {
                ends_on: Some(None),
                ..UpdateRuleInput::default()
            };
            let score = update_rule(&repo, "u-1", "rule-1", &clear, AS_OF, NOW)
                .await
                .expect("updates");
            assert!(score.ends_on.is_none());
            });
    }

    #[test]
    fn pause_resume_archive_and_restore()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[("learning", "Learning")]);
            repo.push_rule("rule-1", "Study", RuleKind::CategoryTotal, "2026-09-01");
            repo.insert_revision(
                "rule-1",
                "2026-09-01",
                &params_json(
                    r#"{"categories":{"ids":["cat-learning"],"include_children":true},
                        "window_days":7,"comparator":"gte","target_hours":4.0}"#,
                )
                .to_string(),
                NOW,
            )
            .await
            .expect("revision");

            // Open-ended pause from today.
            let score = pause_rule(
                &repo,
                "u-1",
                "rule-1",
                &PauseRuleInput::default(),
                AS_OF,
                NOW,
            )
            .await
            .expect("pauses");
            assert_eq!(score.phase, "paused");
            assert_eq!(score.paused_on.as_deref(), Some(AS_OF));
            assert!(repo.list_pauses("u-1").await.expect("pauses").is_empty());

            // Pausing twice is a 409.
            assert!(matches!(
                pause_rule(&repo, "u-1", "rule-1", &PauseRuleInput::default(), AS_OF, NOW).await,
                Err(RulesError::Conflict(_))
            ));

            // Resuming on the same day closes nothing.
            let score = resume_rule(&repo, "u-1", "rule-1", AS_OF, NOW).await.expect("resumes");
            assert_eq!(score.phase, "active");
            assert!(score.paused_on.is_none());
            assert!(repo.list_pauses("u-1").await.expect("pauses").is_empty());

            // A pause that began in the past closes into a per-rule period.
            {
                let mut state = repo.state.lock().unwrap();
                state.rules[0].status = "paused".to_string();
                state.rules[0].paused_on = Some("2026-10-10".to_string());
            }
            let score = resume_rule(&repo, "u-1", "rule-1", AS_OF, NOW).await.expect("resumes");
            assert_eq!(score.phase, "active");
            let pauses = repo.list_pauses("u-1").await.expect("pauses");
            assert_eq!(pauses.len(), 1);
            assert_eq!(pauses[0].from_date, "2026-10-10");
            assert_eq!(pauses[0].to_date, "2026-10-17");
            assert_eq!(pauses[0].rule_id.as_deref(), Some("rule-1"));

            // A scheduled closed range never touches the status.
            let score = pause_rule(
                &repo,
                "u-1",
                "rule-1",
                &PauseRuleInput {
                    from: Some("2026-11-01".to_string()),
                    to: Some("2026-11-05".to_string()),
                },
                AS_OF,
                NOW,
            )
            .await
            .expect("schedules");
            assert_eq!(score.phase, "active");
            assert_eq!(repo.list_pauses("u-1").await.expect("pauses").len(), 2);
            assert!(matches!(
                pause_rule(
                    &repo,
                    "u-1",
                    "rule-1",
                    &PauseRuleInput {
                        from: Some("2026-11-05".to_string()),
                        to: Some("2026-11-01".to_string()),
                    },
                    AS_OF,
                    NOW,
                )
                .await,
                Err(RulesError::Invalid(_))
            ));
            assert!(matches!(
                pause_rule(
                    &repo,
                    "u-1",
                    "rule-1",
                    &PauseRuleInput {
                        from: Some("2026-11-01".to_string()),
                        to: None,
                    },
                    AS_OF,
                    NOW,
                )
                .await,
                Err(RulesError::Invalid(_))
            ));

            // Archive closes an open pause and is idempotent.
            {
                let mut state = repo.state.lock().unwrap();
                state.rules[0].status = "paused".to_string();
                state.rules[0].paused_on = Some("2026-10-10".to_string());
            }
            let score = set_rule_archived(&repo, "u-1", "rule-1", true, AS_OF, NOW)
                .await
                .expect("archives");
            assert_eq!(score.phase, "archived");
            assert!(score.paused_on.is_none());
            let score = set_rule_archived(&repo, "u-1", "rule-1", true, AS_OF, NOW)
                .await
                .expect("archives again");
            assert_eq!(score.phase, "archived");

            // Restore reactivates but keeps `ends_on`.
            {
                let mut state = repo.state.lock().unwrap();
                state.rules[0].ends_on = Some("2026-09-30".to_string());
            }
            let score = set_rule_archived(&repo, "u-1", "rule-1", false, AS_OF, NOW)
                .await
                .expect("restores");
            assert_eq!(score.phase, "finished", "a restored challenge stays finished");
            assert_eq!(score.ends_on.as_deref(), Some("2026-09-30"));

            // A finished rule cannot be paused open-ended.
            assert!(matches!(
                pause_rule(&repo, "u-1", "rule-1", &PauseRuleInput::default(), AS_OF, NOW).await,
                Err(RulesError::Conflict(_))
            ));
            assert_eq!(
                pause_rule(&repo, "u-1", "nope", &PauseRuleInput::default(), AS_OF, NOW).await,
                Err(RulesError::RuleNotFound)
            );
            });
    }

    #[test]
    fn reorder_needs_the_full_set()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[]);
            repo.push_rule("rule-1", "One", RuleKind::SleepDuration, "2026-09-01");
            repo.push_rule("rule-2", "Two", RuleKind::SleepTiming, "2026-09-01");

            reorder_rules(
                &repo,
                "u-1",
                &ReorderInput {
                    ids: vec!["rule-2".to_string(), "rule-1".to_string()],
                },
                NOW,
            )
            .await
            .expect("reorders");
            let rules = repo.rules();
            assert_eq!(rules[0].id, "rule-2");
            assert_eq!(rules[0].sort_order, 0);
            assert_eq!(rules[1].id, "rule-1");
            assert_eq!(rules[1].sort_order, 1);

            for ids in [
                vec!["rule-1".to_string()],
                vec!["rule-1".to_string(), "rule-1".to_string()],
                vec!["rule-1".to_string(), "foreign".to_string()],
            ] {
                assert!(
                    matches!(
                        reorder_rules(&repo, "u-1", &ReorderInput { ids }, NOW).await,
                        Err(RulesError::Invalid(_))
                    ),
                    "partial or foreign id lists are rejected"
                );
            }
            });
    }

    #[test]
    fn global_pauses_are_created_listed_and_deleted()  {
        pollster::block_on(async {
            let repo = FakeRuleRepo::with_categories(&[]);
            let created = create_global_pause(
                &repo,
                "u-1",
                &NewPauseInput {
                    from: "2026-10-01".to_string(),
                    to: "2026-10-03".to_string(),
                    reason: Some("Trip".to_string()),
                },
                NOW,
            )
            .await
            .expect("creates");
            assert!(created.pause.rule_id.is_none());
            assert_eq!(created.pause.reason, "Trip");

            let listed = list_pauses(&repo, "u-1").await.expect("lists");
            assert_eq!(listed.pauses.len(), 1);
            assert_eq!(listed.pauses[0].from, "2026-10-01");

            assert_eq!(
                delete_pause(&repo, "u-1", "nope").await,
                Err(RulesError::PauseNotFound)
            );
            delete_pause(&repo, "u-1", &created.pause.id).await.expect("deletes");
            assert!(list_pauses(&repo, "u-1").await.expect("lists").pauses.is_empty());

            assert!(matches!(
                create_global_pause(
                    &repo,
                    "u-1",
                    &NewPauseInput {
                        from: "2026-10-03".to_string(),
                        to: "2026-10-01".to_string(),
                        reason: None,
                    },
                    NOW,
                )
                .await,
                Err(RulesError::Invalid(_))
            ));
            });
    }
}


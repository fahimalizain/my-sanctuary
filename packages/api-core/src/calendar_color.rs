//! Paint helpers: map cached calendar events to HTTP views that carry the
//! matched category's color (same `classify` rules as tasks, under
//! [`CalendarScope::Event`]).
//!
//! Kept out of `calendar.rs` so the sync/cache module does not grow paint
//! logic. Pure helpers are unit-tested here; the async loader is for the
//! Worker after `list_events` / `create_event` / `update_event_for_user`.

use std::collections::HashMap;

use serde::Serialize;

use crate::categories::{
    classify, ensure_taxonomy, first_matching_pattern, CalendarScope, CategoriesError,
    CategoryWithPatterns, ClassifyOutcome,
};
use crate::models::{CalendarEvent, GoogleCalendar, TaskCategory, TaskCategoryPattern};
use crate::repo::{TaskCategoryRepo, TaskListRepo};

/// Palette for unmatched events (same family as web `EVENT_COLORS`).
const CALENDAR_FALLBACK_COLORS: [&str; 6] = [
    "#2a5c8a", "#c45a2c", "#7a4a6a", "#3a7a5a", "#8a6a2c", "#4a5c8a",
];

/// HTTP event shape: every [`CalendarEvent`] field (flattened) plus category color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CalendarEventView {
    #[serde(flatten)]
    pub event: CalendarEvent,
    /// Canonical `#rrggbb` (or whatever the category stored). Never empty.
    pub color: String,
}

/// Deterministic hex from `calendar_id` (stable per calendar; empty → `"default"`).
pub fn calendar_fallback_color(calendar_id: &str) -> String {
    let key = if calendar_id.is_empty() {
        "default"
    } else {
        calendar_id
    };
    // djb2-ish / JS-style int32 hash (matches web `hashString` / `colorForCalendar`).
    let mut hash: i32 = 0;
    for ch in key.chars() {
        let c = ch as i32;
        hash = hash.wrapping_shl(5).wrapping_sub(hash).wrapping_add(c);
    }
    let idx = (hash.unsigned_abs() as usize) % CALENDAR_FALLBACK_COLORS.len();
    CALENDAR_FALLBACK_COLORS[idx].to_string()
}

/// Category color only. `None` = caller should use calendar fallback.
///
/// 1. `classify(title, CalendarScope::Event { google_calendar_id }, matchers)`
/// 2. Unique match on a **non-untracked** category → that category's trimmed
///    color; if blank, inherit parent; if still blank → `None`.
/// 3. No match / conflict / untracked category → `None`.
pub fn color_for_event_title(
    title: &str,
    google_calendar_id: &str,
    categories: &[TaskCategory],
    matchers: &[CategoryWithPatterns],
) -> Option<String> {
    match classify(
        title,
        CalendarScope::Event { google_calendar_id },
        matchers,
    ) {
        ClassifyOutcome::Matched { category_id } => {
            color_for_matched_category(&category_id, categories)
        }
        ClassifyOutcome::Untracked { .. } => None,
    }
}

/// Destination and category color for a hand-created calendar event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventCreateTarget {
    /// Local `google_calendars.id` the event should be inserted into.
    pub calendar_id: String,
    /// Category hex painted for that destination, if any.
    pub color_hex: Option<String>,
}

/// Resolves the default destination for a hand-created calendar event.
///
/// Mirrors the task timer rule (`tasks::resolve_target_calendar`): `wanted`
/// is the first non-empty of the matched category's first regex-matching
/// pattern's `google_calendar_id` (a first match without a calendar does not
/// skip ahead to a later matching pattern), the matched category's
/// `google_calendar_id`, and the parent root's `google_calendar_id`. A named
/// but missing or read-only calendar falls back to the user's primary writable
/// calendar — never to the next inheritance slot. Unmatched / conflicting
/// titles also fall back to primary.
///
/// The returned color is resolved under the target calendar's
/// [`CalendarScope::Event`] scope, so it matches the chip after the event
/// lands there (a scoped pattern for another calendar is ignored).
pub fn resolve_event_create_target(
    title: &str,
    calendars: &[GoogleCalendar],
    taxonomy: &PaintTaxonomy,
) -> Option<EventCreateTarget> {
    let category = match classify(title, CalendarScope::Ignore, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == category_id),
        // Unmatched / conflicting titles have no inheritance chain: the
        // primary fallback below still runs.
        ClassifyOutcome::Untracked { .. } => None,
    };
    let matcher = category.and_then(|category| {
        taxonomy
            .matchers
            .iter()
            .find(|matcher| matcher.category_id == category.id)
    });
    // One-level tree: `parent_id` is at most a root.
    let parent = category
        .and_then(|category| category.parent_id.as_deref())
        .and_then(|parent_id| {
            taxonomy
                .categories
                .iter()
                .find(|entry| entry.id == parent_id)
        });
    let wanted = matcher
        .and_then(|matcher| first_matching_pattern(title, CalendarScope::Ignore, matcher))
        .and_then(|pattern| pattern.google_calendar_id.as_deref())
        .or_else(|| category.and_then(|category| category.google_calendar_id.as_deref()))
        .or_else(|| parent.and_then(|parent| parent.google_calendar_id.as_deref()));
    let target = wanted
        // A named-but-missing or read-only calendar never falls through to
        // the next inheritance slot: straight to the user's primary.
        .and_then(|wanted| {
            calendars
                .iter()
                .find(|cal| cal.google_calendar_id == wanted && is_writable(cal))
        })
        .or_else(|| {
            calendars
                .iter()
                .find(|cal| cal.is_primary && is_writable(cal))
        })?;
    let color_hex = color_for_event_title(
        title,
        &target.google_calendar_id,
        &taxonomy.categories,
        &taxonomy.matchers,
    );
    Some(EventCreateTarget {
        calendar_id: target.id.clone(),
        color_hex,
    })
}

/// Owner/writer check shared by the calendar create target rule.
fn is_writable(calendar: &GoogleCalendar) -> bool {
    calendar.access_role == "owner" || calendar.access_role == "writer"
}

/// Map a batch. `google_calendar_id_by_local_id` is local `GoogleCalendar.id`
/// → `GoogleCalendar.google_calendar_id`. Missing calendar → `""` (unscoped
/// patterns still match; scoped ones do not).
///
/// Color precedence:
/// 1. A **stored** `category_id` (stamped at write time) paints that category
///    — own color, else parent, else the calendar fallback. The title is never
///    re-matched: a later pattern change must not repaint a stored row.
/// 2. Unclassified rows (`category_id: None`) keep the legacy behavior:
///    [`color_for_event_title`] → [`classify`] (conflict → untracked → no
///    color), then [`calendar_fallback_color`] on the event's local
///    `calendar_id`.
pub fn paint_events(
    events: impl IntoIterator<Item = CalendarEvent>,
    google_calendar_id_by_local_id: &HashMap<String, String>,
    categories: &[TaskCategory],
    matchers: &[CategoryWithPatterns],
) -> Vec<CalendarEventView> {
    events
        .into_iter()
        .map(|event| {
            let google_cal_id = google_calendar_id_by_local_id
                .get(&event.calendar_id)
                .map(String::as_str)
                .unwrap_or("");
            let stored = event
                .category_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let matched = match stored {
                Some(category_id) => color_for_matched_category(category_id, categories),
                None => {
                    color_for_event_title(&event.title, google_cal_id, categories, matchers)
                }
            };
            let color = matched.unwrap_or_else(|| calendar_fallback_color(&event.calendar_id));
            CalendarEventView { event, color }
        })
        .collect()
}

/// Build matchers the same way `tasks::load_taxonomy` does (category_id,
/// parent_id, patterns). Kept here so calendar does not depend on private
/// task helpers.
pub fn matchers_from(
    categories: &[TaskCategory],
    patterns: Vec<TaskCategoryPattern>,
) -> Vec<CategoryWithPatterns> {
    let mut patterns_by_category: HashMap<String, Vec<TaskCategoryPattern>> = HashMap::new();
    for pattern in patterns {
        patterns_by_category
            .entry(pattern.category_id.clone())
            .or_default()
            .push(pattern);
    }
    let mut matchers = Vec::with_capacity(categories.len());
    for category in categories {
        let patterns = patterns_by_category
            .remove(&category.id)
            .unwrap_or_default();
        matchers.push(CategoryWithPatterns::new(
            category.id.clone(),
            category.parent_id.clone(),
            patterns,
        ));
    }
    matchers
}

/// Categories plus compiled matchers, ready to paint any number of events.
/// Built once per request by [`load_paint_taxonomy`].
#[derive(Debug, Clone)]
pub struct PaintTaxonomy {
    pub categories: Vec<TaskCategory>,
    pub matchers: Vec<CategoryWithPatterns>,
}

impl PaintTaxonomy {
    /// Classifier context for events **on one Google calendar** — the write-time
    /// counterpart of the paint above. Pass the calendar's
    /// `GoogleCalendar::google_calendar_id` (never the local row id).
    pub fn event_ctx<'a>(
        &'a self,
        google_calendar_id: &'a str,
    ) -> crate::calendar_apply::EventClassifyCtx<'a> {
        crate::calendar_apply::EventClassifyCtx {
            google_calendar_id,
            categories: &self.categories,
            matchers: &self.matchers,
        }
    }
}

/// Loads the **write-time** classifier taxonomy: the user's living categories
/// and their compiled matchers, in two concurrent queries — no lists, and
/// **never seeds**.
///
/// Writes must not create taxonomy as a side effect (the paint path's
/// [`load_paint_taxonomy`] owns seeding, on first visit). Callers that already
/// have a [`PaintTaxonomy`] in hand should pass it instead of loading again.
pub async fn load_classify_taxonomy(
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<PaintTaxonomy, CategoriesError> {
    let (categories, patterns) = futures::join!(
        category_repo.list_by_user_id(user_id),
        category_repo.list_patterns_by_user_id(user_id),
    );
    let (categories, patterns) = (categories?, patterns?);
    Ok(PaintTaxonomy {
        matchers: matchers_from(&categories, patterns),
        categories,
    })
}

/// Loads the paint taxonomy. Lists, categories and patterns are read
/// concurrently (one round trip deep). [`ensure_taxonomy`] only writes when
/// lists or categories are empty (first visit); only then are categories and
/// patterns re-read — otherwise the first read is used as-is.
///
/// Taxonomy failure should be returned as Err — worker logs and falls back
/// via [`paint_events_default`] (listing must not 500).
pub async fn load_paint_taxonomy(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<PaintTaxonomy, CategoriesError> {
    let (lists, categories, patterns) = futures::join!(
        list_repo.list_by_user_id(user_id),
        category_repo.list_by_user_id(user_id),
        category_repo.list_patterns_by_user_id(user_id),
    );
    let (lists, mut categories, mut patterns) = (lists?, categories?, patterns?);
    if lists.is_empty() || categories.is_empty() {
        ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
        categories = category_repo.list_by_user_id(user_id).await?;
        patterns = category_repo.list_patterns_by_user_id(user_id).await?;
    }
    let matchers = matchers_from(&categories, patterns);
    Ok(PaintTaxonomy {
        categories,
        matchers,
    })
}

/// Paints `events` with an already-loaded taxonomy. `calendars` supplies the
/// local → Google calendar id map for scoped patterns.
pub fn paint_events_with_taxonomy(
    taxonomy: &PaintTaxonomy,
    calendars: &[GoogleCalendar],
    events: Vec<CalendarEvent>,
) -> Vec<CalendarEventView> {
    let google_calendar_id_by_local_id: HashMap<String, String> = calendars
        .iter()
        .map(|cal| (cal.id.clone(), cal.google_calendar_id.clone()))
        .collect();
    paint_events(
        events,
        &google_calendar_id_by_local_id,
        &taxonomy.categories,
        &taxonomy.matchers,
    )
}

/// Load the taxonomy ([`load_paint_taxonomy`]) and paint. Taxonomy failure
/// should be returned as Err — worker logs and falls back via
/// [`paint_events_default`] (listing must not 500).
pub async fn paint_events_for_user(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    calendars: &[GoogleCalendar],
    events: Vec<CalendarEvent>,
    user_id: &str,
) -> Result<Vec<CalendarEventView>, CategoriesError> {
    let taxonomy = load_paint_taxonomy(list_repo, category_repo, user_id).await?;
    Ok(paint_events_with_taxonomy(&taxonomy, calendars, events))
}

/// Fallback paint when taxonomy load fails — each event gets a stable color
/// from its `calendar_id` (not a single shared default).
pub fn paint_events_default(events: impl IntoIterator<Item = CalendarEvent>) -> Vec<CalendarEventView> {
    events
        .into_iter()
        .map(|event| {
            let color = calendar_fallback_color(&event.calendar_id);
            CalendarEventView { event, color }
        })
        .collect()
}

/// Non-untracked match with a non-empty own or parent color → `Some`.
/// Otherwise `None` (caller uses calendar fallback).
fn color_for_matched_category(category_id: &str, categories: &[TaskCategory]) -> Option<String> {
    let category = categories.iter().find(|c| c.id == category_id)?;
    if category.is_untracked {
        return None;
    }
    let own = category.color.trim();
    if !own.is_empty() {
        return Some(own.to_string());
    }
    if let Some(parent_id) = category.parent_id.as_deref() {
        if let Some(parent) = categories.iter().find(|c| c.id == parent_id) {
            let parent_color = parent.color.trim();
            if !parent_color.is_empty() {
                return Some(parent_color.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TaskCategoryPattern;

    fn living_event(id: &str, calendar_id: &str, title: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.to_string(),
            calendar_id: calendar_id.to_string(),
            google_event_id: format!("g-{id}"),
            google_etag: "e1".to_string(),
            google_updated_at: "2026-08-17T12:00:00Z".to_string(),
            last_synced_at: "2026-08-17T12:00:00Z".to_string(),
            title: title.to_string(),
            description: String::new(),
            start_time: "2026-08-19T09:00:00Z".to_string(),
            end_time: "2026-08-19T10:00:00Z".to_string(),
            recurrence: String::new(),
            task_id: String::new(),
            ical_uid: String::new(),
            sequence: 0,
            status: String::new(),
            recurring_event_id: String::new(),
            original_start: String::new(),
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            is_all_day: false,
            raw_json: String::new(),
            category_id: None,
            is_self_organized: true,
            event_type: String::new(),
            created_at: "2026-08-17T12:00:00Z".to_string(),
            updated_at: "2026-08-17T12:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    /// [`living_event`] with a stored (stamped) leaf category.
    fn living_event_stamped(
        id: &str,
        calendar_id: &str,
        title: &str,
        category_id: Option<&str>,
    ) -> CalendarEvent {
        CalendarEvent {
            category_id: category_id.map(str::to_string),
            ..living_event(id, calendar_id, title)
        }
    }

    fn category(
        id: &str,
        parent_id: Option<&str>,
        color: &str,
        is_untracked: bool,
    ) -> TaskCategory {
        TaskCategory {
            id: id.to_string(),
            user_id: "u1".to_string(),
            list_id: if parent_id.is_none() && !is_untracked {
                Some("list-1".to_string())
            } else {
                None
            },
            parent_id: parent_id.map(str::to_string),
            title: id.to_string(),
            slug: id.to_string(),
            color: color.to_string(),
            is_productive: false,
            google_calendar_id: None,
            sort_order: 0,
            is_untracked,
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
            deleted_at: None,
        }
    }

    fn pattern(
        category_id: &str,
        regex: &str,
        google_calendar_id: Option<&str>,
    ) -> TaskCategoryPattern {
        TaskCategoryPattern {
            id: format!("p-{category_id}-{regex}"),
            category_id: category_id.to_string(),
            regex: regex.to_string(),
            google_calendar_id: google_calendar_id.map(str::to_string),
            sort_order: 0,
            created_at: "2026-08-18T00:00:00Z".to_string(),
            updated_at: "2026-08-18T00:00:00Z".to_string(),
        }
    }

    fn matcher(
        category_id: &str,
        parent_id: Option<&str>,
        patterns: Vec<TaskCategoryPattern>,
    ) -> CategoryWithPatterns {
        CategoryWithPatterns::new(
            category_id.to_string(),
            parent_id.map(str::to_string),
            patterns,
        )
    }

    fn google_calendar(
        id: &str,
        google_id: &str,
        is_primary: bool,
        access_role: &str,
    ) -> GoogleCalendar {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "user_id": "u1",
            "google_calendar_id": google_id,
            "summary": id,
            "time_zone": "UTC",
            "is_primary": is_primary,
            "access_role": access_role,
            "sync_enabled": true,
            "created_at": "2026-08-18T00:00:00Z",
            "updated_at": "2026-08-18T00:00:00Z"
        }))
        .expect("valid GoogleCalendar test fixture")
    }

    #[test]
    fn create_target_prefers_pattern_calendar_over_category_and_parent() {
        let mut parent = category("family", None, "#8e24aa", false);
        parent.google_calendar_id = Some("family@x.com".to_string());
        let mut child = category("work", Some("family"), "#4285f4", false);
        child.google_calendar_id = Some("work@x.com".to_string());
        let categories = vec![parent, child];
        let matchers = vec![
            matcher("family", None, vec![pattern("family", "^Never$", None)]),
            matcher(
                "work",
                Some("family"),
                vec![pattern("work", "^Work$", Some("deep@x.com"))],
            ),
        ];
        let calendars = vec![
            google_calendar("cal-primary", "me@x.com", true, "owner"),
            google_calendar("cal-family", "family@x.com", false, "writer"),
            google_calendar("cal-work", "work@x.com", false, "owner"),
            google_calendar("cal-deep", "deep@x.com", false, "owner"),
        ];
        let taxonomy = PaintTaxonomy {
            categories,
            matchers,
        };

        let target = resolve_event_create_target("Work", &calendars, &taxonomy)
            .expect("primary fallback exists");
        assert_eq!(target.calendar_id, "cal-deep");
        assert_eq!(target.color_hex.as_deref(), Some("#4285f4"));
    }

    #[test]
    fn create_target_missing_or_read_only_named_calendar_goes_primary() {
        let mut parent = category("family", None, "#8e24aa", false);
        parent.google_calendar_id = Some("family@x.com".to_string());
        let mut child = category("work", Some("family"), "#4285f4", false);
        child.google_calendar_id = Some("work@x.com".to_string());
        let categories = vec![parent, child];
        let matchers = vec![
            matcher("family", None, vec![pattern("family", "^Never$", None)]),
            matcher(
                "work",
                Some("family"),
                vec![pattern("work", "^Work$", Some("gone@x.com"))],
            ),
        ];
        let calendars = vec![
            google_calendar("cal-primary", "me@x.com", true, "owner"),
            google_calendar("cal-family", "family@x.com", false, "writer"),
            google_calendar("cal-work", "work@x.com", false, "reader"),
        ];
        let taxonomy = PaintTaxonomy {
            categories,
            matchers,
        };

        // The pattern names a calendar that is missing: primary, never the
        // category or parent slot.
        let target = resolve_event_create_target("Work", &calendars, &taxonomy)
            .expect("primary fallback exists");
        assert_eq!(target.calendar_id, "cal-primary");
    }

    #[test]
    fn create_target_uses_first_matching_pattern_without_skipping_to_a_later_calendar() {
        let mut parent = category("family", None, "#8e24aa", false);
        parent.google_calendar_id = Some("family@x.com".to_string());
        let mut child = category("work", Some("family"), "#4285f4", false);
        child.google_calendar_id = Some("work@x.com".to_string());
        let categories = vec![parent, child];
        let matchers = vec![
            matcher("family", None, vec![pattern("family", "^Never$", None)]),
            matcher(
                "work",
                Some("family"),
                vec![
                    pattern("work", "^Work$", None),
                    pattern("work", "^Work [|] Deep$", Some("deep@x.com")),
                ],
            ),
        ];
        let calendars = vec![
            google_calendar("cal-primary", "me@x.com", true, "owner"),
            google_calendar("cal-work", "work@x.com", false, "owner"),
            google_calendar("cal-deep", "deep@x.com", false, "owner"),
        ];
        let taxonomy = PaintTaxonomy {
            categories,
            matchers,
        };

        // The first matching pattern has no calendar; that does not skip to
        // the later matching pattern. The category slot wins here.
        let target = resolve_event_create_target("Work", &calendars, &taxonomy)
            .expect("primary fallback exists");
        assert_eq!(target.calendar_id, "cal-work");
        assert_eq!(target.color_hex.as_deref(), Some("#4285f4"));
    }

    #[test]
    fn create_target_child_without_category_calendar_inherits_parent() {
        let mut parent = category("family", None, "#8e24aa", false);
        parent.google_calendar_id = Some("family@x.com".to_string());
        let mut child = category("chores", Some("family"), "#4285f4", false);
        child.google_calendar_id = None;
        let categories = vec![parent, child];
        let matchers = vec![
            matcher("family", None, vec![pattern("family", "^Never$", None)]),
            matcher(
                "chores",
                Some("family"),
                vec![pattern("chores", "^Chores$", None)],
            ),
        ];
        let calendars = vec![
            google_calendar("cal-primary", "me@x.com", true, "owner"),
            google_calendar("cal-family", "family@x.com", false, "owner"),
        ];
        let taxonomy = PaintTaxonomy {
            categories,
            matchers,
        };

        let target = resolve_event_create_target("Chores", &calendars, &taxonomy)
            .expect("primary fallback exists");
        assert_eq!(target.calendar_id, "cal-family");
        assert_eq!(target.color_hex.as_deref(), Some("#4285f4"));
    }

    #[test]
    fn create_target_untracked_falls_back_to_primary() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher(
                "work",
                None,
                vec![pattern("work", "^Work$", Some("work@x.com"))],
            ),
            matcher("untracked", None, vec![pattern("untracked", ".*", None)]),
        ];
        let calendars = vec![
            google_calendar("cal-primary", "me@x.com", true, "owner"),
            google_calendar("cal-work", "work@x.com", false, "owner"),
        ];
        let taxonomy = PaintTaxonomy {
            categories,
            matchers,
        };

        let target = resolve_event_create_target("Lunch", &calendars, &taxonomy)
            .expect("primary fallback exists");
        assert_eq!(target.calendar_id, "cal-primary");
        assert_eq!(target.color_hex, None);
    }

    #[test]
    fn matched_work_title_uses_work_color() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        assert_eq!(
            color_for_event_title("Work", "any@x.com", &categories, &matchers),
            Some("#4285f4".to_string())
        );
    }

    #[test]
    fn unrelated_title_returns_none_for_calendar_fallback() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        assert_eq!(
            color_for_event_title("Lunch", "any@x.com", &categories, &matchers),
            None
        );
    }

    #[test]
    fn two_sibling_matches_conflict_returns_none() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("a", Some("work"), "#111111", false),
            category("b", Some("work"), "#222222", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher("work", None, vec![pattern("work", "^Never$", None)]),
            matcher("a", Some("work"), vec![pattern("a", "^Work$", None)]),
            matcher("b", Some("work"), vec![pattern("b", "^Work$", None)]),
        ];
        assert_eq!(
            color_for_event_title("Work", "any@x.com", &categories, &matchers),
            None
        );
        // paint applies calendar fallback for the event's calendar_id
        let mut map = HashMap::new();
        map.insert("cal-conflict".to_string(), "any@x.com".to_string());
        let views = paint_events(
            vec![living_event("e1", "cal-conflict", "Work")],
            &map,
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("cal-conflict"));
    }

    #[test]
    fn child_with_empty_color_inherits_parent() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("coding", Some("work"), "", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher("work", None, vec![pattern("work", "^Work$", None)]),
            matcher(
                "coding",
                Some("work"),
                vec![pattern("coding", "^Coding$", None)],
            ),
        ];
        assert_eq!(
            color_for_event_title("Coding", "any@x.com", &categories, &matchers),
            Some("#4285f4".to_string())
        );
    }

    #[test]
    fn event_scope_filters_scoped_patterns() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("family", None, "#8e24aa", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher(
                "work",
                None,
                vec![pattern("work", "^Work$", Some("work@x.com"))],
            ),
            matcher(
                "family",
                None,
                vec![pattern("family", "^Family$", None)],
            ),
        ];

        // Scoped pattern does not match a different calendar → no category color.
        assert_eq!(
            color_for_event_title("Work", "other@x.com", &categories, &matchers),
            None
        );
        // Same calendar matches.
        assert_eq!(
            color_for_event_title("Work", "work@x.com", &categories, &matchers),
            Some("#4285f4".to_string())
        );
        // Unscoped pattern still matches any calendar.
        assert_eq!(
            color_for_event_title("Family", "other@x.com", &categories, &matchers),
            Some("#8e24aa".to_string())
        );
    }

    #[test]
    fn stored_category_paints_its_color_even_when_the_title_conflicts() {
        // The stored stamp wins: two sibling categories both match "Work" now
        // (a conflict for `classify`), but the row was stamped with `work` at
        // write time and must keep painting it.
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("a", Some("work"), "#111111", false),
            category("b", Some("work"), "#222222", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher("work", None, vec![pattern("work", "^Never$", None)]),
            matcher("a", Some("work"), vec![pattern("a", "^Work$", None)]),
            matcher("b", Some("work"), vec![pattern("b", "^Work$", None)]),
        ];
        let mut map = HashMap::new();
        map.insert("cal-conflict".to_string(), "any@x.com".to_string());

        // Legacy path: the same row without a stamp conflicts → fallback.
        let unstamped = living_event("e0", "cal-conflict", "Work");
        let views = paint_events(
            vec![unstamped],
            &map,
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("cal-conflict"));

        // Stamped row: the stored category's color, no re-match.
        let stamped = living_event_stamped("e1", "cal-conflict", "Work", Some("work"));
        let views = paint_events(vec![stamped], &map, &categories, &matchers);
        assert_eq!(views[0].color, "#4285f4");
    }

    #[test]
    fn stored_category_paints_even_when_the_title_would_match_another_category() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("family", None, "#8e24aa", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher("work", None, vec![pattern("work", "^Work$", None)]),
            matcher("family", None, vec![pattern("family", "^Family$", None)]),
        ];
        let mut map = HashMap::new();
        map.insert("cal-1".to_string(), "any@x.com".to_string());

        // Title now matches `family`, but the row is stamped `work`.
        let stamped = living_event_stamped("e1", "cal-1", "Family", Some("work"));
        let views = paint_events(vec![stamped], &map, &categories, &matchers);
        assert_eq!(views[0].color, "#4285f4", "stored stamp beats the title");

        // A stored id whose row is gone / blank falls back to the calendar.
        let stale = living_event_stamped("e2", "cal-1", "Family", Some("ghost"));
        let views = paint_events(vec![stale], &map, &categories, &matchers);
        assert_eq!(views[0].color, calendar_fallback_color("cal-1"));

        // Empty string is "unclassified", not a category id.
        let blank = living_event_stamped("e3", "cal-1", "Family", Some(""));
        let views = paint_events(vec![blank], &map, &categories, &matchers);
        assert_eq!(
            views[0].color, "#8e24aa",
            "blank stamp falls back to the title's unique match"
        );
    }

    #[test]
    fn unclassified_rows_still_use_classify_conflict_fallback() {
        // The unstamped (pre-backfill) path keeps today's behavior exactly:
        // conflict → untracked → calendar fallback.
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("a", Some("work"), "#111111", false),
            category("b", Some("work"), "#222222", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![
            matcher("work", None, vec![pattern("work", "^Never$", None)]),
            matcher("a", Some("work"), vec![pattern("a", "^Work$", None)]),
            matcher("b", Some("work"), vec![pattern("b", "^Work$", None)]),
        ];
        let mut map = HashMap::new();
        map.insert("cal-1".to_string(), "any@x.com".to_string());

        let conflict = living_event("e1", "cal-1", "Work");
        let views = paint_events(vec![conflict], &map, &categories, &matchers);
        assert_eq!(views[0].color, calendar_fallback_color("cal-1"));

        // …and a unique match still paints the matched category.
        let unique_categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let unique_matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        let unique = living_event("e2", "cal-1", "Work");
        let views = paint_events(vec![unique], &map, &unique_categories, &unique_matchers);
        assert_eq!(views[0].color, "#4285f4");
    }

    #[test]
    fn paint_events_copies_fields_and_sets_color() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        let mut map = HashMap::new();
        map.insert("local-cal".to_string(), "work@x.com".to_string());

        let event = living_event("e1", "local-cal", "Work");
        let views = paint_events(vec![event.clone()], &map, &categories, &matchers);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].event, event);
        assert_eq!(views[0].color, "#4285f4");

        // Unmatched event color == calendar_fallback_color(calendar_id)
        let lunch = living_event("e2", "local-cal", "Lunch");
        let views = paint_events(vec![lunch], &map, &categories, &matchers);
        assert_eq!(views[0].color, calendar_fallback_color("local-cal"));
    }

    #[test]
    fn unmatched_events_on_different_calendars_get_different_colors() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "", true),
        ];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        let mut map = HashMap::new();
        map.insert("cal-a".to_string(), "a@x.com".to_string());
        map.insert("cal-b".to_string(), "b@x.com".to_string());

        let views = paint_events(
            vec![
                living_event("e1", "cal-a", "Lunch"),
                living_event("e2", "cal-b", "Dinner"),
            ],
            &map,
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("cal-a"));
        assert_eq!(views[1].color, calendar_fallback_color("cal-b"));
        assert_ne!(views[0].color, views[1].color);
        assert!(!views[0].color.is_empty());
        assert!(!views[1].color.is_empty());
    }

    #[test]
    fn missing_local_calendar_still_returns_non_empty_color() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "#9e69af", true),
        ];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        // Empty map → google_calendar_id is ""; unscoped Work still matches.
        let views = paint_events(
            vec![living_event("e1", "unknown-cal", "Work")],
            &HashMap::new(),
            &categories,
            &matchers,
        );
        assert!(!views[0].color.is_empty());
        assert_eq!(views[0].color, "#4285f4");

        // Unrelated title with missing calendar → calendar fallback on local id.
        let views = paint_events(
            vec![living_event("e2", "unknown-cal", "Lunch")],
            &HashMap::new(),
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("unknown-cal"));
    }

    #[test]
    fn untracked_missing_or_empty_uses_calendar_fallback() {
        // No untracked category at all.
        let categories = vec![category("work", None, "#4285f4", false)];
        let matchers = vec![matcher(
            "work",
            None,
            vec![pattern("work", "^Work$", None)],
        )];
        assert_eq!(
            color_for_event_title("Lunch", "any@x.com", &categories, &matchers),
            None
        );
        let mut map = HashMap::new();
        map.insert("the-cal-id".to_string(), "any@x.com".to_string());
        let views = paint_events(
            vec![living_event("e1", "the-cal-id", "Lunch")],
            &map,
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("the-cal-id"));

        // Empty untracked color — still calendar fallback, not cyan default.
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("untracked", None, "", true),
        ];
        assert_eq!(
            color_for_event_title("Lunch", "any@x.com", &categories, &matchers),
            None
        );
        let views = paint_events(
            vec![living_event("e2", "the-cal-id", "Lunch")],
            &map,
            &categories,
            &matchers,
        );
        assert_eq!(views[0].color, calendar_fallback_color("the-cal-id"));
        assert_ne!(views[0].color, "#039be5");
    }

    #[test]
    fn paint_events_default_uses_calendar_fallback_per_calendar() {
        let views = paint_events_default(vec![
            living_event("e1", "cal-a", "Lunch"),
            living_event("e2", "cal-b", "Dinner"),
        ]);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].color, calendar_fallback_color("cal-a"));
        assert_eq!(views[1].color, calendar_fallback_color("cal-b"));
        assert_ne!(views[0].color, views[1].color);
        assert_ne!(views[0].color, "#039be5");
        assert_ne!(views[1].color, "#039be5");
    }

    #[test]
    fn calendar_fallback_color_is_stable_and_non_empty() {
        let a = calendar_fallback_color("cal-a");
        let a2 = calendar_fallback_color("cal-a");
        let empty = calendar_fallback_color("");
        let default_key = calendar_fallback_color("default");
        assert_eq!(a, a2);
        assert!(!a.is_empty());
        assert!(a.starts_with('#'));
        assert_eq!(empty, default_key);
        assert!(CALENDAR_FALLBACK_COLORS.contains(&a.as_str()));
    }

    #[test]
    fn calendar_event_view_serde_flatten_exposes_color_and_id() {
        let view = CalendarEventView {
            event: living_event("evt-42", "cal-1", "Meeting"),
            color: "#4285f4".to_string(),
        };
        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["color"], "#4285f4");
        assert_eq!(json["id"], "evt-42");
        assert_eq!(json["title"], "Meeting");
        assert_eq!(json["calendar_id"], "cal-1");
        // Flattened — no nested "event" object.
        assert!(json.get("event").is_none());
    }

    #[test]
    fn matchers_from_groups_patterns_by_category() {
        let categories = vec![
            category("work", None, "#4285f4", false),
            category("fitness", None, "#f4511e", false),
        ];
        let patterns = vec![
            pattern("work", "^Work$", None),
            pattern("work", "^.* [|] Work$", None),
            pattern("fitness", "^Fitness$", None),
        ];
        let matchers = matchers_from(&categories, patterns);
        assert_eq!(matchers.len(), 2);
        assert_eq!(matchers[0].category_id, "work");
        assert_eq!(matchers[0].patterns.len(), 2);
        assert_eq!(matchers[1].category_id, "fitness");
        assert_eq!(matchers[1].patterns.len(), 1);
    }
}

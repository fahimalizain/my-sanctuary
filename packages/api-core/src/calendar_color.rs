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
    classify, ensure_taxonomy, CalendarScope, CategoriesError, CategoryWithPatterns,
    ClassifyOutcome,
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

/// Map a batch. `google_calendar_id_by_local_id` is local `GoogleCalendar.id`
/// → `GoogleCalendar.google_calendar_id`. Missing calendar → `""` (unscoped
/// patterns still match; scoped ones do not).
///
/// Unmatched / blank category color → [`calendar_fallback_color`] on the
/// event's local `calendar_id`.
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
            let color = color_for_event_title(&event.title, google_cal_id, categories, matchers)
                .unwrap_or_else(|| calendar_fallback_color(&event.calendar_id));
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
        matchers.push(CategoryWithPatterns {
            category_id: category.id.clone(),
            parent_id: category.parent_id.clone(),
            patterns,
        });
    }
    matchers
}

/// Load lists+categories, `ensure_taxonomy`, reload, load patterns, paint.
/// Taxonomy failure should be returned as Err — worker logs and falls back
/// via [`paint_events_default`] (listing must not 500).
pub async fn paint_events_for_user(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    calendars: &[GoogleCalendar],
    events: Vec<CalendarEvent>,
    user_id: &str,
) -> Result<Vec<CalendarEventView>, CategoriesError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;

    let categories = category_repo.list_by_user_id(user_id).await?;
    let patterns = category_repo.list_patterns_by_user_id(user_id).await?;
    let matchers = matchers_from(&categories, patterns);

    let google_calendar_id_by_local_id: HashMap<String, String> = calendars
        .iter()
        .map(|cal| (cal.id.clone(), cal.google_calendar_id.clone()))
        .collect();

    Ok(paint_events(
        events,
        &google_calendar_id_by_local_id,
        &categories,
        &matchers,
    ))
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
            created_at: "2026-08-17T12:00:00Z".to_string(),
            updated_at: "2026-08-17T12:00:00Z".to_string(),
            deleted_at: None,
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
        CategoryWithPatterns {
            category_id: category_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            patterns,
        }
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

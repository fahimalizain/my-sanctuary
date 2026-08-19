//! Task category service: a one-level category forest with regex patterns,
//! plus the title matcher.
//!
//! Pure Rust and unit-testable: persistence goes through [`TaskCategoryRepo`]
//! (faked with in-memory impls in tests). The Worker layers session checks on
//! top (`apps/worker/src/categories.rs`).
//!
//! Domain rules (locked):
//! - One-level tree: a category is a root (`parent_id` NULL) or a child of a
//!   root. Grandchildren are rejected (400).
//! - `list_id` is meaningful only on roots. Children are stored with
//!   `list_id` NULL and inherit the parent's list on read; creating/updating a
//!   child with a non-null `list_id` is a 400.
//! - Every non-`untracked` root must have a `list_id` (400 otherwise). The
//!   column stays nullable in SQLite.
//! - `untracked` is the only root allowed `list_id` NULL. It is seeded
//!   (`is_untracked = 1`, slug `untracked`), has no patterns, and cannot be
//!   deleted (409) or mutated.
//! - Deleting a category with living children is a 409; otherwise patterns
//!   are hard-deleted and the category soft-deleted.
//! - Pattern writes must compile, be non-empty, and ≤ 256 chars (400
//!   otherwise). Stored regexes that fail to compile are skipped on read by
//!   [`classify`].
//!
//! See [`classify`] for the title-matching reduction rules.

use std::collections::HashMap;

use regex::Regex;
use thiserror::Error;

use crate::lists::SEED_LISTS;
use crate::models::{
    NewTaskCategory, NewTaskCategoryInput, NewTaskCategoryPattern, TaskCategory,
    TaskCategoryPattern, TaskList, UpdateTaskCategory,
};
use crate::repo::{RepoError, TaskCategoryRepo, TaskListRepo};

/// Maximum length of a stored pattern regex (in chars).
pub const MAX_PATTERN_LEN: usize = 256;
/// `sort_order` for the seeded `untracked` sink, so it sorts after the
/// 0..3 root seed.
const UNTRACKED_SORT_ORDER: i64 = 100;

/// Errors produced by the categories service.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CategoriesError {
    #[error("{0}")]
    Invalid(String),
    #[error("category not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// Response envelope for `GET /api/categories`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CategoriesResponse {
    pub categories: Vec<CategoryView>,
}

/// Response envelope for `POST /api/categories` and `PATCH /api/categories/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CategoryResponse {
    pub category: CategoryView,
}

/// Response envelope for `DELETE /api/categories/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteCategoryResponse {
    pub success: bool,
}

/// HTTP shape of a category: every `task_categories` column, the category's
/// patterns, and the list the category belongs to (`inherited_list_id` — the
/// root's own `list_id`, or the parent root's `list_id` for children;
/// `None` for `untracked`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CategoryView {
    pub id: String,
    pub user_id: String,
    /// Stored column; `None` for children and `untracked`.
    pub list_id: Option<String>,
    pub parent_id: Option<String>,
    pub title: String,
    pub slug: String,
    pub color: String,
    pub is_productive: bool,
    pub google_calendar_id: Option<String>,
    pub google_color_id: Option<String>,
    pub sort_order: i64,
    pub is_untracked: bool,
    pub created_at: String,
    pub updated_at: String,
    pub patterns: Vec<TaskCategoryPattern>,
    pub inherited_list_id: Option<String>,
}

/// One living category plus its stored patterns, the unit of classification.
///
/// Built by the caller (slice 3's task service) from `task_categories` rows
/// and `list_patterns_by_category_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryWithPatterns {
    pub category_id: String,
    pub parent_id: Option<String>,
    pub patterns: Vec<TaskCategoryPattern>,
}

/// Result of classifying a title against the category set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifyOutcome {
    Matched { category_id: String },
    /// No (unique) match — the title lands in the `untracked` sink.
    /// `conflict: true` means several categories matched (a client-side
    /// conflict the user should resolve), `false` that nothing matched.
    Untracked { conflict: bool },
}

/// Classifies a title (task title or event summary) into exactly one
/// category.
///
/// Reduction rules (locked):
/// 1. Every stored pattern is compiled; invalid stored regexes are skipped.
/// 2. A pattern with `google_calendar_id` set only applies when the caller
///    supplies a matching event calendar id. When `event_google_calendar_id`
///    is `None` (tasks), scoped patterns are skipped entirely.
/// 3. All matching categories are collected (a category matches when any of
///    its patterns matches).
/// 4. If a child and its own parent both match, the parent is dropped — that
///    is NOT a conflict.
/// 5. After reduction: 0 matches → `Untracked { conflict: false }`; 1 match →
///    that category; 2+ → `Untracked { conflict: true }`.
///
/// Computes the reduced match set [`classify`] decides on: every category
/// whose patterns match the title, minus parents beaten by their own
/// children (rule 4), in input order.
pub(crate) fn reduced_matches<'a>(
    title: &str,
    event_google_calendar_id: Option<&str>,
    categories: &'a [CategoryWithPatterns],
) -> Vec<&'a CategoryWithPatterns> {
    let mut matched: Vec<&CategoryWithPatterns> = Vec::new();
    for category in categories {
        let matches = category.patterns.iter().any(|pattern| {
            // Calendar-scoped patterns only apply to that calendar's events;
            // task titles (None) never match them.
            if let Some(scoped) = pattern.google_calendar_id.as_deref() {
                if event_google_calendar_id != Some(scoped) {
                    return false;
                }
            }
            match Regex::new(pattern.regex.trim()) {
                Ok(regex) => regex.is_match(title),
                Err(_) => false, // skip invalid stored regexes on read
            }
        });
        if matches {
            matched.push(category);
        }
    }

    // A child beating its own parent is not a conflict: drop the parents
    // whose children are already in the matching set.
    let parent_ids: std::collections::HashSet<&str> = matched
        .iter()
        .filter_map(|category| category.parent_id.as_deref())
        .collect();
    matched.retain(|category| !parent_ids.contains(category.category_id.as_str()));
    matched
}

/// Classifies a title (task title or event summary) into exactly one
/// category.
///
/// Reduction rules (locked):
/// 1. Every stored pattern is compiled; invalid stored regexes are skipped.
/// 2. A pattern with `google_calendar_id` set only applies when the caller
///    supplies a matching event calendar id. When `event_google_calendar_id`
///    is `None` (tasks), scoped patterns are skipped entirely.
/// 3. All matching categories are collected (a category matches when any of
///    its patterns matches).
/// 4. If a child and its own parent both match, the parent is dropped — that
///    is NOT a conflict.
/// 5. After reduction: 0 matches → `Untracked { conflict: false }`; 1 match →
///    that category; 2+ → `Untracked { conflict: true }`.
pub fn classify(
    title: &str,
    event_google_calendar_id: Option<&str>,
    categories: &[CategoryWithPatterns],
) -> ClassifyOutcome {
    let matched = reduced_matches(title, event_google_calendar_id, categories);
    match matched.len() {
        0 => ClassifyOutcome::Untracked { conflict: false },
        1 => ClassifyOutcome::Matched {
            category_id: matched[0].category_id.clone(),
        },
        _ => ClassifyOutcome::Untracked { conflict: true },
    }
}

/// Reduced match set of [`classify`]: the category ids that would match
/// after the child-beats-parent rule, in input order. Len 0 = no match,
/// 1 = unique match, 2+ = conflict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClassifyDetail {
    pub matched: Vec<String>,
}

/// Classifies a title into its reduced match set, naming every matching
/// category id (so callers can report a conflict's categories).
///
/// Same reduction rules as [`classify`]; this is [`reduced_matches`] as
/// owned ids.
pub fn classify_detailed(
    title: &str,
    event_google_calendar_id: Option<&str>,
    categories: &[CategoryWithPatterns],
) -> ClassifyDetail {
    ClassifyDetail {
        matched: reduced_matches(title, event_google_calendar_id, categories)
            .into_iter()
            .map(|category| category.category_id.clone())
            .collect(),
    }
}

/// Idempotent first-visit taxonomy seed.
///
/// 1. When `lists` is empty, the four [`SEED_LISTS`] are inserted (each as its
///    own row, minted by the list repo), then re-fetched.
/// 2. When `categories` is empty, the four seed roots are inserted — each
///    bound to the seeded list with the same name (`list_id` = that list's id,
///    `is_productive` for Work/Fitness only, color copied from the list) —
///    plus two patterns each (`^{Name}$` and `^.* [|] {Name}$`, with
///    [`regex::escape`]), and the undeletable `untracked` sink (no patterns,
///    `list_id` NULL).
///
/// A second call sees non-empty lists/categories and never duplicates. The
/// seed runs after lists exist: roots need their `list_id`.
pub async fn ensure_taxonomy(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    lists: &[TaskList],
    categories: &[TaskCategory],
    user_id: &str,
) -> Result<(), CategoriesError> {
    let lists = if lists.is_empty() {
        for (name, color, sort_order) in SEED_LISTS {
            list_repo
                .insert(crate::models::NewTaskList {
                    user_id: user_id.to_string(),
                    name: name.to_string(),
                    color: color.to_string(),
                    sort_order,
                })
                .await?;
        }
        list_repo.list_by_user_id(user_id).await?
    } else {
        lists.to_vec()
    };

    if categories.is_empty() {
        for (name, _color, _sort_order) in SEED_LISTS {
            let Some(list) = lists.iter().find(|list| list.name == name) else {
                // A custom list set without a matching seed name (e.g. lists
                // reseeded after the defaults were renamed): skip this root.
                continue;
            };
            let category = category_repo
                .insert(NewTaskCategory {
                    user_id: user_id.to_string(),
                    list_id: Some(list.id.clone()),
                    parent_id: None,
                    title: name.to_string(),
                    slug: slugify(name),
                    color: list.color.clone(),
                    is_productive: matches!(name, "Work" | "Fitness"),
                    google_calendar_id: None,
                    google_color_id: None,
                    sort_order: list.sort_order,
                    is_untracked: false,
                })
                .await?;
            let escaped = regex::escape(name);
            category_repo
                .replace_patterns(
                    &category.id,
                    vec![
                        NewTaskCategoryPattern {
                            regex: format!("^{escaped}$"),
                            google_calendar_id: None,
                        },
                        NewTaskCategoryPattern {
                            regex: format!("^.* [|] {escaped}$"),
                            google_calendar_id: None,
                        },
                    ],
                )
                .await?;
        }
        category_repo
            .insert(NewTaskCategory {
                user_id: user_id.to_string(),
                list_id: None,
                parent_id: None,
                title: "Untracked".to_string(),
                slug: "untracked".to_string(),
                color: String::new(),
                is_productive: false,
                google_calendar_id: None,
                google_color_id: None,
                sort_order: UNTRACKED_SORT_ORDER,
                is_untracked: true,
            })
            .await?;
    }
    Ok(())
}

/// Slugifies a title: lowercase, non-alphanumeric → `-`, collapsed runs,
/// trimmed. Falls back to `"category"` when nothing remains.
pub fn slugify(title: &str) -> String {
    let slug: String = title
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join("-");
    if slug.is_empty() {
        "category".to_string()
    } else {
        slug
    }
}

/// Lists the user's living categories with their patterns and
/// `inherited_list_id` (root: own `list_id`; child: parent root's `list_id`;
/// `untracked`: `None`).
///
/// Ordering is the repo's (`sort_order`, `title`); the frontend groups by
/// list. Unlike `list_lists`, this does NOT seed: the taxonomy is seeded once
/// by the lists path so parallel first-visit fetches can never double-seed.
pub async fn list_categories(
    repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<CategoriesResponse, CategoriesError> {
    let categories = repo.list_by_user_id(user_id).await?;
    // Root `list_id`s by category id, so one-level children can inherit.
    let list_id_by_root: HashMap<&str, &str> = categories
        .iter()
        .filter(|category| category.parent_id.is_none())
        .filter_map(|category| {
            category
                .list_id
                .as_deref()
                .map(|list_id| (category.id.as_str(), list_id))
        })
        .collect();

    let mut views = Vec::with_capacity(categories.len());
    for category in &categories {
        let inherited_list_id = match category.parent_id.as_deref() {
            Some(parent_id) => list_id_by_root
                .get(parent_id)
                .map(|list_id| (*list_id).to_string()),
            None => category.list_id.clone(),
        };
        let patterns = repo.list_patterns_by_category_id(&category.id).await?;
        views.push(to_view(category, inherited_list_id, patterns));
    }
    Ok(CategoriesResponse { categories: views })
}

/// Creates a category (root when `parent_id` is absent, child otherwise).
///
/// Validation (all 400 unless noted):
/// - `title`/`color` must not be empty; `slug` is slugified from `title`
///   when omitted, and must be unique among the user's living categories
///   (Conflict).
/// - `is_untracked` cannot be set via the API (the sink is system-seeded).
/// - A child must not carry a `list_id`; a non-`untracked` root must carry
///   one, and that list must exist and belong to the user (404 otherwise).
/// - The parent must be a living root of the same user (404 when missing;
///   400 when it is itself a child — no grandchildren).
/// - Every pattern must compile, be non-empty, and ≤ [`MAX_PATTERN_LEN`]
///   chars.
pub async fn create_category(
    repo: &dyn TaskCategoryRepo,
    list_repo: &dyn TaskListRepo,
    user_id: &str,
    input: &NewTaskCategoryInput,
) -> Result<CategoryResponse, CategoriesError> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(CategoriesError::Invalid("title must not be empty".to_string()));
    }
    let color = input.color.trim().to_string();
    if color.is_empty() {
        return Err(CategoriesError::Invalid("color must not be empty".to_string()));
    }
    if input.is_untracked == Some(true) {
        return Err(CategoriesError::Invalid(
            "is_untracked can only be set by the system".to_string(),
        ));
    }
    let slug = match input.slug.as_deref() {
        Some(slug) if !slug.trim().is_empty() => slug.trim().to_string(),
        _ => slugify(&title),
    };
    let patterns = normalize_patterns(&input.patterns);
    for pattern in &patterns {
        validate_pattern(pattern)?;
    }
    let google_calendar_id = normalize_optional(input.google_calendar_id.as_deref());
    let google_color_id = normalize_optional(input.google_color_id.as_deref());
    let sort_order = input.sort_order.unwrap_or(0);

    let (parent_id, list_id, parent) = match input.parent_id.as_deref() {
        Some(parent_id_str) => {
            if input.list_id.is_some() {
                return Err(CategoriesError::Invalid(
                    "children cannot have a list_id".to_string(),
                ));
            }
            let parent = repo.get_by_id(parent_id_str).await?;
            let parent = parent
                .filter(|parent| parent.user_id == user_id)
                .ok_or(CategoriesError::NotFound)?;
            if parent.parent_id.is_some() {
                return Err(CategoriesError::Invalid(
                    "cannot create a category under a non-root category".to_string(),
                ));
            }
            (Some(parent_id_str.to_string()), None, Some(parent))
        }
        None => {
            let Some(list_id) = input.list_id.as_deref() else {
                return Err(CategoriesError::Invalid(
                    "root categories must have a list_id".to_string(),
                ));
            };
            // The list must exist and belong to the user; another user's (or
            // a missing) list is a plain 404, never a leak.
            let list = list_repo.get_by_id(list_id).await?;
            let list = list.filter(|list| list.user_id == user_id).ok_or(CategoriesError::NotFound)?;
            (None, Some(list.id.clone()), None)
        }
    };

    // Slug uniqueness among living categories. The DB partial unique index
    // backs this up, but surfacing it as a Conflict beats a raw D1 error.
    let existing = repo.list_by_user_id(user_id).await?;
    if existing.iter().any(|category| category.slug == slug) {
        return Err(CategoriesError::Conflict(format!("slug {slug:?} is already in use")));
    }

    let category = repo
        .insert(NewTaskCategory {
            user_id: user_id.to_string(),
            list_id,
            parent_id,
            title,
            slug,
            color,
            is_productive: input.is_productive.unwrap_or(false),
            google_calendar_id,
            google_color_id,
            sort_order,
            is_untracked: false,
        })
        .await?;
    repo.replace_patterns(&category.id, patterns).await?;

    let patterns = repo.list_patterns_by_category_id(&category.id).await?;
    // Root: own list_id. Child: the (validated) parent root's list_id.
    let inherited_list_id = match category.parent_id.as_ref() {
        Some(_) => parent.as_ref().and_then(|parent| parent.list_id.clone()),
        None => category.list_id.clone(),
    };
    Ok(CategoryResponse {
        category: to_view(&category, inherited_list_id, patterns),
    })
}

/// Updates a category's mutable fields (title, slug, color, is_productive,
/// calendar ids, sort_order, parent, patterns).
///
/// Validation:
/// - Nothing to update → 400. `is_untracked` cannot be set → 400.
/// - Missing, soft-deleted, or another user's category → 404.
/// - The `untracked` sink is immutable → 409.
/// - Reparenting: the new parent must be a living root of the same user
///   (404 when missing, 400 when it is a child, 400 for self-parenting), the
///   category must not already have children (400), and a child can never
///   carry a `list_id` (400). A root moved under a parent is stored as a
///   child — `list_id` NULL (the repo's UPDATE NULLs it).
/// - A root's new `list_id` must exist and belong to the user (404
///   otherwise).
/// - Patterns: same validation as create; `Some` replaces the whole set.
pub async fn update_category(
    repo: &dyn TaskCategoryRepo,
    list_repo: &dyn TaskListRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateTaskCategory,
) -> Result<CategoryResponse, CategoriesError> {
    if updates.title.is_none()
        && updates.slug.is_none()
        && updates.color.is_none()
        && updates.is_productive.is_none()
        && updates.google_calendar_id.is_none()
        && updates.google_color_id.is_none()
        && updates.list_id.is_none()
        && updates.parent_id.is_none()
        && updates.sort_order.is_none()
        && updates.is_untracked.is_none()
        && updates.patterns.is_none()
    {
        return Err(CategoriesError::Invalid("nothing to update".to_string()));
    }
    if updates.is_untracked == Some(true) {
        return Err(CategoriesError::Invalid(
            "is_untracked can only be set by the system".to_string(),
        ));
    }

    let Some(category) = repo.get_by_id(id).await? else {
        return Err(CategoriesError::NotFound);
    };
    if category.user_id != user_id {
        return Err(CategoriesError::NotFound);
    }
    if category.is_untracked {
        return Err(CategoriesError::Conflict(
            "untracked category cannot be modified".to_string(),
        ));
    }

    if let Some(title) = updates.title.as_deref() {
        if title.trim().is_empty() {
            return Err(CategoriesError::Invalid("title must not be empty".to_string()));
        }
    }
    if let Some(color) = updates.color.as_deref() {
        if color.trim().is_empty() {
            return Err(CategoriesError::Invalid("color must not be empty".to_string()));
        }
    }
    if let Some(slug) = updates.slug.as_deref() {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err(CategoriesError::Invalid("slug must not be empty".to_string()));
        }
        let existing = repo.list_by_user_id(user_id).await?;
        if existing.iter().any(|entry| entry.slug == slug && entry.id != id) {
            return Err(CategoriesError::Conflict(format!("slug {slug:?} is already in use")));
        }
    }
    if let Some(patterns) = &updates.patterns {
        for pattern in patterns.iter() {
            validate_pattern(pattern)?;
        }
    }

    if let Some(parent_id_str) = updates.parent_id.as_deref() {
        if parent_id_str == id {
            return Err(CategoriesError::Invalid(
                "cannot parent a category to itself".to_string(),
            ));
        }
        let parent = repo.get_by_id(parent_id_str).await?;
        let parent = parent
            .filter(|parent| parent.user_id == user_id)
            .ok_or(CategoriesError::NotFound)?;
        if parent.parent_id.is_some() {
            return Err(CategoriesError::Invalid(
                "cannot parent a category to a child".to_string(),
            ));
        }
        if repo.count_children(id).await? > 0 {
            return Err(CategoriesError::Invalid(
                "cannot reparent a category that has children".to_string(),
            ));
        }
        if updates.list_id.is_some() {
            return Err(CategoriesError::Invalid(
                "children cannot have a list_id".to_string(),
            ));
        }
    } else if updates.list_id.is_some() {
        // Not moving: only roots may change lists; children never carry one.
        if category.parent_id.is_some() {
            return Err(CategoriesError::Invalid("children cannot have a list_id".to_string()));
        }
        if let Some(list_id) = updates.list_id.as_deref() {
            if list_id.trim().is_empty() {
                return Err(CategoriesError::Invalid("list_id must not be empty".to_string()));
            }
            // The new list must exist and belong to the user; a missing or
            // other-user list is a plain 404, never a leak.
            let list = list_repo.get_by_id(list_id).await?;
            if !list.is_some_and(|list| list.user_id == user_id) {
                return Err(CategoriesError::NotFound);
            }
        }
    }

    let Some(updated) = repo.update(id, updates).await? else {
        // Deleted between the read and the write.
        return Err(CategoriesError::NotFound);
    };
    if let Some(patterns) = &updates.patterns {
        repo.replace_patterns(id, normalize_patterns(patterns)).await?;
    }

    let patterns = repo.list_patterns_by_category_id(id).await?;
    let inherited_list_id = match updated.parent_id.as_deref() {
        Some(parent_id) => repo
            .get_by_id(parent_id)
            .await?
            .and_then(|parent| parent.list_id),
        None => updated.list_id.clone(),
    };
    Ok(CategoryResponse {
        category: to_view(&updated, inherited_list_id, patterns),
    })
}

/// SOFT deletes a category.
///
/// - Missing, soft-deleted, or another user's category → 404.
/// - The `untracked` sink → 409 (undeletable).
/// - Any living children → 409 (delete the tree top-down).
/// - Otherwise patterns are hard-deleted and the row is stamped with
///   `deleted_at = now_rfc3339`; `{"success": true}` is returned.
pub async fn delete_category(
    repo: &dyn TaskCategoryRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
) -> Result<DeleteCategoryResponse, CategoriesError> {
    let Some(category) = repo.get_by_id(id).await? else {
        return Err(CategoriesError::NotFound);
    };
    // Ownership is checked before the guards so another user's category is a
    // plain 404 and never reveals children or the untracked flag.
    if category.user_id != user_id {
        return Err(CategoriesError::NotFound);
    }
    if category.is_untracked {
        return Err(CategoriesError::Conflict(
            "untracked category cannot be deleted".to_string(),
        ));
    }
    if repo.count_children(id).await? > 0 {
        return Err(CategoriesError::Conflict("category has children".to_string()));
    }
    repo.delete_patterns_by_category_id(id).await?;
    repo.soft_delete(id, now_rfc3339).await?;
    Ok(DeleteCategoryResponse { success: true })
}

// ──────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────

fn to_view(
    category: &TaskCategory,
    inherited_list_id: Option<String>,
    patterns: Vec<TaskCategoryPattern>,
) -> CategoryView {
    CategoryView {
        id: category.id.clone(),
        user_id: category.user_id.clone(),
        list_id: category.list_id.clone(),
        parent_id: category.parent_id.clone(),
        title: category.title.clone(),
        slug: category.slug.clone(),
        color: category.color.clone(),
        is_productive: category.is_productive,
        google_calendar_id: category.google_calendar_id.clone(),
        google_color_id: category.google_color_id.clone(),
        sort_order: category.sort_order,
        is_untracked: category.is_untracked,
        created_at: category.created_at.clone(),
        updated_at: category.updated_at.clone(),
        patterns,
        inherited_list_id,
    }
}

/// Trims an optional string, mapping blank values to `None`.
fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Normalizes a pattern set: trimmed regex, blank calendar ids → `None`.
fn normalize_patterns(patterns: &[NewTaskCategoryPattern]) -> Vec<NewTaskCategoryPattern> {
    patterns
        .iter()
        .map(|pattern| NewTaskCategoryPattern {
            regex: pattern.regex.trim().to_string(),
            google_calendar_id: normalize_optional(pattern.google_calendar_id.as_deref()),
        })
        .collect()
}

/// A pattern must compile, be non-empty, and be ≤ [`MAX_PATTERN_LEN`] chars.
fn validate_pattern(pattern: &NewTaskCategoryPattern) -> Result<(), CategoriesError> {
    let regex = pattern.regex.trim();
    if regex.is_empty() {
        return Err(CategoriesError::Invalid("pattern regex must not be empty".to_string()));
    }
    if regex.chars().count() > MAX_PATTERN_LEN {
        return Err(CategoriesError::Invalid(
            "pattern regex must be at most 256 characters".to_string(),
        ));
    }
    if Regex::new(regex).is_err() {
        return Err(CategoriesError::Invalid("pattern regex does not compile".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{NewTaskList, TaskList, UpdateTaskList};

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// In-memory `TaskCategoryRepo` mirroring D1 semantics: soft-deleted rows
    /// are filtered from reads, patterns are hard-replaced, and an UPDATE with
    /// a `parent_id` NULLs `list_id` (children never store one).
    struct FakeTaskCategoryRepo {
        stored: Mutex<Vec<TaskCategory>>,
        patterns: Mutex<HashMap<String, Vec<TaskCategoryPattern>>>,
        inserted: Mutex<Vec<NewTaskCategory>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskCategoryRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                patterns: Mutex::new(HashMap::new()),
                inserted: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        fn row(
            id: &str,
            user_id: &str,
            list_id: Option<&str>,
            parent_id: Option<&str>,
            title: &str,
            slug: &str,
            is_productive: bool,
            is_untracked: bool,
            sort_order: i64,
        ) -> TaskCategory {
            TaskCategory {
                id: id.to_string(),
                user_id: user_id.to_string(),
                list_id: list_id.map(str::to_string),
                parent_id: parent_id.map(str::to_string),
                title: title.to_string(),
                slug: slug.to_string(),
                color: "#2a5c8a".to_string(),
                is_productive,
                google_calendar_id: None,
                google_color_id: None,
                sort_order,
                is_untracked,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            }
        }

        /// Seeds the fake with living rows (no patterns).
        fn with(rows: Vec<TaskCategory>) -> Self {
            let repo = Self::new();
            repo.stored.lock().unwrap().extend(rows);
            repo
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskCategoryRepo for FakeTaskCategoryRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
            let mut rows: Vec<TaskCategory> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            // Mirrors TASK_CATEGORY_LIST_BY_USER_ID_SQL.
            rows.sort_by(|a, b| (a.sort_order, &a.title).cmp(&(b.sort_order, &b.title)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, category: NewTaskCategory) -> Result<TaskCategory, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = TaskCategory {
                id: format!("cat-{next}"),
                user_id: category.user_id.clone(),
                list_id: category.list_id.clone(),
                parent_id: category.parent_id.clone(),
                title: category.title.clone(),
                slug: category.slug.clone(),
                color: category.color.clone(),
                is_productive: category.is_productive,
                google_calendar_id: category.google_calendar_id.clone(),
                google_color_id: category.google_color_id.clone(),
                sort_order: category.sort_order,
                is_untracked: category.is_untracked,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.inserted.lock().unwrap().push(category);
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            id: &str,
            updates: &UpdateTaskCategory,
        ) -> Result<Option<TaskCategory>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            else {
                return Ok(None);
            };
            // Raw storage, mirroring the D1 COALESCE update.
            if let Some(title) = &updates.title {
                row.title = title.clone();
            }
            if let Some(slug) = &updates.slug {
                row.slug = slug.clone();
            }
            if let Some(color) = &updates.color {
                row.color = color.clone();
            }
            if let Some(is_productive) = updates.is_productive {
                row.is_productive = is_productive;
            }
            if let Some(google_calendar_id) = &updates.google_calendar_id {
                row.google_calendar_id = Some(google_calendar_id.clone());
            }
            if let Some(google_color_id) = &updates.google_color_id {
                row.google_color_id = Some(google_color_id.clone());
            }
            if let Some(sort_order) = updates.sort_order {
                row.sort_order = sort_order;
            }
            // Mirrors TASK_CATEGORY_UPDATE_SQL: a new parent NULLs list_id.
            if let Some(parent_id) = &updates.parent_id {
                row.parent_id = Some(parent_id.clone());
                row.list_id = None;
            }
            if row.parent_id.is_none() {
                if let Some(list_id) = &updates.list_id {
                    row.list_id = Some(list_id.clone());
                }
            }
            row.updated_at = "2026-08-18T01:00:00Z".to_string();
            Ok(Some(row.clone()))
        }

        async fn soft_delete(&self, id: &str, now_rfc3339: &str) -> Result<(), RepoError> {
            if let Some(row) = self
                .stored
                .lock()
                .unwrap()
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            {
                row.deleted_at = Some(now_rfc3339.to_string());
                row.updated_at = now_rfc3339.to_string();
            }
            Ok(())
        }

        async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .count() as i64)
        }

        async fn count_children(&self, category_id: &str) -> Result<i64, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.parent_id.as_deref() == Some(category_id) && row.deleted_at.is_none())
                .count() as i64)
        }

        async fn get_untracked(&self, user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.user_id == user_id && row.is_untracked && row.deleted_at.is_none())
                .cloned())
        }

        async fn list_patterns_by_category_id(
            &self,
            category_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            let mut rows = self
                .patterns
                .lock()
                .unwrap()
                .get(category_id)
                .cloned()
                .unwrap_or_default();
            rows.sort_by_key(|row| row.sort_order);
            Ok(rows)
        }

        async fn replace_patterns(
            &self,
            category_id: &str,
            patterns: Vec<NewTaskCategoryPattern>,
        ) -> Result<(), RepoError> {
            let mut stored = self.patterns.lock().unwrap();
            stored.remove(category_id);
            let rows: Vec<TaskCategoryPattern> = patterns
                .into_iter()
                .enumerate()
                .map(|(sort_order, input)| TaskCategoryPattern {
                    id: format!("pat-{category_id}-{sort_order}"),
                    category_id: category_id.to_string(),
                    regex: input.regex,
                    google_calendar_id: input.google_calendar_id,
                    sort_order: sort_order as i64,
                    created_at: "2026-08-18T00:00:00Z".to_string(),
                    updated_at: "2026-08-18T00:00:00Z".to_string(),
                })
                .collect();
            stored.insert(category_id.to_string(), rows);
            Ok(())
        }

        async fn delete_patterns_by_category_id(&self, category_id: &str) -> Result<(), RepoError> {
            self.patterns.lock().unwrap().remove(category_id);
            Ok(())
        }
    }

    /// Minimal in-memory `TaskListRepo` for the seed and ownership tests (the
    /// lists tests keep their own richer fake in `crate::lists`).
    struct FakeTaskListRepo {
        stored: Mutex<Vec<TaskList>>,
        inserted: Mutex<Vec<NewTaskList>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskListRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                inserted: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        /// Seeds the fake with living list rows.
        fn with(rows: Vec<TaskList>) -> Self {
            let repo = Self::new();
            repo.stored.lock().unwrap().extend(rows);
            repo
        }

        fn row(id: &str, user_id: &str, name: &str, sort_order: i64) -> TaskList {
            TaskList {
                id: id.to_string(),
                user_id: user_id.to_string(),
                name: name.to_string(),
                color: "#2a5c8a".to_string(),
                sort_order,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskListRepo for FakeTaskListRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskList>, RepoError> {
            let mut rows: Vec<TaskList> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            rows.sort_by(|a, b| (a.sort_order, &a.name).cmp(&(b.sort_order, &b.name)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TaskList>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, list: NewTaskList) -> Result<TaskList, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = TaskList {
                id: format!("list-{next}"),
                user_id: list.user_id.clone(),
                name: list.name.clone(),
                color: list.color.clone(),
                sort_order: list.sort_order,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.inserted.lock().unwrap().push(list);
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            _id: &str,
            _updates: &UpdateTaskList,
        ) -> Result<Option<TaskList>, RepoError> {
            Ok(None)
        }

        async fn soft_delete(&self, _id: &str, _now_rfc3339: &str) -> Result<(), RepoError> {
            Ok(())
        }

        async fn count_by_user_id(&self, user_id: &str) -> Result<i64, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .count() as i64)
        }

        async fn count_root_categories_for_list(&self, _list_id: &str) -> Result<i64, RepoError> {
            Ok(0)
        }
    }

    // ──────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────

    fn pattern(regex: &str) -> NewTaskCategoryPattern {
        NewTaskCategoryPattern {
            regex: regex.to_string(),
            google_calendar_id: None,
        }
    }

    fn cat_with_patterns(
        category_id: &str,
        parent_id: Option<&str>,
        patterns: &[&str],
    ) -> CategoryWithPatterns {
        CategoryWithPatterns {
            category_id: category_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            patterns: patterns
                .iter()
                .enumerate()
                .map(|(i, regex)| TaskCategoryPattern {
                    id: format!("p-{category_id}-{i}"),
                    category_id: category_id.to_string(),
                    regex: regex.to_string(),
                    google_calendar_id: None,
                    sort_order: i as i64,
                    created_at: "2026-08-18T00:00:00Z".to_string(),
                    updated_at: "2026-08-18T00:00:00Z".to_string(),
                })
                .collect(),
        }
    }

    fn input(title: &str, color: &str) -> NewTaskCategoryInput {
        NewTaskCategoryInput {
            title: title.to_string(),
            slug: None,
            color: color.to_string(),
            is_productive: None,
            google_calendar_id: None,
            google_color_id: None,
            list_id: None,
            parent_id: None,
            sort_order: None,
            is_untracked: None,
            patterns: Vec::new(),
        }
    }

    // ──────────────────────────────────────────
    // Matcher
    // ──────────────────────────────────────────

    #[test]
    fn exact_title_matches_root_seed_pattern() {
        let categories = vec![cat_with_patterns("work", None, &["^Work$", "^.* [|] Work$"])];
        assert_eq!(
            classify("Work", None, &categories),
            ClassifyOutcome::Matched { category_id: "work".to_string() }
        );
        assert_eq!(
            classify("Review Q3 | Work", None, &categories),
            ClassifyOutcome::Matched { category_id: "work".to_string() }
        );
    }

    #[test]
    fn unmatched_title_is_untracked_without_conflict() {
        let categories = vec![cat_with_patterns("work", None, &["^Work$"])];
        assert_eq!(
            classify("asdf", None, &categories),
            ClassifyOutcome::Untracked { conflict: false }
        );
    }

    #[test]
    fn child_beating_own_parent_is_not_a_conflict() {
        let categories = vec![
            cat_with_patterns("work", None, &["^Work$"]),
            cat_with_patterns("coding", Some("work"), &["^Work$"]),
        ];
        assert_eq!(
            classify("Work", None, &categories),
            ClassifyOutcome::Matched { category_id: "coding".to_string() }
        );
    }

    #[test]
    fn two_matching_roots_conflict_into_untracked() {
        let categories = vec![
            cat_with_patterns("work", None, &["^Work$"]),
            cat_with_patterns("fitness", None, &["^Work$"]),
        ];
        assert_eq!(
            classify("Work", None, &categories),
            ClassifyOutcome::Untracked { conflict: true }
        );
    }

    #[test]
    fn two_sibling_matches_conflict_into_untracked() {
        let categories = vec![
            cat_with_patterns("work", None, &["^Never$"]),
            cat_with_patterns("a", Some("work"), &["^Work$"]),
            cat_with_patterns("b", Some("work"), &["^Work$"]),
        ];
        assert_eq!(
            classify("Work", None, &categories),
            ClassifyOutcome::Untracked { conflict: true }
        );
    }

    #[test]
    fn calendar_scoped_pattern_is_skipped_without_calendar_context() {
        let mut scoped = cat_with_patterns("work", None, &["^Work$"]);
        scoped.patterns[0].google_calendar_id = Some("cal-1".to_string());

        assert_eq!(
            classify("Work", None, &[scoped.clone()]),
            ClassifyOutcome::Untracked { conflict: false },
            "task titles (None) never match calendar-scoped patterns"
        );
        assert_eq!(
            classify("Work", Some("other-cal"), &[scoped.clone()]),
            ClassifyOutcome::Untracked { conflict: false },
            "a different calendar does not match"
        );
        assert_eq!(
            classify("Work", Some("cal-1"), &[scoped]),
            ClassifyOutcome::Matched { category_id: "work".to_string() }
        );
    }

    #[test]
    fn invalid_stored_regexes_are_skipped_on_read() {
        let categories = vec![cat_with_patterns("work", None, &["[unclosed", "^Work$"])];
        assert_eq!(
            classify("Work", None, &categories),
            ClassifyOutcome::Matched { category_id: "work".to_string() },
            "the valid sibling pattern still matches"
        );
        let only_bad = vec![cat_with_patterns("work", None, &["("])];
        assert_eq!(
            classify("Work", None, &only_bad),
            ClassifyOutcome::Untracked { conflict: false }
        );
    }

    #[test]
    fn classify_detailed_names_the_unique_match() {
        let categories = vec![cat_with_patterns(
            "work",
            None,
            &["^Work$", "^.* [|] Work$"],
        )];
        assert_eq!(
            classify_detailed("Work", None, &categories),
            ClassifyDetail {
                matched: vec!["work".to_string()]
            }
        );
    }

    #[test]
    fn classify_detailed_no_match_is_empty() {
        let categories = vec![cat_with_patterns("work", None, &["^Work$"])];
        assert_eq!(
            classify_detailed("asdf", None, &categories),
            ClassifyDetail {
                matched: Vec::new()
            }
        );
    }

    #[test]
    fn classify_detailed_two_sibling_matches_name_both_in_input_order() {
        let categories = vec![
            cat_with_patterns("work", None, &["^Never$"]),
            cat_with_patterns("a", Some("work"), &["^Work$"]),
            cat_with_patterns("b", Some("work"), &["^Work$"]),
        ];
        assert_eq!(
            classify_detailed("Work", None, &categories),
            ClassifyDetail {
                matched: vec!["a".to_string(), "b".to_string()]
            }
        );
    }

    #[test]
    fn classify_detailed_child_beats_own_parent() {
        let categories = vec![
            cat_with_patterns("work", None, &["^Work$"]),
            cat_with_patterns("coding", Some("work"), &["^Work$"]),
        ];
        assert_eq!(
            classify_detailed("Work", None, &categories),
            ClassifyDetail {
                matched: vec!["coding".to_string()]
            }
        );
    }

    #[test]
    fn slugify_lowercases_and_hyphenates() {
        assert_eq!(slugify("Work"), "work");
        assert_eq!(slugify("Deep Work & Q3 Review!"), "deep-work-q3-review");
        assert_eq!(slugify("  100%  "), "100");
        assert_eq!(slugify("日本語"), "category", "fallback when nothing remains");
    }

    // ──────────────────────────────────────────
    // Seed
    // ──────────────────────────────────────────

    #[test]
    fn first_visit_seeds_lists_categories_and_patterns() {
        let list_repo = FakeTaskListRepo::new();
        let category_repo = FakeTaskCategoryRepo::new();
        let response = pollster::block_on(crate::list_lists(&list_repo, &category_repo, "u-1"))
            .unwrap();

        assert_eq!(response.lists.len(), 4);
        let names: Vec<&str> = response.lists.iter().map(|list| list.name.as_str()).collect();
        assert_eq!(names, ["Work", "Fitness", "Family", "Personal"]);

        let categories = pollster::block_on(category_repo.list_by_user_id("u-1")).unwrap();
        assert_eq!(categories.len(), 5, "four roots + untracked");

        // Roots bind to the seeded list of the same name and copy its color.
        let work_list = response.lists.iter().find(|list| list.name == "Work").unwrap();
        let work = categories.iter().find(|cat| cat.slug == "work").unwrap();
        assert_eq!(work.list_id.as_deref(), Some(work_list.id.as_str()));
        assert_eq!(work.color, work_list.color);
        assert!(work.is_productive, "Work is productive");
        let fitness = categories.iter().find(|cat| cat.slug == "fitness").unwrap();
        assert!(fitness.is_productive, "Fitness is productive");
        let family = categories.iter().find(|cat| cat.slug == "family").unwrap();
        assert!(!family.is_productive, "Family is not productive");

        // Two patterns per root, none on untracked.
        let mut pattern_count = 0;
        for category in &categories {
            pattern_count += pollster::block_on(
                category_repo.list_patterns_by_category_id(&category.id),
            )
            .unwrap()
            .len();
        }
        assert_eq!(pattern_count, 8, "2 patterns × 4 roots");

        // Untracked: NULL list_id, flagged, no patterns.
        let untracked = categories.iter().find(|cat| cat.is_untracked).unwrap();
        assert_eq!(untracked.slug, "untracked");
        assert_eq!(untracked.list_id, None);
        assert_eq!(
            pollster::block_on(category_repo.list_patterns_by_category_id(&untracked.id))
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn second_visit_does_not_reseed() {
        let list_repo = FakeTaskListRepo::new();
        let category_repo = FakeTaskCategoryRepo::new();
        pollster::block_on(crate::list_lists(&list_repo, &category_repo, "u-1")).unwrap();
        pollster::block_on(crate::list_lists(&list_repo, &category_repo, "u-1")).unwrap();

        assert_eq!(list_repo.inserted.lock().unwrap().len(), 4, "lists seeded once");
        assert_eq!(
            category_repo.inserted.lock().unwrap().len(),
            5,
            "categories seeded once"
        );
        assert_eq!(
            pollster::block_on(category_repo.count_by_user_id("u-1")).unwrap(),
            5
        );
    }

    #[test]
    fn ensure_taxonomy_seeds_untracked_even_without_matching_lists() {
        // A user with custom lists (no seed names) still gets the untracked
        // sink, but no roots (they need a same-named list).
        let list_repo = FakeTaskListRepo::new();
        let category_repo = FakeTaskCategoryRepo::new();
        pollster::block_on(list_repo.insert(NewTaskList {
            user_id: "u-1".to_string(),
            name: "Hobbies".to_string(),
            color: "#000000".to_string(),
            sort_order: 0,
        }))
        .unwrap();
        let lists = pollster::block_on(list_repo.list_by_user_id("u-1")).unwrap();
        pollster::block_on(ensure_taxonomy(
            &list_repo,
            &category_repo,
            &lists,
            &[],
            "u-1",
        ))
        .unwrap();

        let categories = pollster::block_on(category_repo.list_by_user_id("u-1")).unwrap();
        assert_eq!(categories.len(), 1);
        assert!(categories[0].is_untracked);
    }

    // ──────────────────────────────────────────
    // List (view)
    // ──────────────────────────────────────────

    #[test]
    fn list_categories_computes_inherited_list_ids() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root-a", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("root-b", "u-1", Some("l-2"), None, "Fitness", "fitness", true, false, 1),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root-a"), "Coding", "coding", false, false, 0),
            FakeTaskCategoryRepo::row("sink", "u-1", None, None, "Untracked", "untracked", false, true, 100),
            FakeTaskCategoryRepo::row("other", "u-2", Some("l-9"), None, "Theirs", "theirs", false, false, 0),
        ]);
        let response = pollster::block_on(list_categories(&repo, "u-1")).unwrap();
        assert_eq!(response.categories.len(), 4);

        let by_id = |id: &str| {
            response
                .categories
                .iter()
                .find(|view| view.id == id)
                .unwrap_or_else(|| panic!("missing {id}"))
        };
        assert_eq!(by_id("root-a").inherited_list_id.as_deref(), Some("l-1"));
        assert_eq!(
            by_id("child").inherited_list_id.as_deref(),
            Some("l-1"),
            "child inherits the parent root's list"
        );
        assert_eq!(by_id("child").list_id, None, "stored column stays NULL");
        assert_eq!(by_id("sink").inherited_list_id, None);
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_root_with_list_id_and_patterns() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::with(vec![FakeTaskListRepo::row("l-1", "u-1", "Work", 0)]);
        let mut new_input = input("  Deep Work  ", "  #2a5c8a  ");
        new_input.list_id = Some("l-1".to_string());
        new_input.is_productive = Some(true);
        new_input.patterns = vec![pattern("^Deep Work$"), pattern("^.* [|] Deep Work$")];

        let response = pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)).unwrap();
        let category = response.category;
        assert_eq!(category.title, "Deep Work");
        assert_eq!(category.slug, "deep-work", "slugified from the title");
        assert_eq!(category.color, "#2a5c8a");
        assert!(category.is_productive);
        assert_eq!(category.inherited_list_id.as_deref(), Some("l-1"));
        assert_eq!(category.list_id.as_deref(), Some("l-1"));
        assert_eq!(category.patterns.len(), 2);
        assert_eq!(category.patterns[0].regex, "^Deep Work$");
    }

    #[test]
    fn create_child_inherits_parent_list_and_has_no_list_id() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("l-1", "u-2", None, None, "Other", "other", false, false, 0),
        ]);
        let lists = FakeTaskListRepo::with(vec![FakeTaskListRepo::row("l-1", "u-1", "Work", 0)]);
        let mut new_input = input("Code Reviews", "#2a5c8a");
        new_input.parent_id = Some("root".to_string());

        let response = pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)).unwrap();
        let category = response.category;
        assert_eq!(category.parent_id.as_deref(), Some("root"));
        assert_eq!(category.list_id, None);
        assert_eq!(category.inherited_list_id.as_deref(), Some("l-1"));
    }

    #[test]
    fn create_child_with_list_id_is_invalid() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let mut new_input = input("Child", "#2a5c8a");
        new_input.parent_id = Some("root".to_string());
        new_input.list_id = Some("l-1".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
            Err(CategoriesError::Invalid(message)) if message == "children cannot have a list_id"
        ));
    }

    #[test]
    fn create_root_without_list_id_is_invalid() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::new();
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &input("Work", "#2a5c8a"))),
            Err(CategoriesError::Invalid(message)) if message == "root categories must have a list_id"
        ));
    }

    #[test]
    fn create_grandchild_is_invalid() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root"), "Coding", "coding", false, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let mut new_input = input("Grandchild", "#2a5c8a");
        new_input.parent_id = Some("child".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
            Err(CategoriesError::Invalid(message)) if message == "cannot create a category under a non-root category"
        ));
    }

    #[test]
    fn create_requires_existing_user_owned_parent() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-2", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let mut new_input = input("Child", "#2a5c8a");
        new_input.parent_id = Some("root".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
            Err(CategoriesError::NotFound)
        ));
        let mut other = input("Child", "#2a5c8a");
        other.parent_id = Some("nope".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &other)),
            Err(CategoriesError::NotFound)
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 0, "nothing persisted");
    }

    #[test]
    fn create_rejects_empty_title_color_and_untracked_flag() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::new();
        let mut no_title = input("   ", "#2a5c8a");
        no_title.list_id = Some("l-1".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &no_title)),
            Err(CategoriesError::Invalid(message)) if message == "title must not be empty"
        ));
        let mut no_color = input("Work", "  ");
        no_color.list_id = Some("l-1".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &no_color)),
            Err(CategoriesError::Invalid(message)) if message == "color must not be empty"
        ));
        let mut sink = input("Work", "#2a5c8a");
        sink.list_id = Some("l-1".to_string());
        sink.is_untracked = Some(true);
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &sink)),
            Err(CategoriesError::Invalid(message)) if message == "is_untracked can only be set by the system"
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 0);
    }

    #[test]
    fn create_rejects_bad_patterns() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::new();
        let mut empty_pattern = input("Work", "#2a5c8a");
        empty_pattern.list_id = Some("l-1".to_string());
        empty_pattern.patterns = vec![pattern("   ")];
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &empty_pattern)),
            Err(CategoriesError::Invalid(message)) if message == "pattern regex must not be empty"
        ));

        let mut bad_regex = input("Work", "#2a5c8a");
        bad_regex.list_id = Some("l-1".to_string());
        bad_regex.patterns = vec![pattern("(unclosed")];
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &bad_regex)),
            Err(CategoriesError::Invalid(message)) if message == "pattern regex does not compile"
        ));

        let mut long_pattern = input("Work", "#2a5c8a");
        long_pattern.list_id = Some("l-1".to_string());
        long_pattern.patterns = vec![pattern(&"a".repeat(257))];
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &long_pattern)),
            Err(CategoriesError::Invalid(message)) if message == "pattern regex must be at most 256 characters"
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 0);
    }

    #[test]
    fn create_rejects_duplicate_slug() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::with(vec![FakeTaskListRepo::row("l-1", "u-1", "Work", 0)]);
        let mut new_input = input("Work", "#2a5c8a");
        new_input.list_id = Some("l-1".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
            Err(CategoriesError::Conflict(message)) if message == r#"slug "work" is already in use"#
        ));
    }

    #[test]
    fn create_root_rejects_another_users_list() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::with(vec![FakeTaskListRepo::row("l-1", "u-2", "Theirs", 0)]);
        let mut new_input = input("Work", "#2a5c8a");
        new_input.list_id = Some("l-1".to_string());
        assert!(
            matches!(
                pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
                Err(CategoriesError::NotFound)
            ),
            "a category must never be filed onto another user's list"
        );
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 0, "nothing persisted");
    }

    #[test]
    fn create_root_rejects_missing_list() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::new();
        let mut new_input = input("Work", "#2a5c8a");
        new_input.list_id = Some("no-such-list".to_string());
        assert!(matches!(
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)),
            Err(CategoriesError::NotFound)
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 0);
    }

    #[test]
    fn create_root_with_own_list_is_ok() {
        let repo = FakeTaskCategoryRepo::new();
        let lists = FakeTaskListRepo::with(vec![FakeTaskListRepo::row("l-1", "u-1", "Work", 0)]);
        let mut new_input = input("Deep Work", "#2a5c8a");
        new_input.list_id = Some("l-1".to_string());
        let response = pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)).unwrap();
        assert_eq!(response.category.inherited_list_id.as_deref(), Some("l-1"));
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            1,
            "created under the user's own list"
        );
    }

    // ──────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────

    #[test]
    fn update_applies_fields_and_replaces_patterns() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            title: Some("Deep Work".to_string()),
            color: Some("#3a3a3a".to_string()),
            is_productive: Some(false),
            patterns: Some(vec![pattern("^Deep Work$")]),
            ..UpdateTaskCategory::default()
        };
        let response = pollster::block_on(update_category(&repo, &lists, "u-1", "root", &updates)).unwrap();
        let category = response.category;
        assert_eq!(category.title, "Deep Work");
        assert_eq!(category.color, "#3a3a3a");
        assert!(!category.is_productive);
        assert_eq!(category.slug, "work", "slug untouched when omitted");
        assert_eq!(category.inherited_list_id.as_deref(), Some("l-1"));
        assert_eq!(category.patterns.len(), 1, "old patterns replaced");
        assert_eq!(category.patterns[0].regex, "^Deep Work$");
    }

    #[test]
    fn update_child_with_list_id_is_invalid() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root"), "Coding", "coding", false, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            list_id: Some("l-1".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "child", &updates)),
            Err(CategoriesError::Invalid(message)) if message == "children cannot have a list_id"
        ));
    }

    #[test]
    fn update_untracked_is_conflict() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("sink", "u-1", None, None, "Untracked", "untracked", false, true, 100),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            title: Some("Renamed".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "sink", &updates)),
            Err(CategoriesError::Conflict(message)) if message == "untracked category cannot be modified"
        ));
    }

    #[test]
    fn update_rejects_self_parenting_and_parenting_to_a_child() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root"), "Coding", "coding", false, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let self_parent = UpdateTaskCategory {
            parent_id: Some("child".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "child", &self_parent)),
            Err(CategoriesError::Invalid(message)) if message == "cannot parent a category to itself"
        ));
        let to_child = UpdateTaskCategory {
            parent_id: Some("child".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "root", &to_child)),
            Err(CategoriesError::Invalid(message)) if message == "cannot parent a category to a child"
        ));
    }

    #[test]
    fn update_rejects_reparenting_a_category_with_children() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("other", "u-1", Some("l-2"), None, "Fitness", "fitness", true, false, 1),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root"), "Coding", "coding", false, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            parent_id: Some("other".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "root", &updates)),
            Err(CategoriesError::Invalid(message)) if message == "cannot reparent a category that has children"
        ));
    }

    #[test]
    fn update_can_move_a_leaf_root_under_another_root_and_nulls_list_id() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("other", "u-1", Some("l-2"), None, "Fitness", "fitness", true, false, 1),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            parent_id: Some("root".to_string()),
            ..UpdateTaskCategory::default()
        };
        let response = pollster::block_on(update_category(&repo, &lists, "u-1", "other", &updates)).unwrap();
        let category = response.category;
        assert_eq!(category.parent_id.as_deref(), Some("root"));
        assert_eq!(category.list_id, None, "stored list_id NULLed on move");
        assert_eq!(category.inherited_list_id.as_deref(), Some("l-1"));
    }

    #[test]
    fn update_root_moving_to_another_users_list_is_not_found() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", 0),
            FakeTaskListRepo::row("l-2", "u-2", "Theirs", 1),
        ]);
        let updates = UpdateTaskCategory {
            list_id: Some("l-2".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "root", &updates)),
            Err(CategoriesError::NotFound)
        ));
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            1,
            "category survives"
        );
    }

    #[test]
    fn update_root_moving_to_missing_list_is_not_found() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let updates = UpdateTaskCategory {
            list_id: Some("no-such-list".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "root", &updates)),
            Err(CategoriesError::NotFound)
        ));
    }

    #[test]
    fn update_root_can_move_to_another_own_list() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", 0),
            FakeTaskListRepo::row("l-2", "u-1", "Reading", 1),
        ]);
        let updates = UpdateTaskCategory {
            list_id: Some("l-2".to_string()),
            ..UpdateTaskCategory::default()
        };
        let response = pollster::block_on(update_category(&repo, &lists, "u-1", "root", &updates)).unwrap();
        assert_eq!(response.category.inherited_list_id.as_deref(), Some("l-2"));
        assert_eq!(response.category.list_id.as_deref(), Some("l-2"));
    }

    #[test]
    fn update_rejects_empty_body_and_other_users_category() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "root", &UpdateTaskCategory::default())),
            Err(CategoriesError::Invalid(message)) if message == "nothing to update"
        ));
        let updates = UpdateTaskCategory {
            title: Some("X".to_string()),
            ..UpdateTaskCategory::default()
        };
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-2", "root", &updates)),
            Err(CategoriesError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(update_category(&repo, &lists, "u-1", "nope", &updates)),
            Err(CategoriesError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────

    #[test]
    fn delete_leaf_hard_deletes_patterns_and_soft_deletes_the_category() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        let lists = FakeTaskListRepo::new();
        let mut new_input = input("Code Reviews", "#2a5c8a");
        new_input.parent_id = Some("root".to_string());
        new_input.patterns = vec![pattern("^Review$")];
        let created =
            pollster::block_on(create_category(&repo, &lists, "u-1", &new_input)).unwrap().category;

        let response = pollster::block_on(delete_category(
            &repo,
            "u-1",
            &created.id,
            "2026-08-18T02:00:00Z",
        ))
        .unwrap();
        assert!(response.success);
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            1,
            "only the root survives"
        );
        assert_eq!(
            pollster::block_on(repo.list_patterns_by_category_id(&created.id))
                .unwrap()
                .len(),
            0,
            "patterns are hard-deleted"
        );
        assert!(matches!(
            pollster::block_on(delete_category(&repo, "u-1", &created.id, "2026-08-18T02:00:00Z")),
            Err(CategoriesError::NotFound)
        ));
    }

    #[test]
    fn delete_root_with_children_is_conflict() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
            FakeTaskCategoryRepo::row("child", "u-1", None, Some("root"), "Coding", "coding", false, false, 0),
        ]);
        assert!(matches!(
            pollster::block_on(delete_category(&repo, "u-1", "root", "2026-08-18T02:00:00Z")),
            Err(CategoriesError::Conflict(message)) if message == "category has children"
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 2);
    }

    #[test]
    fn delete_untracked_is_conflict() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("sink", "u-1", None, None, "Untracked", "untracked", false, true, 100),
        ]);
        assert!(matches!(
            pollster::block_on(delete_category(&repo, "u-1", "sink", "2026-08-18T02:00:00Z")),
            Err(CategoriesError::Conflict(message)) if message == "untracked category cannot be deleted"
        ));
        assert_eq!(pollster::block_on(repo.count_by_user_id("u-1")).unwrap(), 1);
    }

    #[test]
    fn delete_is_not_found_for_missing_and_other_users_category() {
        let repo = FakeTaskCategoryRepo::with(vec![
            FakeTaskCategoryRepo::row("root", "u-1", Some("l-1"), None, "Work", "work", true, false, 0),
        ]);
        assert!(matches!(
            pollster::block_on(delete_category(&repo, "u-2", "root", "2026-08-18T02:00:00Z")),
            Err(CategoriesError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(delete_category(&repo, "u-1", "nope", "2026-08-18T02:00:00Z")),
            Err(CategoriesError::NotFound)
        ));
    }
}

//! Task list service: CRUD for `task_lists` (the former "streams"), with a
//! first-visit seed.
//!
//! Pure Rust and unit-testable: persistence goes through [`TaskListRepo`]
//! (faked with an in-memory impl in tests). The Worker layers session checks
//! on top (`apps/worker/src/lists.rs`).
//!
//! Domain rules:
//! - A list is a coarse folder. It does **not** own tasks; `tasks` will hang
//!   off categories (slice 3).
//! - `task_lists` is user-scoped; the session cookie alone authorizes (no
//!   Google token refresh, unlike the calendar handlers).
//! - The first `GET /api/lists` for a user with zero living lists seeds the
//!   four default lists (`sort_order` 0..3).
//! - Delete is blocked (409) while any living ROOT category references the
//!   list (`count_root_categories_for_list`).
//! - Soft-delete everywhere: reads filter `deleted_at IS NULL`.
//!
//! Error/ownership rules:
//! - Missing, soft-deleted, or another user's list is always [`ListsError::NotFound`]
//!   — ownership is never leaked as a different status.
//! - Invalid input (empty name/color, non-palette color, empty PATCH body) is
//!   [`ListsError::Invalid`] → HTTP 400.

use thiserror::Error;

use crate::google_color::{canonicalize_hex, is_event_label_hex};
use crate::models::{NewTaskList, TaskList, UpdateTaskList};
use crate::repo::{RepoError, TaskCategoryRepo, TaskListRepo};

/// The first-visit seed: the four default lists, `sort_order` 0..3.
///
/// Colors are event-label palette hexes (see [`crate::google_color`]):
/// Work = cobalt, Fitness = tangerine, Family = grape, Personal = basil.
/// Existing rows are never migrated — only brand-new users see these.
pub const SEED_LISTS: [(&str, &str, i64); 4] = [
    ("Work", "#4285f4", 0),
    ("Fitness", "#f4511e", 1),
    ("Family", "#8e24aa", 2),
    ("Personal", "#0b8043", 3),
];

/// Errors produced by the lists service.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ListsError {
    #[error("{0}")]
    Invalid(String),
    #[error("list not found")]
    NotFound,
    #[error("list in use")]
    Conflict,
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// `list_lists` seeds the category taxonomy too, so its errors fold into
/// [`ListsError`] (same HTTP mapping: 400 / 404 / 409 / 500).
impl From<crate::categories::CategoriesError> for ListsError {
    fn from(err: crate::categories::CategoriesError) -> Self {
        match err {
            crate::categories::CategoriesError::Invalid(message) => ListsError::Invalid(message),
            crate::categories::CategoriesError::NotFound => ListsError::NotFound,
            crate::categories::CategoriesError::Conflict(_) => ListsError::Conflict,
            crate::categories::CategoriesError::Repo(err) => ListsError::Repo(err),
        }
    }
}

/// Response envelope for `GET /api/lists`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskListsResponse {
    pub lists: Vec<TaskList>,
}

/// Response envelope for `POST /api/lists` and `PATCH /api/lists/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskListResponse {
    pub list: TaskList,
}

/// Response envelope for `DELETE /api/lists/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteListResponse {
    pub success: bool,
}

/// Lists the user's living lists, seeding the defaults on first visit.
///
/// The seed is keyed off the living-list count: when the user has zero
/// non-deleted lists, the four [`SEED_LISTS`] are inserted (each as its own
/// row, minted by the repo), then the fresh rows are returned ordered by
/// `sort_order` then `name`. A second call sees `count > 0` and never
/// duplicates.
///
/// Once the lists are in place, the category taxonomy is seeded too:
/// [`crate::categories::ensure_taxonomy`] inserts the four seed roots plus
/// `untracked` when the user has zero living categories. This is the single
/// seeding path — `GET /api/categories` intentionally does not seed, so
/// parallel first-visit fetches can never double-seed.
pub async fn list_lists(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<TaskListsResponse, ListsError> {
    if list_repo.count_by_user_id(user_id).await? == 0 {
        for (name, color, sort_order) in SEED_LISTS {
            list_repo
                .insert(NewTaskList {
                    user_id: user_id.to_string(),
                    name: name.to_string(),
                    color: color.to_string(),
                    sort_order,
                })
                .await?;
        }
    }
    let lists = list_repo.list_by_user_id(user_id).await?;
    if !lists.is_empty() {
        let categories = category_repo.list_by_user_id(user_id).await?;
        crate::categories::ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id)
            .await?;
    }
    Ok(TaskListsResponse { lists })
}

/// Creates a list for the user.
///
/// `name` is trimmed and must not be empty; `color` must be one of the 24
/// Google event-label hexes (trimmed, case-insensitive) and is stored in its
/// canonical lowercase `#rrggbb` form. Violations are [`ListsError::Invalid`]
/// → HTTP 400.
pub async fn create_list(
    repo: &dyn TaskListRepo,
    user_id: &str,
    name: &str,
    color: &str,
) -> Result<TaskListResponse, ListsError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ListsError::Invalid("name must not be empty".to_string()));
    }
    let color = validate_color(color)?;
    let list = repo
        .insert(NewTaskList {
            user_id: user_id.to_string(),
            name: name.to_string(),
            color,
            sort_order: 0,
        })
        .await?;
    Ok(TaskListResponse { list })
}

/// Updates a list's `name`/`color`/`sort_order` (`None` = unchanged).
///
/// - A body with nothing to update is [`ListsError::Invalid`] (400).
/// - A name that trims to empty is [`ListsError::Invalid`] (400).
/// - A `color` that trims to empty, is not a hex, or is not one of the 24
///   Google event-label hexes is [`ListsError::Invalid`] (400). A present
///   color is stored canonicalized; an omitted one leaves the stored hex
///   untouched (stale non-palette hexes keep loading until a color PATCH).
/// - A missing, soft-deleted, or another user's list is
///   [`ListsError::NotFound`] (404) — ownership is never leaked.
pub async fn update_list(
    repo: &dyn TaskListRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateTaskList,
) -> Result<TaskListResponse, ListsError> {
    let mut updates = updates.clone();
    if updates.name.is_none() && updates.color.is_none() && updates.sort_order.is_none() {
        return Err(ListsError::Invalid("nothing to update".to_string()));
    }
    if let Some(name) = updates.name.as_deref() {
        if name.trim().is_empty() {
            return Err(ListsError::Invalid("name must not be empty".to_string()));
        }
    }
    if let Some(color) = updates.color.as_deref() {
        let color = validate_color(color)?;
        updates.color = Some(color);
    }

    let Some(list) = repo.get_by_id(id).await? else {
        return Err(ListsError::NotFound);
    };
    // `get_by_id` is intentionally not user-scoped; ownership is checked here
    // so another user's list is a plain 404, never a 409/other leak.
    if list.user_id != user_id {
        return Err(ListsError::NotFound);
    }

    let Some(updated) = repo.update(id, &updates).await? else {
        // Deleted between the read and the write.
        return Err(ListsError::NotFound);
    };
    Ok(TaskListResponse { list: updated })
}

/// SOFT deletes a list.
///
/// - Missing or another user's list → [`ListsError::NotFound`] (404).
/// - Any living ROOT category still referencing the list →
///   [`ListsError::Conflict`] (409).
/// - Otherwise the row is stamped with `deleted_at = now_rfc3339` and
///   `{"success": true}` is returned.
pub async fn delete_list(
    repo: &dyn TaskListRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
) -> Result<DeleteListResponse, ListsError> {
    let Some(list) = repo.get_by_id(id).await? else {
        return Err(ListsError::NotFound);
    };
    // Ownership is checked before the category guard so another user's list is
    // a plain 404 and never reveals whether it has referencing categories.
    if list.user_id != user_id {
        return Err(ListsError::NotFound);
    }
    if repo.count_root_categories_for_list(id).await? > 0 {
        return Err(ListsError::Conflict);
    }
    repo.soft_delete(id, now_rfc3339).await?;
    Ok(DeleteListResponse { success: true })
}

/// Validates a list color for writes: trimmed, must be a hex that is exactly
/// one of the 24 Google event-label hexes (case-insensitive); returns the
/// canonical lowercase `#rrggbb` form to persist.
///
/// - empty → `"color must not be empty"`
/// - unparseable hex (`blue`, `#gg0000`) → `"color must be #rgb or #rrggbb"`
/// - parseable but not among the 24 (`#535050`, `#2a5c8a`) → the palette
///   message below
fn validate_color(color: &str) -> Result<String, ListsError> {
    let color = color.trim();
    if color.is_empty() {
        return Err(ListsError::Invalid("color must not be empty".to_string()));
    }
    let canonical = canonicalize_hex(color)
        .map_err(|err| ListsError::Invalid(err.to_string()))?;
    if !is_event_label_hex(&canonical) {
        return Err(ListsError::Invalid(
            "color must be one of the 24 Google event-label hexes".to_string(),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        NewTaskCategory, NewTaskCategoryPattern, TaskCategory, TaskCategoryPattern,
        UpdateTaskCategory,
    };
    use crate::repo::TaskCategoryRepo;

    // ──────────────────────────────────────────
    // Fake
    // ──────────────────────────────────────────

    /// In-memory `TaskListRepo`: rows are materialized like D1, and the
    /// root-category count per list is scripted by the test (the category
    /// tables are slice 2).
    struct FakeTaskListRepo {
        stored: Mutex<Vec<TaskList>>,
        root_categories: Mutex<HashMap<String, i64>>,
        inserted: Mutex<Vec<NewTaskList>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskListRepo {
        fn with(lists: Vec<TaskList>) -> Self {
            Self {
                stored: Mutex::new(lists),
                root_categories: Mutex::new(HashMap::new()),
                inserted: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        /// Scripts the delete guard: `count_root_categories_for_list` returns
        /// this count for `list_id` (0 by default).
        fn with_root_categories(&self, list_id: &str, count: i64) -> &Self {
            self.root_categories
                .lock()
                .unwrap()
                .insert(list_id.to_string(), count);
            self
        }

        fn row(list_id: &str, user_id: &str, name: &str, color: &str, sort_order: i64) -> TaskList {
            TaskList {
                id: list_id.to_string(),
                user_id: user_id.to_string(),
                name: name.to_string(),
                color: color.to_string(),
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
                .filter(|list| list.user_id == user_id && list.deleted_at.is_none())
                .cloned()
                .collect();
            // Mirrors TASK_LIST_LIST_BY_USER_ID_SQL.
            rows.sort_by(|a, b| (a.sort_order, &a.name).cmp(&(b.sort_order, &b.name)));
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<TaskList>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|list| list.id == id && list.deleted_at.is_none())
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
            id: &str,
            updates: &UpdateTaskList,
        ) -> Result<Option<TaskList>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|list| list.id == id && list.deleted_at.is_none())
            else {
                return Ok(None);
            };
            if let Some(name) = &updates.name {
                row.name = name.clone();
            }
            if let Some(color) = &updates.color {
                row.color = color.clone();
            }
            if let Some(sort_order) = updates.sort_order {
                row.sort_order = sort_order;
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
                .find(|list| list.id == id && list.deleted_at.is_none())
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
                .filter(|list| list.user_id == user_id && list.deleted_at.is_none())
                .count() as i64)
        }

        async fn count_root_categories_for_list(&self, list_id: &str) -> Result<i64, RepoError> {
            Ok(self.root_categories.lock().unwrap().get(list_id).copied().unwrap_or(0))
        }
    }

    /// In-memory `TaskCategoryRepo` for the lists tests: no-op persistence
    /// that records inserts (the taxonomy behaviour itself is tested in
    /// `crate::categories`). `list_lists` needs the trait, so every method is
    /// implemented against an empty store except the count/insert/patters
    /// bookkeeping the seed path exercises.
    struct FakeTaskCategoryRepo {
        stored: Mutex<Vec<TaskCategory>>,
        inserted: Mutex<Vec<NewTaskCategory>>,
        patterns: Mutex<Vec<TaskCategoryPattern>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskCategoryRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                inserted: Mutex::new(Vec::new()),
                patterns: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        fn inserted_patterns(&self) -> usize {
            self.patterns.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskCategoryRepo for FakeTaskCategoryRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<TaskCategory>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect())
        }

        async fn get_by_id(&self, _id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(None)
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
            _id: &str,
            _updates: &UpdateTaskCategory,
        ) -> Result<Option<TaskCategory>, RepoError> {
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

        async fn count_children(&self, _category_id: &str) -> Result<i64, RepoError> {
            Ok(0)
        }

        async fn get_untracked(&self, _user_id: &str) -> Result<Option<TaskCategory>, RepoError> {
            Ok(None)
        }

        async fn list_patterns_by_category_id(
            &self,
            _category_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            Ok(Vec::new())
        }

        async fn list_patterns_by_user_id(
            &self,
            user_id: &str,
        ) -> Result<Vec<TaskCategoryPattern>, RepoError> {
            // Flat storage: match on the user's living categories' ids.
            let living: Vec<String> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .map(|row| row.id.clone())
                .collect();
            Ok(self
                .patterns
                .lock()
                .unwrap()
                .iter()
                .filter(|pattern| living.contains(&pattern.category_id))
                .cloned()
                .collect())
        }

        async fn replace_patterns(
            &self,
            _category_id: &str,
            patterns: Vec<NewTaskCategoryPattern>,
        ) -> Result<(), RepoError> {
            self.patterns.lock().unwrap().extend(
                patterns
                    .into_iter()
                    .enumerate()
                    .map(|(sort_order, input)| TaskCategoryPattern {
                        id: format!("pat-{sort_order}"),
                        category_id: "cat".to_string(),
                        regex: input.regex,
                        google_calendar_id: input.google_calendar_id,
                        sort_order: sort_order as i64,
                        created_at: "2026-08-18T00:00:00Z".to_string(),
                        updated_at: "2026-08-18T00:00:00Z".to_string(),
                    }),
            );
            Ok(())
        }

        async fn delete_patterns_by_category_id(&self, _category_id: &str) -> Result<(), RepoError> {
            Ok(())
        }
    }

    /// Convenience: a fresh pair of repos for the seed tests.
    fn repos() -> (FakeTaskListRepo, FakeTaskCategoryRepo) {
        (FakeTaskListRepo::with(Vec::new()), FakeTaskCategoryRepo::new())
    }

    // ──────────────────────────────────────────
    // Seeding
    // ──────────────────────────────────────────

    #[test]
    fn first_get_seeds_four_default_lists() {
        let (repo, category_repo) = repos();
        let response = pollster::block_on(list_lists(&repo, &category_repo, "u-1")).unwrap();

        let names: Vec<&str> = response.lists.iter().map(|list| list.name.as_str()).collect();
        assert_eq!(names, ["Work", "Fitness", "Family", "Personal"]);
        let orders: Vec<i64> = response.lists.iter().map(|list| list.sort_order).collect();
        assert_eq!(orders, [0, 1, 2, 3]);
        let colors: Vec<&str> = response.lists.iter().map(|list| list.color.as_str()).collect();
        assert_eq!(colors, ["#4285f4", "#f4511e", "#8e24aa", "#0b8043"]);
        assert!(response.lists.iter().all(|list| list.user_id == "u-1"));
    }

    #[test]
    fn second_get_does_not_reseed() {
        let (repo, category_repo) = repos();
        let first = pollster::block_on(list_lists(&repo, &category_repo, "u-1")).unwrap();
        let second = pollster::block_on(list_lists(&repo, &category_repo, "u-1")).unwrap();

        assert_eq!(first.lists.len(), 4);
        assert_eq!(second.lists.len(), 4, "no duplicate seeding");
        assert_eq!(repo.inserted.lock().unwrap().len(), 4, "exactly one seed");
        assert_eq!(
            category_repo.inserted.lock().unwrap().len(),
            5,
            "category seed (four roots + untracked) also runs once"
        );
        assert_eq!(category_repo.inserted_patterns(), 8, "two patterns per root");
    }

    #[test]
    fn list_lists_sorts_by_sort_order_then_name() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-a", "u-1", "Zzz", "#000000", 3),
            FakeTaskListRepo::row("l-b", "u-1", "Alpha", "#000000", 1),
            FakeTaskListRepo::row("l-c", "u-1", "Beta", "#000000", 1),
            FakeTaskListRepo::row("l-d", "u-2", "Other", "#000000", 0),
        ]);
        let category_repo = FakeTaskCategoryRepo::new();
        let response = pollster::block_on(list_lists(&repo, &category_repo, "u-1")).unwrap();

        let ids: Vec<&str> = response.lists.iter().map(|list| list.id.as_str()).collect();
        assert_eq!(ids, ["l-b", "l-c", "l-a"]);
        assert_eq!(repo.inserted.lock().unwrap().len(), 0, "lists exist — no seed");
        // No seed-name list ("Zzz"/"Alpha"/"Beta") → only untracked is seeded.
        assert_eq!(category_repo.inserted.lock().unwrap().len(), 1);
        assert_eq!(category_repo.inserted_patterns(), 0, "untracked has no patterns");
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_list_trims_name_and_canonicalizes_color() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let response = pollster::block_on(create_list(&repo, "u-1", "  Work?  ", "  #039BE5  "))
            .unwrap();
        assert_eq!(response.list.name, "Work?");
        assert_eq!(response.list.color, "#039be5", "palette hex stored canonical lowercase");
        assert_eq!(response.list.sort_order, 0);
        assert_eq!(response.list.user_id, "u-1");
    }

    #[test]
    fn create_list_rejects_empty_or_whitespace_name() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let empty = pollster::block_on(create_list(&repo, "u-1", "", "#039be5"));
        assert!(matches!(empty, Err(ListsError::Invalid(m)) if m == "name must not be empty"));
        let blank = pollster::block_on(create_list(&repo, "u-1", "   ", "#039be5"));
        assert!(matches!(blank, Err(ListsError::Invalid(_))));
        assert!(repo.inserted.lock().unwrap().is_empty(), "nothing persisted");
    }

    #[test]
    fn create_list_rejects_empty_color() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let empty = pollster::block_on(create_list(&repo, "u-1", "Work", ""));
        assert!(matches!(empty, Err(ListsError::Invalid(m)) if m == "color must not be empty"));
        let blank = pollster::block_on(create_list(&repo, "u-1", "Work", "   "));
        assert!(matches!(blank, Err(ListsError::Invalid(m)) if m == "color must not be empty"));
        assert!(repo.inserted.lock().unwrap().is_empty(), "nothing persisted");
    }

    #[test]
    fn create_list_rejects_non_palette_hexes() {
        let repo = FakeTaskListRepo::with(Vec::new());
        // Parseable hexes that are not among the 24 event labels.
        for bad in ["#535050", "#2a5c8a", "#3a3a3a"] {
            assert!(
                matches!(
                    pollster::block_on(create_list(&repo, "u-1", "Work", bad)),
                    Err(ListsError::Invalid(m)) if m == "color must be one of the 24 Google event-label hexes"
                ),
                "color {bad:?} must be rejected as not-in-palette"
            );
        }
        // Non-hex strings keep the parse message.
        for bad in ["blue", "#gg0000", "2a5c8a"] {
            assert!(
                matches!(
                    pollster::block_on(create_list(&repo, "u-1", "Work", bad)),
                    Err(ListsError::Invalid(m)) if m == "color must be #rgb or #rrggbb"
                ),
                "color {bad:?} must be rejected as non-hex"
            );
        }
        assert!(repo.inserted.lock().unwrap().is_empty(), "nothing persisted");
    }

    // ──────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────

    #[test]
    fn update_list_applies_partial_updates() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        let updates = UpdateTaskList {
            name: Some("Deep Work".to_string()),
            color: None,
            sort_order: None,
        };
        let response = pollster::block_on(update_list(&repo, "u-1", "l-1", &updates)).unwrap();
        assert_eq!(response.list.name, "Deep Work");
        assert_eq!(response.list.color, "#2a5c8a", "color left unchanged");
    }

    #[test]
    fn update_list_name_only_leaves_stale_non_palette_color_untouched() {
        // A PATCH that does not send `color` must succeed even when the
        // stored hex is not one of the 24 event labels.
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        let updates = UpdateTaskList {
            name: Some("Renamed".to_string()),
            color: None,
            sort_order: None,
        };
        let response = pollster::block_on(update_list(&repo, "u-1", "l-1", &updates)).unwrap();
        assert_eq!(response.list.name, "Renamed");
        assert_eq!(
            response.list.color, "#2a5c8a",
            "stale non-palette hex survives a color-less PATCH"
        );
    }

    #[test]
    fn update_list_color_is_canonicalized_and_must_be_palette() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        // A parseable but non-palette hex is a 400 and the row is untouched.
        let bad = UpdateTaskList {
            name: None,
            color: Some("#535050".to_string()),
            sort_order: None,
        };
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-1", &bad)),
            Err(ListsError::Invalid(m)) if m == "color must be one of the 24 Google event-label hexes"
        ));
        let stale = UpdateTaskList {
            name: None,
            color: Some("#2a5c8a".to_string()),
            sort_order: None,
        };
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-1", &stale)),
            Err(ListsError::Invalid(m)) if m == "color must be one of the 24 Google event-label hexes"
        ));
        // The stored row still carries its original hex.
        let row = pollster::block_on(repo.get_by_id("l-1")).unwrap().unwrap();
        assert_eq!(row.color, "#2a5c8a");

        // A palette hex is canonicalized (case + whitespace folded).
        let ok = UpdateTaskList {
            name: None,
            color: Some("  #4285F4  ".to_string()),
            sort_order: None,
        };
        let response = pollster::block_on(update_list(&repo, "u-1", "l-1", &ok)).unwrap();
        assert_eq!(response.list.color, "#4285f4");
    }

    #[test]
    fn update_list_404s_for_missing_soft_deleted_and_other_users_list() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        let updates = UpdateTaskList {
            name: Some("Renamed".to_string()),
            color: None,
            sort_order: None,
        };
        // Missing id.
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "nope", &updates)),
            Err(ListsError::NotFound)
        ));
        // Another user's list: 404, never a leak.
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-2", "l-1", &updates)),
            Err(ListsError::NotFound)
        ));
        // Soft-deleted list: get_by_id filters it out.
        let deleted = FakeTaskListRepo::row("l-2", "u-1", "Gone", "#000000", 1);
        repo.stored.lock().unwrap().push(TaskList {
            deleted_at: Some("2026-08-18T02:00:00Z".to_string()),
            ..deleted
        });
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-2", &updates)),
            Err(ListsError::NotFound)
        ));
    }

    #[test]
    fn update_list_rejects_empty_body_and_blank_values() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        let empty = UpdateTaskList::default();
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-1", &empty)),
            Err(ListsError::Invalid(m)) if m == "nothing to update"
        ));
        let blank_name = UpdateTaskList {
            name: Some("   ".to_string()),
            color: None,
            sort_order: None,
        };
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-1", &blank_name)),
            Err(ListsError::Invalid(m)) if m == "name must not be empty"
        ));
        let blank_color = UpdateTaskList {
            name: None,
            color: Some("".to_string()),
            sort_order: None,
        };
        assert!(matches!(
            pollster::block_on(update_list(&repo, "u-1", "l-1", &blank_color)),
            Err(ListsError::Invalid(m)) if m == "color must not be empty"
        ));
    }

    // ──────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────

    #[test]
    fn delete_list_succeeds_without_referencing_categories() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        let response = pollster::block_on(delete_list(&repo, "u-1", "l-1", "2026-08-18T02:00:00Z"))
            .unwrap();
        assert!(response.success);
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            0,
            "delete is soft: list no longer counted"
        );
        assert!(
            matches!(
                pollster::block_on(delete_list(&repo, "u-1", "l-1", "2026-08-18T02:00:00Z")),
                Err(ListsError::NotFound)
            ),
            "second delete of a soft-deleted list is a 404"
        );
    }

    #[test]
    fn delete_list_409s_when_living_root_categories_reference_it() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        repo.with_root_categories("l-1", 1);
        assert!(matches!(
            pollster::block_on(delete_list(&repo, "u-1", "l-1", "2026-08-18T02:00:00Z")),
            Err(ListsError::Conflict)
        ));
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            1,
            "list survives"
        );
    }

    #[test]
    fn delete_list_404s_for_missing_and_other_users_list() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-1", "u-1", "Work", "#2a5c8a", 0),
        ]);
        // Missing id.
        assert!(matches!(
            pollster::block_on(delete_list(&repo, "u-1", "nope", "2026-08-18T02:00:00Z")),
            Err(ListsError::NotFound)
        ));
        // Another user's list: 404 — even with referencing categories, the
        // ownership check runs first so the guard never leaks.
        repo.with_root_categories("l-1", 3);
        assert!(matches!(
            pollster::block_on(delete_list(&repo, "u-2", "l-1", "2026-08-18T02:00:00Z")),
            Err(ListsError::NotFound)
        ));
        assert_eq!(
            pollster::block_on(repo.count_by_user_id("u-1")).unwrap(),
            1,
            "list survives"
        );
    }
}

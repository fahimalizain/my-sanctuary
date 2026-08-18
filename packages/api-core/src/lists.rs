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
//! - Invalid input (empty name/color, empty PATCH body) is
//!   [`ListsError::Invalid`] → HTTP 400.

use thiserror::Error;

use crate::models::{NewTaskList, TaskList, UpdateTaskList};
use crate::repo::{RepoError, TaskListRepo};

/// The first-visit seed: the four default lists, `sort_order` 0..3.
pub const SEED_LISTS: [(&str, &str, i64); 4] = [
    ("Work", "#2a5c8a", 0),
    ("Fitness", "#c45a2c", 1),
    ("Family", "#7a4a6a", 2),
    ("Personal", "#3a7a5a", 3),
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
pub async fn list_lists(
    repo: &dyn TaskListRepo,
    user_id: &str,
) -> Result<TaskListsResponse, ListsError> {
    if repo.count_by_user_id(user_id).await? == 0 {
        for (name, color, sort_order) in SEED_LISTS {
            repo.insert(NewTaskList {
                user_id: user_id.to_string(),
                name: name.to_string(),
                color: color.to_string(),
                sort_order,
            })
            .await?;
        }
    }
    Ok(TaskListsResponse {
        lists: repo.list_by_user_id(user_id).await?,
    })
}

/// Creates a list for the user.
///
/// `name` is trimmed and must not be empty; `color` is trimmed and must not be
/// empty (the UI sends hex like `#2a5c8a`). Both violations are
/// [`ListsError::Invalid`] → HTTP 400.
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
    let color = color.trim();
    if color.is_empty() {
        return Err(ListsError::Invalid("color must not be empty".to_string()));
    }
    let list = repo
        .insert(NewTaskList {
            user_id: user_id.to_string(),
            name: name.to_string(),
            color: color.to_string(),
            sort_order: 0,
        })
        .await?;
    Ok(TaskListResponse { list })
}

/// Updates a list's `name`/`color`/`sort_order` (`None` = unchanged).
///
/// - A body with nothing to update is [`ListsError::Invalid`] (400).
/// - A name that trims to empty, or a color that trims to empty, is
///   [`ListsError::Invalid`] (400).
/// - A missing, soft-deleted, or another user's list is
///   [`ListsError::NotFound`] (404) — ownership is never leaked.
pub async fn update_list(
    repo: &dyn TaskListRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateTaskList,
) -> Result<TaskListResponse, ListsError> {
    if updates.name.is_none() && updates.color.is_none() && updates.sort_order.is_none() {
        return Err(ListsError::Invalid("nothing to update".to_string()));
    }
    if let Some(name) = updates.name.as_deref() {
        if name.trim().is_empty() {
            return Err(ListsError::Invalid("name must not be empty".to_string()));
        }
    }
    if let Some(color) = updates.color.as_deref() {
        if color.trim().is_empty() {
            return Err(ListsError::Invalid("color must not be empty".to_string()));
        }
    }

    let Some(list) = repo.get_by_id(id).await? else {
        return Err(ListsError::NotFound);
    };
    // `get_by_id` is intentionally not user-scoped; ownership is checked here
    // so another user's list is a plain 404, never a 409/other leak.
    if list.user_id != user_id {
        return Err(ListsError::NotFound);
    }

    let Some(updated) = repo.update(id, updates).await? else {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

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

    // ──────────────────────────────────────────
    // Seeding
    // ──────────────────────────────────────────

    #[test]
    fn first_get_seeds_four_default_lists() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let response = pollster::block_on(list_lists(&repo, "u-1")).unwrap();

        let names: Vec<&str> = response.lists.iter().map(|list| list.name.as_str()).collect();
        assert_eq!(names, ["Work", "Fitness", "Family", "Personal"]);
        let orders: Vec<i64> = response.lists.iter().map(|list| list.sort_order).collect();
        assert_eq!(orders, [0, 1, 2, 3]);
        let colors: Vec<&str> = response.lists.iter().map(|list| list.color.as_str()).collect();
        assert_eq!(colors, ["#2a5c8a", "#c45a2c", "#7a4a6a", "#3a7a5a"]);
        assert!(response.lists.iter().all(|list| list.user_id == "u-1"));
    }

    #[test]
    fn second_get_does_not_reseed() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let first = pollster::block_on(list_lists(&repo, "u-1")).unwrap();
        let second = pollster::block_on(list_lists(&repo, "u-1")).unwrap();

        assert_eq!(first.lists.len(), 4);
        assert_eq!(second.lists.len(), 4, "no duplicate seeding");
        assert_eq!(repo.inserted.lock().unwrap().len(), 4, "exactly one seed");
    }

    #[test]
    fn list_lists_sorts_by_sort_order_then_name() {
        let repo = FakeTaskListRepo::with(vec![
            FakeTaskListRepo::row("l-a", "u-1", "Zzz", "#000000", 3),
            FakeTaskListRepo::row("l-b", "u-1", "Alpha", "#000000", 1),
            FakeTaskListRepo::row("l-c", "u-1", "Beta", "#000000", 1),
            FakeTaskListRepo::row("l-d", "u-2", "Other", "#000000", 0),
        ]);
        let response = pollster::block_on(list_lists(&repo, "u-1")).unwrap();

        let ids: Vec<&str> = response.lists.iter().map(|list| list.id.as_str()).collect();
        assert_eq!(ids, ["l-b", "l-c", "l-a"]);
        assert_eq!(repo.inserted.lock().unwrap().len(), 0, "lists exist — no seed");
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_list_trims_name_and_color() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let response = pollster::block_on(create_list(&repo, "u-1", "  Work?  ", "  #2a5c8a  "))
            .unwrap();
        assert_eq!(response.list.name, "Work?");
        assert_eq!(response.list.color, "#2a5c8a");
        assert_eq!(response.list.sort_order, 0);
        assert_eq!(response.list.user_id, "u-1");
    }

    #[test]
    fn create_list_rejects_empty_or_whitespace_name() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let empty = pollster::block_on(create_list(&repo, "u-1", "", "#2a5c8a"));
        assert!(matches!(empty, Err(ListsError::Invalid(m)) if m == "name must not be empty"));
        let blank = pollster::block_on(create_list(&repo, "u-1", "   ", "#2a5c8a"));
        assert!(matches!(blank, Err(ListsError::Invalid(_))));
        assert!(repo.inserted.lock().unwrap().is_empty(), "nothing persisted");
    }

    #[test]
    fn create_list_rejects_empty_color() {
        let repo = FakeTaskListRepo::with(Vec::new());
        let empty = pollster::block_on(create_list(&repo, "u-1", "Work", ""));
        assert!(matches!(empty, Err(ListsError::Invalid(m)) if m == "color must not be empty"));
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

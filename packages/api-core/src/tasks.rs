//! Task service: CRUD for `tasks`, classified by title regex.
//!
//! Pure Rust and unit-testable: persistence goes through [`TaskRepo`]/
//! [`TaskCategoryRepo`]/[`TaskListRepo`] (faked with in-memory impls in tests).
//! The Worker layers session checks on top (`apps/worker/src/tasks.rs`).
//!
//! Domain rules (locked):
//! - Tasks carry NO `category_id`/`list_id`. The category is **computed** per
//!   title via [`crate::categories::classify`] with `event_google_calendar_id
//!   = None` (calendar-scoped patterns never match task titles).
//! - Create/update reject a title that does not uniquely match a non-untracked
//!   category (400). `Untracked { conflict: false }` (0 matches),
//!   `Untracked { conflict: true }` (cross-tree conflict), and a match on the
//!   `untracked` sink itself are all invalid. A title matching only a root
//!   whose children do not match is **allowed** (parent remainder).
//! - Titles are not unique. Create always stores `status = "OPEN"`; this slice
//!   has no status transitions (no complete/discard/in_progress) and
//!   `UpdateTask` has no `status` field.
//! - `duration_minutes` defaults to 15 and must be >= 1; `priority` must be
//!   `high|medium|low` (default `medium`).
//! - Delete is SOFT; a missing/soft-deleted/other-user task is always
//!   [`TasksError::NotFound`].
//! - List returns each living task with its **computed** category
//!   ([`TaskCategorySummary`]) so the client never reimplements the regex
//!   matching. A task whose title no longer matches anything (e.g. the user
//!   deleted the pattern) is still returned with the `untracked` summary —
//!   listing is a read, not a validation.
//!
//! Taxonomy seeding: `list_tasks` and `create_task` run `ensure_taxonomy`
//! (lists count + categories count) so a tasks-only client still gets a
//! matcher; the gate keeps the first Lists visit order (lists → categories →
//! tasks) safe.

use thiserror::Error;

use crate::categories::{ensure_taxonomy, classify, CategoryWithPatterns, ClassifyOutcome};
use crate::models::{NewTask, NewTaskInput, Task, TaskCategory, UpdateTask};
use crate::repo::{RepoError, TaskCategoryRepo, TaskListRepo, TaskRepo};

/// Default planned duration for a new task, in minutes.
pub const DEFAULT_DURATION_MINUTES: i64 = 15;
/// Minimum planned duration, in minutes.
pub const MIN_DURATION_MINUTES: i64 = 1;
/// The status every created task gets (this slice has no other states).
pub const TASK_STATUS_OPEN: &str = "OPEN";

/// Errors produced by the tasks service.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TasksError {
    #[error("{0}")]
    Invalid(String),
    #[error("task not found")]
    NotFound,
    #[error("database error: {0}")]
    Repo(#[from] RepoError),
}

/// `list_tasks`/`create_task` run `ensure_taxonomy`, whose errors fold into
/// [`TasksError`] (same HTTP mapping as the lists path: 400 / 404 / 500; a
/// taxonomy Conflict is a domain violation, so it surfaces as a 400 Invalid).
impl From<crate::categories::CategoriesError> for TasksError {
    fn from(err: crate::categories::CategoriesError) -> Self {
        match err {
            crate::categories::CategoriesError::Invalid(message) => TasksError::Invalid(message),
            crate::categories::CategoriesError::NotFound => TasksError::NotFound,
            crate::categories::CategoriesError::Conflict(message) => TasksError::Invalid(message),
            crate::categories::CategoriesError::Repo(err) => TasksError::Repo(err),
        }
    }
}

/// Response envelope for `GET /api/tasks`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TasksResponse {
    pub tasks: Vec<TaskView>,
}

/// Response envelope for `POST /api/tasks` and `PATCH /api/tasks/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskResponse {
    pub task: TaskView,
}

/// Response envelope for `DELETE /api/tasks/:id`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeleteTaskResponse {
    pub success: bool,
}

/// HTTP shape of a task: every `tasks` column (minus `deleted_at`) plus the
/// **computed** category summary — the client groups tasks by `category.id`
/// without reimplementing the matcher.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskView {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub description: String,
    pub duration_minutes: i64,
    pub priority: String,
    pub status: String,
    /// RFC 3339 instant.
    pub created_at: String,
    /// RFC 3339 instant.
    pub updated_at: String,
    pub category: TaskCategorySummary,
}

/// Slimmer category attached to a [`TaskView`]: the fields the Lists page
/// needs to show a task under its category (full `CategoryView` pulls in the
/// pattern rows, which a task list does not need).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskCategorySummary {
    pub id: String,
    pub title: String,
    pub slug: String,
    /// Stored column; `None` for children and `untracked`.
    pub list_id: Option<String>,
    /// Root's own `list_id`, or the parent root's `list_id` for children.
    pub inherited_list_id: Option<String>,
    pub is_untracked: bool,
    pub color: String,
}

/// All living categories plus the matcher set built from their patterns, in
/// one round-trip pair — the unit of work for every classify here.
struct Taxonomy {
    categories: Vec<TaskCategory>,
    matchers: Vec<CategoryWithPatterns>,
}

/// Lists the user's living tasks with their computed category summaries.
///
/// Seeding: like `list_lists`, this runs `ensure_taxonomy` (when the user has
/// zero living lists OR zero living categories) so a tasks-only client still
/// has a matcher. The count-gate keeps the sequential first-visit order
/// (lists → categories → tasks) safe: by the time the frontend's third fetch
/// lands, taxonomy exists and this is a no-op.
///
/// A task whose title now classifies as `untracked` (e.g. the user deleted the
/// matching pattern) is still returned, with the `untracked` summary attached
/// — never a 400 from a read.
pub async fn list_tasks(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
) -> Result<TasksResponse, TasksError> {
    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    // Re-load: the seed may have inserted lists/categories.
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    let tasks = task_repo.list_by_user_id(user_id).await?;
    let views = tasks
        .iter()
        .map(|task| to_view(task, &taxonomy))
        .collect();
    Ok(TasksResponse { tasks: views })
}

/// Creates a task (always `status = "OPEN"`).
///
/// Validation (all 400):
/// - `title` must not be blank, and must classify to a single non-untracked
///   category: 0 matches → `title does not match a category`, several matches
///   → `title matches multiple categories`, a match on the `untracked` sink →
///   `title matches untracked`.
/// - `duration_minutes` defaults to [`DEFAULT_DURATION_MINUTES`] and must be
///   at least [`MIN_DURATION_MINUTES`].
/// - `priority` defaults to `medium` and must be `high|medium|low`.
///
/// `ensure_taxonomy` runs first so the very first task of a fresh user finds
/// a seeded matcher.
pub async fn create_task(
    list_repo: &dyn TaskListRepo,
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    input: &NewTaskInput,
) -> Result<TaskResponse, TasksError> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(TasksError::Invalid("title must not be empty".to_string()));
    }
    let duration_minutes = input.duration_minutes.unwrap_or(DEFAULT_DURATION_MINUTES);
    if duration_minutes < MIN_DURATION_MINUTES {
        return Err(TasksError::Invalid("duration must be at least 1 minute".to_string()));
    }
    let priority = input
        .priority
        .as_deref()
        .unwrap_or("medium")
        .to_string();
    if !is_valid_priority(&priority) {
        return Err(TasksError::Invalid(
            "priority must be one of high, medium, low".to_string(),
        ));
    }

    let lists = list_repo.list_by_user_id(user_id).await?;
    let categories = category_repo.list_by_user_id(user_id).await?;
    ensure_taxonomy(list_repo, category_repo, &lists, &categories, user_id).await?;
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    // Validates the rules; the response view re-classifies the stored title.
    resolve_category(&title, &taxonomy)?;

    let task = task_repo
        .insert(NewTask {
            user_id: user_id.to_string(),
            title,
            description: input.description.as_deref().unwrap_or("").trim().to_string(),
            duration_minutes,
            priority,
        })
        .await?;
    Ok(TaskResponse {
        task: to_view(&task, &taxonomy),
    })
}

/// Updates a task's `title`/`description`/`duration_minutes`/`priority`
/// (`None` = unchanged; status is never updated this slice).
///
/// - A body with nothing to update is 400.
/// - When `title` is present it must be non-blank AND uniquely match a
///   non-untracked category (same rules as create, same messages).
/// - `duration_minutes` must be >= 1 when present; `priority` must be
///   `high|medium|low` when present.
/// - A missing, soft-deleted, or another user's task is 404.
pub async fn update_task(
    category_repo: &dyn TaskCategoryRepo,
    task_repo: &dyn TaskRepo,
    user_id: &str,
    id: &str,
    updates: &UpdateTask,
) -> Result<TaskResponse, TasksError> {
    if updates.title.is_none()
        && updates.description.is_none()
        && updates.duration_minutes.is_none()
        && updates.priority.is_none()
    {
        return Err(TasksError::Invalid("nothing to update".to_string()));
    }
    if let Some(duration_minutes) = updates.duration_minutes {
        if duration_minutes < MIN_DURATION_MINUTES {
            return Err(TasksError::Invalid("duration must be at least 1 minute".to_string()));
        }
    }
    if let Some(priority) = updates.priority.as_deref() {
        if !is_valid_priority(priority) {
            return Err(TasksError::Invalid(
                "priority must be one of high, medium, low".to_string(),
            ));
        }
    }

    let Some(task) = task_repo.get_by_id(id).await? else {
        return Err(TasksError::NotFound);
    };
    // `get_by_id` is intentionally not user-scoped; ownership is checked here
    // so another user's task is a plain 404, never a leak.
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }

    // A new title must classify to a single non-untracked category, exactly
    // like create. The old title needs no re-validation.
    if let Some(title) = updates.title.as_deref() {
        let title = title.trim();
        if title.is_empty() {
            return Err(TasksError::Invalid("title must not be empty".to_string()));
        }
        let taxonomy = load_taxonomy(category_repo, user_id).await?;
        resolve_category(title, &taxonomy)?;
    }

    let Some(updated) = task_repo.update(id, updates).await? else {
        // Deleted between the read and the write.
        return Err(TasksError::NotFound);
    };
    let taxonomy = load_taxonomy(category_repo, user_id).await?;
    Ok(TaskResponse {
        task: to_view(&updated, &taxonomy),
    })
}

/// SOFT deletes a task.
///
/// - Missing, soft-deleted, or another user's task → [`TasksError::NotFound`]
///   (404) — ownership is never leaked.
/// - Otherwise the row is stamped with `deleted_at = now_rfc3339` and
///   `{"success": true}` is returned.
pub async fn delete_task(
    task_repo: &dyn TaskRepo,
    user_id: &str,
    id: &str,
    now_rfc3339: &str,
) -> Result<DeleteTaskResponse, TasksError> {
    let Some(task) = task_repo.get_by_id(id).await? else {
        return Err(TasksError::NotFound);
    };
    if task.user_id != user_id {
        return Err(TasksError::NotFound);
    }
    task_repo.soft_delete(id, now_rfc3339).await?;
    Ok(DeleteTaskResponse { success: true })
}

// ──────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────

fn is_valid_priority(priority: &str) -> bool {
    matches!(priority, "high" | "medium" | "low")
}

fn to_view(task: &Task, taxonomy: &Taxonomy) -> TaskView {
    let outcome = classify(&task.title, None, &taxonomy.matchers);
    let category = match &outcome {
        ClassifyOutcome::Matched { category_id } => taxonomy
            .categories
            .iter()
            .find(|category| category.id == *category_id)
            // A task matched a category that vanished between the pattern
            // build and here (impossible in one run, but never panic on read):
            // fall back to the untracked sink.
            .map(|category| summary_for(category, &taxonomy.categories))
            .or_else(|| untracked_summary(&taxonomy.categories)),
        ClassifyOutcome::Untracked { .. } => untracked_summary(&taxonomy.categories),
    };
    let category = category.expect("ensure_taxonomy guarantees the untracked sink exists");
    TaskView {
        id: task.id.clone(),
        user_id: task.user_id.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        duration_minutes: task.duration_minutes,
        priority: task.priority.clone(),
        status: task.status.clone(),
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        category,
    }
}

/// The `untracked` sink summary; `None` only before the first seed (callers
/// run `ensure_taxonomy` first, and the sink is undeletable).
fn untracked_summary(categories: &[TaskCategory]) -> Option<TaskCategorySummary> {
    categories
        .iter()
        .find(|category| category.is_untracked)
        .map(|category| summary_for(category, categories))
}

fn summary_for(category: &TaskCategory, categories: &[TaskCategory]) -> TaskCategorySummary {
    // Inherited list: a child takes the parent root's list_id; a root takes
    // its own (untracked has none).
    let inherited_list_id = match category.parent_id.as_deref() {
        Some(parent_id) => categories
            .iter()
            .find(|entry| entry.id == parent_id)
            .and_then(|parent| parent.list_id.clone()),
        None => category.list_id.clone(),
    };
    TaskCategorySummary {
        id: category.id.clone(),
        title: category.title.clone(),
        slug: category.slug.clone(),
        list_id: category.list_id.clone(),
        inherited_list_id,
        is_untracked: category.is_untracked,
        color: category.color.clone(),
    }
}

/// Resolves a title to a single non-untracked category id, enforcing the
/// locked create/update rules:
/// - 0 matches → `title does not match a category`
/// - several matches (cross-tree conflict) → `title matches multiple categories`
/// - the matched category is the `untracked` sink → `title matches untracked`
///
/// A title matching only a root whose children do not match (parent
/// remainder) resolves to the root — `classify` already drops parents beaten
/// by their own children, so a single leftover match is always OK here.
fn resolve_category(title: &str, taxonomy: &Taxonomy) -> Result<String, TasksError> {
    match classify(title, None, &taxonomy.matchers) {
        ClassifyOutcome::Matched { category_id } => {
            if taxonomy
                .categories
                .iter()
                .any(|category| category.id == category_id && category.is_untracked)
            {
                return Err(TasksError::Invalid("title matches untracked".to_string()));
            }
            Ok(category_id)
        }
        ClassifyOutcome::Untracked { conflict: false } => {
            Err(TasksError::Invalid("title does not match a category".to_string()))
        }
        ClassifyOutcome::Untracked { conflict: true } => {
            Err(TasksError::Invalid("title matches multiple categories".to_string()))
        }
    }
}

/// Loads the living categories AND their patterns in one pass, mirroring the
/// two queries `list_categories` performs per category (list + patterns).
async fn load_taxonomy(
    category_repo: &dyn TaskCategoryRepo,
    user_id: &str,
) -> Result<Taxonomy, RepoError> {
    let categories = category_repo.list_by_user_id(user_id).await?;
    let mut matchers = Vec::with_capacity(categories.len());
    for category in &categories {
        let patterns = category_repo.list_patterns_by_category_id(&category.id).await?;
        matchers.push(CategoryWithPatterns {
            category_id: category.id.clone(),
            parent_id: category.parent_id.clone(),
            patterns,
        });
    }
    Ok(Taxonomy { categories, matchers })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::models::{
        NewTaskCategory, NewTaskCategoryInput, NewTaskCategoryPattern, NewTaskList, TaskCategory,
        TaskCategoryPattern, TaskList, UpdateTaskCategory, UpdateTaskList,
    };
    use crate::repo::TaskCategoryRepo;

    // ──────────────────────────────────────────
    // Fakes
    // ──────────────────────────────────────────

    /// In-memory `TaskRepo` mirroring D1 semantics: soft-deleted rows are
    /// filtered from reads, updates are partial (COALESCE).
    struct FakeTaskRepo {
        stored: Mutex<Vec<Task>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl TaskRepo for FakeTaskRepo {
        async fn list_by_user_id(&self, user_id: &str) -> Result<Vec<Task>, RepoError> {
            let mut rows: Vec<Task> = self
                .stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.deleted_at.is_none())
                .cloned()
                .collect();
            // Mirrors TASK_LIST_BY_USER_ID_SQL (stable sort ties by id).
            rows.sort_by(|a, b| {
                (b.updated_at.as_str(), b.created_at.as_str(), &b.id)
                    .cmp(&(a.updated_at.as_str(), a.created_at.as_str(), &a.id))
            });
            Ok(rows)
        }

        async fn get_by_id(&self, id: &str) -> Result<Option<Task>, RepoError> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|row| row.id == id && row.deleted_at.is_none())
                .cloned())
        }

        async fn insert(&self, task: NewTask) -> Result<Task, RepoError> {
            let mut next = self.next_id.lock().unwrap();
            let row = Task {
                id: format!("task-{next}"),
                user_id: task.user_id.clone(),
                title: task.title.clone(),
                description: task.description.clone(),
                duration_minutes: task.duration_minutes,
                priority: task.priority.clone(),
                status: TASK_STATUS_OPEN.to_string(),
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
            self.stored.lock().unwrap().push(row.clone());
            Ok(row)
        }

        async fn update(
            &self,
            id: &str,
            updates: &UpdateTask,
        ) -> Result<Option<Task>, RepoError> {
            let mut stored = self.stored.lock().unwrap();
            let Some(row) = stored
                .iter_mut()
                .find(|row| row.id == id && row.deleted_at.is_none())
            else {
                return Ok(None);
            };
            if let Some(title) = &updates.title {
                row.title = title.clone();
            }
            if let Some(description) = &updates.description {
                row.description = description.clone();
            }
            if let Some(duration_minutes) = updates.duration_minutes {
                row.duration_minutes = duration_minutes;
            }
            if let Some(priority) = &updates.priority {
                row.priority = priority.clone();
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
    }

    /// In-memory `TaskListRepo` for seeding via `list_lists`.
    struct FakeTaskListRepo {
        stored: Mutex<Vec<TaskList>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskListRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
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
                user_id: list.user_id,
                name: list.name,
                color: list.color,
                sort_order: list.sort_order,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
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

    /// In-memory `TaskCategoryRepo` with real insert/pattern replacement so
    /// `ensure_taxonomy` and child creation work end to end.
    struct FakeTaskCategoryRepo {
        stored: Mutex<Vec<TaskCategory>>,
        patterns: Mutex<HashMap<String, Vec<TaskCategoryPattern>>>,
        next_id: Mutex<u64>,
    }

    impl FakeTaskCategoryRepo {
        fn new() -> Self {
            Self {
                stored: Mutex::new(Vec::new()),
                patterns: Mutex::new(HashMap::new()),
                next_id: Mutex::new(1),
            }
        }

        /// Ids of the user's living categories matching a predicate.
        fn ids(&self, user_id: &str, is_untracked: bool) -> Vec<String> {
            self.stored
                .lock()
                .unwrap()
                .iter()
                .filter(|row| row.user_id == user_id && row.is_untracked == is_untracked)
                .map(|row| row.id.clone())
                .collect()
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
                user_id: category.user_id,
                list_id: category.list_id,
                parent_id: category.parent_id,
                title: category.title,
                slug: category.slug,
                color: category.color,
                is_productive: category.is_productive,
                google_calendar_id: category.google_calendar_id,
                google_color_id: category.google_color_id,
                sort_order: category.sort_order,
                is_untracked: category.is_untracked,
                created_at: "2026-08-18T00:00:00Z".to_string(),
                updated_at: "2026-08-18T00:00:00Z".to_string(),
                deleted_at: None,
            };
            *next += 1;
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

    // ──────────────────────────────────────────
    // Helpers
    // ──────────────────────────────────────────

    fn pattern(regex: &str) -> NewTaskCategoryPattern {
        NewTaskCategoryPattern {
            regex: regex.to_string(),
            google_calendar_id: None,
        }
    }

    fn input(title: &str) -> NewTaskInput {
        NewTaskInput {
            title: title.to_string(),
            description: None,
            duration_minutes: None,
            priority: None,
        }
    }

    /// Seeds a fresh user's taxonomy (4 lists, 4 roots + untracked, 8
    /// patterns) through the exact path the app uses, returning the repos.
    fn seeded() -> (FakeTaskListRepo, FakeTaskCategoryRepo, FakeTaskRepo) {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        pollster::block_on(crate::list_lists(&lists, &categories, "u-1")).unwrap();
        (lists, categories, tasks)
    }

    fn category_ids_by_slug(repo: &FakeTaskCategoryRepo) -> HashMap<String, String> {
        repo.stored
            .lock()
            .unwrap()
            .iter()
            .filter(|row| row.user_id == "u-1" && row.deleted_at.is_none())
            .filter_map(|row| Some((row.slug.clone(), row.id.clone())))
            .collect()
    }

    // ──────────────────────────────────────────
    // Create
    // ──────────────────────────────────────────

    #[test]
    fn create_matches_root_exact_title_and_pipe_suffix() {
        let (lists, categories, tasks) = seeded();
        let work = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(work.task.status, "OPEN");
        assert_eq!(work.task.duration_minutes, 15, "duration defaults to 15");
        assert_eq!(work.task.priority, "medium", "priority defaults to medium");
        assert_eq!(work.task.category.title, "Work");
        assert!(!work.task.category.is_untracked);
        assert!(work.task.category.inherited_list_id.is_some(), "root owns a list");

        let review = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Review Q3 | Work"),
        ))
        .unwrap();
        assert_eq!(review.task.category.title, "Work");
        assert_ne!(work.task.id, review.task.id, "titles are not unique");
        assert_eq!(work.task.category.id, review.task.category.id);
    }

    #[test]
    fn create_unmatched_title_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("asdf"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title does not match a category"));
        assert_eq!(tasks.stored.lock().unwrap().len(), 0, "nothing persisted");
    }

    #[test]
    fn create_title_matching_two_roots_is_invalid() {
        let (lists, categories, tasks) = seeded();
        // Give Fitness the same pattern as Work so "Work" matches two roots.
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(
            &ids["fitness"],
            vec![pattern("^Work$")],
        ))
        .unwrap();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title matches multiple categories"));
    }

    #[test]
    fn create_title_matching_untracked_is_invalid() {
        let (lists, categories, tasks) = seeded();
        // The sink normally has no patterns; give it one to prove the guard.
        let untracked_id = categories.ids("u-1", true).pop().unwrap();
        pollster::block_on(categories.replace_patterns(
            &untracked_id,
            vec![pattern("^Untracked$")],
        ))
        .unwrap();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Untracked"),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title matches untracked"));
    }

    #[test]
    fn create_parent_remainder_is_allowed() {
        let (lists, categories, tasks) = seeded();
        // A child whose own pattern does not match the parent's title.
        let ids = category_ids_by_slug(&categories);
        let child = pollster::block_on(crate::categories::create_category(
            &categories,
            &lists,
            "u-1",
            &NewTaskCategoryInput {
                title: "Coding".to_string(),
                slug: None,
                color: "#2a5c8a".to_string(),
                is_productive: None,
                google_calendar_id: None,
                google_color_id: None,
                list_id: None,
                parent_id: Some(ids["work"].clone()),
                sort_order: None,
                is_untracked: None,
                patterns: vec![pattern("^Code$")],
            },
        ))
        .unwrap()
        .category;

        // "Work" matches only the root (parent remainder) → allowed, root.
        let work = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(work.task.category.id, ids["work"]);
        assert_eq!(work.task.category.inherited_list_id, categories.stored.lock().unwrap().iter().find(|c| c.id == ids["work"]).unwrap().list_id);

        // "Code" matches only the child → allowed, child (inherits the root's
        // list).
        let code = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Code"),
        ))
        .unwrap();
        assert_eq!(code.task.category.id, child.id);
        assert_eq!(code.task.category.inherited_list_id, work.task.category.inherited_list_id);
    }

    #[test]
    fn create_validates_duration_and_priority() {
        let (lists, categories, tasks) = seeded();
        let zero_duration = NewTaskInput {
            title: "Work".to_string(),
            description: None,
            duration_minutes: Some(0),
            priority: None,
        };
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &zero_duration,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "duration must be at least 1 minute"));

        let bad_priority = NewTaskInput {
            title: "Work".to_string(),
            description: None,
            duration_minutes: None,
            priority: Some("urgent".to_string()),
        };
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &bad_priority,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "priority must be one of high, medium, low"));
        assert_eq!(tasks.stored.lock().unwrap().len(), 0, "nothing persisted");
    }

    #[test]
    fn create_blank_title_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let err = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("   "),
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title must not be empty"));
    }

    #[test]
    fn first_create_seeds_taxonomy_so_fresh_user_can_file_tasks() {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        // No prior visit to /api/lists — create must seed internally.
        let response = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap();
        assert_eq!(response.task.category.title, "Work");
        assert_eq!(
            pollster::block_on(categories.count_by_user_id("u-1")).unwrap(),
            5,
            "four roots + untracked seeded"
        );
    }

    // ──────────────────────────────────────────
    // Update
    // ──────────────────────────────────────────

    #[test]
    fn update_title_to_unmatched_is_invalid() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            title: Some("asdf".to_string()),
            ..UpdateTask::default()
        };
        let err = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap_err();
        assert!(matches!(err, TasksError::Invalid(m) if m == "title does not match a category"));
        // The row is untouched.
        let stored = pollster::block_on(tasks.get_by_id(&created.id)).unwrap().unwrap();
        assert_eq!(stored.title, "Work");
    }

    #[test]
    fn update_title_to_another_category_recomputes_category() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Review | Work"),
        ))
        .unwrap()
        .task;
        assert_eq!(created.category.title, "Work");
        let updates = UpdateTask {
            title: Some("Fitness".to_string()),
            ..UpdateTask::default()
        };
        let response = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap();
        assert_eq!(response.task.category.title, "Fitness");
        assert_eq!(response.task.status, "OPEN", "status untouched");
    }

    #[test]
    fn update_without_title_preserves_category_and_applies_fields() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            title: None,
            description: Some("Deep focus session".to_string()),
            duration_minutes: Some(25),
            priority: Some("high".to_string()),
        };
        let response = pollster::block_on(update_task(
            &categories, &tasks, "u-1", &created.id, &updates,
        ))
        .unwrap();
        assert_eq!(response.task.description, "Deep focus session");
        assert_eq!(response.task.duration_minutes, 25);
        assert_eq!(response.task.priority, "high");
        assert_eq!(response.task.category.id, created.category.id, "category preserved");
    }

    #[test]
    fn update_rejects_empty_body_bad_duration_and_bad_priority() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &UpdateTask::default())),
            Err(TasksError::Invalid(m)) if m == "nothing to update"
        ));
        let bad_duration = UpdateTask {
            duration_minutes: Some(0),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &bad_duration)),
            Err(TasksError::Invalid(m)) if m == "duration must be at least 1 minute"
        ));
        let bad_priority = UpdateTask {
            priority: Some("urgent".to_string()),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", &created.id, &bad_priority)),
            Err(TasksError::Invalid(m)) if m == "priority must be one of high, medium, low"
        ));
    }

    #[test]
    fn update_is_not_found_for_missing_and_other_users_task() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let updates = UpdateTask {
            description: Some("x".to_string()),
            ..UpdateTask::default()
        };
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-2", &created.id, &updates)),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(update_task(&categories, &tasks, "u-1", "nope", &updates)),
            Err(TasksError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // Delete
    // ──────────────────────────────────────────

    #[test]
    fn delete_soft_deletes_and_404s_on_second_try() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        let response = pollster::block_on(delete_task(
            &tasks, "u-1", &created.id, "2026-08-18T02:00:00Z",
        ))
        .unwrap();
        assert!(response.success);
        assert_eq!(
            pollster::block_on(tasks.list_by_user_id("u-1")).unwrap().len(),
            0,
            "no living tasks remain"
        );
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-1", &created.id, "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
    }

    #[test]
    fn delete_is_not_found_for_missing_and_other_users_task() {
        let (lists, categories, tasks) = seeded();
        let created = pollster::block_on(create_task(
            &lists, &categories, &tasks, "u-1", &input("Work"),
        ))
        .unwrap()
        .task;
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-2", &created.id, "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
        assert!(matches!(
            pollster::block_on(delete_task(&tasks, "u-1", "nope", "2026-08-18T02:00:00Z")),
            Err(TasksError::NotFound)
        ));
    }

    // ──────────────────────────────────────────
    // List
    // ──────────────────────────────────────────

    #[test]
    fn list_includes_computed_category_per_task() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Dinner | Family"))).unwrap();

        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert_eq!(response.tasks.len(), 2);
        let by_title = |title: &str| response.tasks.iter().find(|t| t.title == title).unwrap();
        assert_eq!(by_title("Work").category.title, "Work");
        assert_eq!(by_title("Dinner | Family").category.title, "Family");
        assert!(response.tasks.iter().all(|t| t.status == "OPEN"));
    }

    #[test]
    fn list_keeps_tasks_that_no_longer_match_under_untracked() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        // The user deletes Work's patterns: the task still lists, as untracked.
        let ids = category_ids_by_slug(&categories);
        pollster::block_on(categories.replace_patterns(&ids["work"], vec![])).unwrap();

        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert_eq!(response.tasks.len(), 1, "read never drops tasks");
        assert!(response.tasks[0].category.is_untracked);
        assert_eq!(response.tasks[0].category.title, "Untracked");
    }

    #[test]
    fn list_seeds_taxonomy_for_tasks_only_client() {
        let lists = FakeTaskListRepo::new();
        let categories = FakeTaskCategoryRepo::new();
        let tasks = FakeTaskRepo::new();
        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-1")).unwrap();
        assert!(response.tasks.is_empty());
        assert_eq!(
            pollster::block_on(categories.count_by_user_id("u-1")).unwrap(),
            5,
            "tasks-only client still gets a matcher"
        );
    }

    #[test]
    fn list_is_scoped_to_the_user() {
        let (lists, categories, tasks) = seeded();
        pollster::block_on(create_task(&lists, &categories, &tasks, "u-1", &input("Work"))).unwrap();
        let response = pollster::block_on(list_tasks(&lists, &categories, &tasks, "u-2")).unwrap();
        assert!(response.tasks.is_empty());
    }
}

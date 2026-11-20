//! `GET /api/board/planned` — the date-grouped Planned Board read (ADR 0014).
//!
//! The Planned column is rendered as date groups: every living `PLANNED` task
//! is either **dated** (it holds an `agenda_items` row before the planning
//! horizon) or **undated** (rowless, or its row sits at/after the horizon —
//! ADR 0013's "undated is a region" rule). The grouping is computed here, on
//! the server, so the browser never re-derives it from raw `local_date`
//! strings.
//!
//! Pure Rust: persistence goes through [`TaskRepo`]/[`AgendaItemRepo`]/
//! [`CalendarRepo`], and "now" comes from the caller — never `SystemTime`.

use std::collections::HashMap;

use crate::agenda::{planning_horizon_date, user_today, AgendaError, AGENDA_KIND_TASK};
use crate::models::{AgendaItem, Task};
use crate::repo::{AgendaItemRepo, CalendarRepo, TaskRepo};
use crate::tasks::TASK_STATUS_PLANNED;

/// Response envelope for `GET /api/board/planned`: the server-computed civil
/// `today`/`time_zone` (same "home zone" contract as `GET /api/agenda`) and
/// one row per living `PLANNED` task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlannedBoardResponse {
    /// `YYYY-MM-DD` — civil today in `time_zone`.
    pub today: String,
    /// IANA name of the zone that produced `today` (the primary calendar's
    /// `time_zone`, or `UTC` when the user has no primary calendar).
    pub time_zone: String,
    /// Dated rows first, then undated rows (see [`PlannedBoardRow`]).
    pub rows: Vec<PlannedBoardRow>,
}

/// One living `PLANNED` task on the date-grouped Planned column.
///
/// `item_id`/`local_date`/`agenda_sort_order` come from the task's single
/// `agenda_items` row of kind `task`; all three are `None` when the task is
/// rowless. `board_sort_order` is always the task's own `tasks.sort_order`.
///
/// `undated` is **server-computed**: `true` when the task has no agenda row,
/// or its `local_date` is at/after the `today + 10 years` planning horizon.
/// A dated row sorts by `(local_date, agenda_sort_order, task_id)`; an undated
/// row sorts by `(board_sort_order, task_id)`. Every dated row precedes every
/// undated row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PlannedBoardRow {
    /// `tasks.id`.
    pub task_id: String,
    /// `agenda_items.id`, or `None` when the task is rowless.
    pub item_id: Option<String>,
    /// The agenda row's local civil date `YYYY-MM-DD`, or `None` when rowless.
    pub local_date: Option<String>,
    /// The agenda row's rank inside its date pile, or `None` when rowless.
    pub agenda_sort_order: Option<i64>,
    /// The task's Planned-column board rank (`tasks.sort_order`) — always
    /// present, used for the undated region's order.
    pub board_sort_order: i64,
    /// ADR 0013: `true` for rowless tasks and for rows at/after the horizon.
    pub undated: bool,
}

/// `GET /api/board/planned` → every living `PLANNED` task of `user_id`, with
/// its agenda row (if any) and the server-computed undated flag.
///
/// Reads: one `list_by_user_id` for the tasks, one batched `list_by_refs` for
/// their agenda rows (`kind = task`, any date), one `list_by_user_id` inside
/// [`user_today`] for the primary calendar's zone. No writes.
pub async fn get_planned_board(
    calendars: &dyn CalendarRepo,
    tasks: &dyn TaskRepo,
    agenda: &dyn AgendaItemRepo,
    user_id: &str,
    now_unix: i64,
) -> Result<PlannedBoardResponse, AgendaError> {
    let (today, time_zone) = user_today(calendars, user_id, now_unix).await?;
    let horizon = planning_horizon_date(&today)?;

    // One row per living PLANNED task. `list_by_user_id` already filters
    // soft-deleted rows; the status filter is the Planned column.
    let planned: Vec<Task> = tasks
        .list_by_user_id(user_id)
        .await?
        .into_iter()
        .filter(|task| task.status == TASK_STATUS_PLANNED)
        .collect();

    // One batched membership read for every planned task, on any date. The
    // `(user_id, ref_id) WHERE kind = 'task'` invariant means at most one row
    // per task; `or_insert` keeps this read deterministic even if a
    // hand-edited duplicate ever slips in.
    let ref_ids: Vec<String> = planned.iter().map(|task| task.id.clone()).collect();
    let mut item_by_ref: HashMap<String, AgendaItem> = HashMap::new();
    for item in agenda
        .list_by_refs(user_id, AGENDA_KIND_TASK, &ref_ids)
        .await?
    {
        if item.user_id != user_id || item.kind != AGENDA_KIND_TASK {
            continue;
        }
        item_by_ref.entry(item.ref_id.clone()).or_insert(item);
    }

    let mut dated_rows: Vec<PlannedBoardRow> = Vec::new();
    let mut undated_rows: Vec<PlannedBoardRow> = Vec::new();
    for task in planned {
        let item = item_by_ref.get(&task.id);
        let local_date = item.map(|row| row.local_date.clone());
        let undated = match local_date.as_deref() {
            // The 10-year line is inclusive: `today + 10y` itself is already
            // the undated region (ADR 0007/0013). `YYYY-MM-DD` sorts
            // chronologically, so the string compare IS the civil compare.
            Some(date) => date >= horizon.as_str(),
            None => true,
        };
        let row = PlannedBoardRow {
            task_id: task.id,
            item_id: item.map(|row| row.id.clone()),
            local_date,
            agenda_sort_order: item.map(|row| row.sort_order),
            board_sort_order: task.sort_order,
            undated,
        };
        if undated {
            undated_rows.push(row);
        } else {
            dated_rows.push(row);
        }
    }

    dated_rows.sort_by(|a, b| {
        a.local_date
            .as_deref()
            .cmp(&b.local_date.as_deref())
            .then_with(|| a.agenda_sort_order.cmp(&b.agenda_sort_order))
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    undated_rows.sort_by(|a, b| {
        a.board_sort_order
            .cmp(&b.board_sort_order)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });

    // Every dated row precedes every undated row: the agenda order is the
    // date-grouped column's spine, then the warehouse region.
    dated_rows.extend(undated_rows);
    Ok(PlannedBoardResponse {
        today,
        time_zone,
        rows: dated_rows,
    })
}

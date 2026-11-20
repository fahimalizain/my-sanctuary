# ADR 0014: A dated Planned column on the Board

Status: Accepted
Date: 2026-11-20

> Amends ADR 0002 § Move API and § DnD for the Board's Planned column only.
> ADR 0013 remains the source of truth for the status/date split; this ADR
> makes the Board's Planned column read and write that split. Every other
> column keeps its flat `tasks.sort_order` behavior.

## Context

The Board's Planned column is one flat list ordered by `tasks.sort_order` (ADR
0002). Home splits the same status: dated `PLANNED` rows render in their day
groups in `agenda_items.sort_order`, and undated rows render in the Planned
warehouse in Board order (ADR 0013). The two views therefore disagree on
order, and the Board cannot schedule: a Planned drag can only re-rank.

The user wants the Board's Planned column to mirror Home and be fully
interactive, including drags between days and between dated and undated work.

## Decision

### Read model

`GET /api/board/planned` returns the Planned column's placement read for
living `PLANNED` tasks:

```json
{
  "today": "2026-11-20",
  "time_zone": "Asia/Jakarta",
  "rows": [
    {
      "task_id": "tsk_…",
      "item_id": "itm_…",
      "local_date": "2026-11-21",
      "agenda_sort_order": 0,
      "board_sort_order": 3,
      "undated": false
    }
  ]
}
```

| Field               | Meaning                                                          |
| ------------------- | ---------------------------------------------------------------- |
| `task_id`           | the task                                                         |
| `item_id`           | its `agenda_items` row                                           |
| `local_date`        | the row's date, whether real or the backlog sentinel             |
| `agenda_sort_order` | rank within that `local_date`                                    |
| `board_sort_order`  | `tasks.sort_order` within the `PLANNED` pile                     |
| `undated`           | rowless, or `local_date >= today + 120 months` (10-year horizon) |

- `rows` carries both ranks because the column needs both:
  `agenda_sort_order` places a card inside a dated group, while
  `board_sort_order` mirrors the task's own `tasks.sort_order` for the
  optimistic row helpers. Undated display orders by the task's
  `sort_order` (the shared `['tasks']` cache).
- `today` and `time_zone` are the server's civil day, the same pair
  `GET /api/agenda` returns.
- The server classifies `undated` with the 10-year horizon. The client never
  computes today and never compares a date to a browser clock.
- A rowless `PLANNED` task has no dated row and falls into Undated until the
  fill heals it.

### Rendering

- Dated groups ascending by `local_date`; cards inside a group by
  `agenda_sort_order`.
- One **Undated** group last, ordered by `tasks.sort_order`. It is always
  rendered, even when empty, so the warehouse has a visible drop target.
- This is exactly Home's split — dated day stream plus Planned warehouse — so
  Home and the Board agree wherever they overlap.
- The whole group section, **including its sticky day header**, is the drop
  target; the gap before the next group belongs to the section above it. The
  column body outside the groups is Undated only for a card arriving from
  another column; a `PLANNED` card can unschedule only by dropping on the
  explicit Undated group. This keeps the column's padding strips from
  silently losing a date.
- A drop on the dragged card's own group header/body is a no-op. The dashed
  placeholder only appears when the write actually uses a position, so the
  preview never promises a move the drop refuses.
- Past dated groups render like any other group, but they are not
  date-change targets: the reschedule API rejects the past. A same-group
  reorder is still allowed on a past day, because it is an agenda `/move`
  (rank only) and never changes `local_date`.
- Filtering hides empty **dated** groups; the Undated group stays. Rank math
  stays view-relative (ADR 0002 § Filters): a drop index is spliced into the
  visible order, not the unfiltered pile.
- If the Planned read is unavailable or still loading, the column falls back
  to the flat Board-ordered list and the flat status/rank drop path. It does
  not pretend to know dates.
- Other Board columns stay flat and keep `tasks.sort_order`. Only Planned
  gains date groups.

### DnD matrix

The Planned column is the only Board column where a drag can change
`local_date`.

| From                | Drop target            | Write(s)                                                                                                                                                                               |
| ------------------- | ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| dated group         | same dated group       | `POST /api/agenda/items/:id/move { sort_order }`                                                                                                                                       |
| dated group         | different dated group  | `POST /api/agenda/items/:id/reschedule { date, sort_order? }`                                                                                                                          |
| dated group         | Undated group          | `POST /api/agenda/items/:id/reschedule { unschedule: true }`                                                                                                                           |
| Undated             | dated group            | `POST /api/agenda/items/:id/reschedule { date, sort_order? }`                                                                                                                          |
| Undated             | Undated (within)       | `POST /api/tasks/:id/move { status: 'PLANNED', sort_order }`                                                                                                                           |
| another column      | dated group            | `POST /api/tasks/:id/move { status: 'PLANNED' }`, then `POST /api/agenda/items/:id/reschedule { date, sort_order? }` (or `/move { sort_order }` when the row was already on that date) |
| another column      | Undated or column body | `POST /api/tasks/:id/move { status: 'PLANNED', sort_order }`, then `POST /api/agenda/items/:id/reschedule { unschedule: true }` only when the arriving row was dated                   |
| Planned (any group) | another column         | `POST /api/tasks/:id/move { status }`; no date write (ADR 0013)                                                                                                                        |

- **The Undated group means Undated**, mirroring Home's Planned warehouse. A
  `PLANNED` card unschedules only by dropping on that group, never on the
  column's padding; a card arriving from another column may use the column
  body. A rowless arrival is already undated, so it needs only the status
  move.
- Past dates are not `local_date` targets. `reschedule { date }` 400s outside
  `[today, today + 10y)`, so a past group accepts its own in-section reorder
  but refuses a date-changing drop.
- Within Undated, the rank is the Board's `PLANNED` rank. A same-status move
  with an explicit `sort_order` is the reorder; omitted rank stays a no-op
  (ADR 0002 § Move API).
- Planned → another column is a status-only move: no date write. An existing
  dated row keeps its date and agenda rank (ADR 0013); an undated row may be
  dated today by the target status. The target column owns the new Board rank.
- A cross-column drop first changes status, then applies the date. The status
  move alone leaves an existing date untouched, which is the honest fallback
  if the second write fails.

### Two-write drops

- Cross-column drops into a dated group or into Undated need two writes:
  status first, then date. A drop into a dated group whose row is already on
  that date uses a same-date agenda `/move { sort_order }` as the second write
  instead of a no-op reschedule.
- If the second write fails, keep the honest state — `PLANNED` with its old
  date — repaint from the server, and surface an error. Never paint the
  intended date as if the reschedule had landed.
- A same-group reorder and the dated ↔ Undated drags are one write.

## Consequences

- Board drags can now change dates. The Planned column is a scheduling surface;
  every other column still changes only status and `tasks.sort_order`, and
  never reschedules an existing dated row.
- Home and the Board agree on Planned order: dated work in agenda rank,
  undated work in Board rank.
- Cross-column drops into Planned are two writes, so a partial failure is
  visible as the honest intermediate state rather than an invented date.
- A dated `PLANNED` row keeps an invisible `tasks.sort_order` that no dated
  group renders. When the row is unscheduled, that rank decides where it
  re-enters Undated; a cross-column drop into a dated group takes the Planned
  column's default rank (append, except the pause prepend from In Progress).
  This is accepted: the Board rank is warehouse order, not day order.
- A rowless `PLANNED` task falls into Undated until the fill inserts its row.
- Filtering hides empty dated groups; the Undated group stays as the warehouse
  drop target. Rank math stays view-relative.

## Residual risks

- A failed second write leaves the old date in place. The user sees the card
  in its old group or in Undated, not at the intended target, until they retry.
- The invisible Board rank on dated rows can surprise: unscheduling is what
  reveals it, and there is no UI to edit it while the row is dated.
- A past dated group renders but cannot receive a drop, which can read as an
  asymmetry next to future groups.
- A filtered reorder is view-relative; the absolute `sort_order` written to the
  server may not equal the card's unfiltered index (ADR 0002 § Filters).

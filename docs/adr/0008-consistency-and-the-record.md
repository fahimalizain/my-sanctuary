# ADR 0008: Consistency rules and The Record

Status: Accepted
Date: 2026-10-19
Implemented: PR #114, merge `9a5e756`; migrations `0019`–`0021`.

## Context

`/consistency` was a hardcoded mock: four invented rules, fake streaks, a
14-day strip, and no query behind any of it.

The precursor project, `calendar-ipynb`, contained two different things:

1. The **Enforcement scoreboard** — five rules scored over rolling windows.
   Its `RULES.md` calls it "the only daily review" and says not to resurrect the
   archived Today and Productivity notebooks as a second scoreboard.
2. **Descriptive reports** — hours by category, rankings, moving averages,
   heatmaps, year comparisons. `RULES.md` rejected percentage targets for these
   because percentage goals compete with each other.

The decision is to keep that split: the scoreboard is the only thing that
judges; the reports only describe. They live on one page, with the scoreboard
first and The Record collapsed below it.

Three data gaps had to be closed first:

- Events had no stored category. The server matched titles at read time only to
  return a colour.
- Recurring events were stored as masters with `recurrence` set; there was no
  per-instance expansion.
- There were no daily aggregates, so every report would have rescanned events.

This ADR is the consistency train that ADR 0004 and ADR 0006 deferred. It does
not supersede any earlier ADR. It records the decisions made while building
PR #114, including the corrections made during the three independent reviews
and the follow-up sleep-pairing review.

## Decision

### 1. One page: the scoreboard judges, The Record describes

`/consistency` is one page, not two routes.

- **Top: the scoreboard.** One card per active rule with a verdict, actual vs
  target, owed text, its own streak/best/compliance, and a real 14-day strip.
  Expanding a card shows the evidence behind it:
  - productive weekdays — bars for the last 10 selected weekdays with the
    target line; travel-excluded days greyed out;
  - sleep — bed and wake times on a dot timeline;
  - fitness — a 60-day dot grid;
  - learning — a rolling 7-day sum.
- **Bottom: The Record**, in a collapsed `<details>` section on the same page.
  A separate `/consistency/record` tab was rejected: the reports are secondary
  and should not get their own navigation.
- **Header:** `X / Y on track` plus a smaller `all-pass streak: N · best M`.
  When rules are paused it reads `X / Y on track · N paused`; when everything is
  paused it says `All paused`, never `0 / 0`. The mock's "Total checks" card is
  gone.
- The all-pass streak walks the rules that were **in play on each date**, not
  the current board membership, so archiving a rule does not rewrite history.
  A finished challenge's final day counts.
- Full CRUD: create, edit, pause/resume, archive/restore, reorder, plus a global
  pause with a date range and reason.

**Principle:** The Record has no targets and no red/green judgment. Its deltas
are neutral. If tracking is bad, that shows up as a data-quality warning, not
as a score.

### 2. Events carry their category

Migration `0019` adds three columns to `calendar_events`:

| Column              | Meaning                                                                                                                                             |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `category_id`       | The matched **leaf** category, or `NULL` for unclassified. No FK — a category delete must not fail event sync. It is never the `untracked` sink id. |
| `is_self_organized` | `1` when `organizer.self` is true or the organizer is absent; `0` for an invite organized by someone else. Existing rows default to `1`.            |
| `event_type`        | Google's `eventType`; `''` means unknown/legacy. Reports include `''`, `default`, and `fromGmail`.                                                  |

Classification happens at **write time**, not read time, on every path that
writes events: replica sync, webhook/queue/cron, window fetch, and app
create/patch. `classify_event_leaf` lives in `api-core` and is shared with the
colour path; the colour path uses the stored id when present and falls back to
the old matcher only while the id is `NULL`.

Precedence is deterministic: the child-beats-parent reduced match set, then the
minimum `(category.sort_order, winning pattern sort_order, category.id)`. This
replaces the old first-match-wins pipeline that only logged a warning.

When a category or pattern is created, edited, or deleted, the worker enqueues
a **reclassify** job. It runs in batches through the existing queue, stamps
changed rows, marks their local dates dirty, and reports `reclassifying` so the
page can say so. The first backfill waits for reclassify to finish; otherwise
history would be rolled up as unclassified. A taxonomy load failure must be
non-destructive: it keeps the stored `category_id` instead of writing `NULL`.

**Recurring events:** no RRULE expansion. The user's weekly
`duplicate_recurring_tasks` script materialises past instances as standalone
events and cancels the originals. The digest therefore excludes any row with a
non-empty `recurrence` (the master), plus cancelled rows. Past occurrences are
counted once they are standalone. This depends on the materialisation script;
if a recurring series was never materialised, its masters are invisible to the
digest.

The digest also drops all-day events and non-self-organized events, matching the
old `process_events_and_classify` pipeline.

### 3. Daily aggregates are precomputed

Building them later was rejected: overlap splitting depends on all of a day's
events together, and the YTD reports would otherwise rescan the replica.
Migration `0020` adds:

| Table                    | Contents                                                                                                                                                                                                          |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `daily_meta`             | One row per user/day: inferred `time_zone`, `tracked_seconds`, `sleep_seconds`, `algo_version`, `computed_at`.                                                                                                    |
| `daily_category_minutes` | Per user/day/category **seconds** after overlap sharing. `''` = unclassified real time; `*sleep` = synthetic sleep when the user has no sleep category. The table name is historical; values are integer seconds. |
| `daily_hour_seconds`     | Per user/day/hour-of-day/category seconds, for the hour × weekday heatmap.                                                                                                                                        |
| `sleep_nights`           | One row per paired or estimated night; multiple rows may share a `wake_date` (a nap and a night).                                                                                                                 |
| `dirty_local_days`       | Days whose aggregates need rebuilding.                                                                                                                                                                            |
| `report_job_status`      | `reclassify` and `backfill` cursors/status.                                                                                                                                                                       |

Only per-category seconds are stored. Productive totals, travel share, and
untracked time are derived at read time, so toggling `is_productive` does not
force a rebuild. Time is stored in **integer seconds** so overlap sharing is
exact; divide by 3600 only when rendering hours.

The pipeline ports the old notebook faithfully:

- split overlapping time equally among concurrently active events;
- split at every local midnight;
- clip to `now`;
- drop all-day, non-self-organized, cancelled, and recurring-master rows;
- fill untracked time only for finished days (`86400 - tracked`).

**Per-day timezone.** The user did not want a global timezone setting. Each
event is dated using its **own** start zone; the day's `time_zone` is the
duration-weighted vote of that day's real segments. If a day has no votes, it
inherits the nearest earlier **persisted** `daily_meta.time_zone`, then the
primary calendar's zone, then UTC. The fallback must be a pure function of
persisted data, not of which other dirty dates happened to be in the same
rebuild batch. Dating uses the event's zone, so the day zone never feeds back
into which date an event belongs to.

**Keeping it fresh.**

- Any event write marks its old and new local dates dirty (±1 day); sleep also
  dirties neighbouring days because a night spans two dates.
- A queue consumer and the existing cron rebuild dirty days, seven days per
  tick.
- A date after today is never rebuilt; it stays dirty until the date arrives.
- Today is always dirty and provisional: it is rebuilt on every write/tick, and
  untracked time is not filled in until the day ends. The day that just ended
  is finalised by the next tick after local midnight.
- A full backfill starts when there is no `daily_meta` yet, or when any row has
  a different `algo_version`. On a first run, reclassify is queued at the same
  time and the tick drains it before rebuilding days. `ALGO_VERSION` is `2`.
- Sparse dirty dates are rebuilt as separate contiguous runs, capped at 45 days
  per load, so `{2023-01-05, 2026-10-18}` cannot load all history at once.

**Known limit:** rule windows and digest loads look back at most 400 calendar
days (`MAX_LOOKBACK_DAYS`). `window_days` itself is capped at 366, inside that
lookback; if pauses push a particular window past the cap, the window is treated
as not full rather than scored short. DST days are treated as a flat 86,400
seconds, so untracked time can be an hour off on those two days a year.

### 4. Sleep is paired, then short interruptions merge

Markers are identified by title regex `^.*Asleep.*$` and `^.*Woke Up.*$`,
case-sensitive, trimmed. A night runs from the **start** of the `Asleep` event
to the **start** of the `Woke Up` event.

The two old algorithms are merged:

1. **Greedy pairing.** Sort markers by start. For each `Asleep` oldest-first,
   take the first unused `Woke Up` whose start is in `(asleep, asleep + 16h]`.
   There is **no 2-hour floor**: touching markers are a real night. A wake that
   falls outside the window is not consumed.
2. **Merge short interruptions.** Walk the resulting pairs by `Asleep` start.
   Merge a pair into the current night when the awake gap is at most 60 minutes
   (`SLEEP_INTERRUPTION_MAX_SECS = 3600`) **and** the merged span from the first
   `Asleep` to the last `Woke Up` stays within 16 hours. The stored night is the
   outer span; the brief awake gap is counted as sleep. Unmerged short pairs
   (naps) remain their own nights.
3. **Estimation fills gaps only.** When a marker is missing, the old
   `sleep_events.py` fallback estimates a night (default 6.5h, or the user's
   `sleep_duration` rule target when one exists). A fully blank day pair invents
   nothing.

Each night stores its `source`: `paired`, `est_no_wake`, `est_no_asleep`, or
`est_none`, plus the bed/wake minutes needed by the timing rules. **Rules use
`source = paired` only.** The 24-hour view in The Record uses every source, so
sleep and untracked time add up correctly.

The no-2-hour-floor choice is deliberate: the user has nights where the
`Asleep` and `Woke Up` events touch. The latest-wake-first pairing that briefly
replaced the greedy walk was rejected because it swallowed afternoon naps into
a 16-hour "night". The module header in `digest/sleep.rs` documents both failure
modes.

### 5. Rules are typed, revisioned, and scored on read

Migration `0021` adds `rules`, `rule_revisions`, and `pause_periods`.

**Kinds.** Five typed kinds, chosen over a generic rule builder. The generic
builder was rejected for the first version: most combinations make no sense,
and the form is hard to use on a phone. New kinds can be added without a schema
change because each revision stores `params_json`.

| Kind                 | What it measures                                                         |
| -------------------- | ------------------------------------------------------------------------ |
| `category_total`     | Sum of a category selector over the window; `≥` or `≤` target hours.     |
| `category_days`      | Count days where the selector reaches `min_minutes`; pass at `min_days`. |
| `category_daily_avg` | Mean selector hours over selected weekdays; optional travel exclusion.   |
| `sleep_duration`     | Median paired-night hours; needs `min_nights` paired nights.             |
| `sleep_timing`       | Bedtime/wake median or sample stdev, compared to `target_minutes`.       |

A selector is either a list of category ids (with `include_children`) or the
special `all productive`. Productive means `is_productive` on the category or
its parent (one-level tree).

**Lifecycle.** `status` is `active | paused | archived`; there is no delete.
`starts_on` and optional `ends_on` are inclusive civil dates. A rule with an
`ends_on` is a **challenge**: when the date passes it gets a real final verdict
for that day, appears in the archive, and can be restored. Finished takes
precedence over paused.

**Honest history.** Editing **scoring params** creates a new revision effective
from today; each past day is scored with the revision active on that day.
Cosmetic edits (name, icon, order, status, dates) do not create a revision.
Re-scoring all history on every edit was rejected: it rewrites the past.

**Pauses.** Both global and per-rule:

- a global pause is a `pause_periods` row with `rule_id = NULL` (e.g. travel);
- a per-rule pause is either an open-ended `paused_on` + status, or a closed
  `pause_periods` row;
- paused days are neutral in the strip, excluded from compliance, and do not
  break a streak;
- the rule window **extends backwards by the number of paused days**, so a
  60-day window still means 60 days that counted. Travel-excluded days do not
  extend anything; they are dropped from the mean.

**Window semantics.** For a verdict on date `D` with `window_days = N`, the
window is the `N` **calendar** days ending on `D` inclusive (`start =
D - (N - 1)`), then extended for pauses. This applies to every kind. A 60-day
weekday rule therefore covers about 42 weekdays, not 60 weekdays. Selected
weekdays inside the window start at 0h if nothing was tracked. A
travel-majority weekday is removed from both numerator and denominator and
shown as `excluded`; it is not replaced by an older day from before the window.

If a non-paused day in the window has no `daily_meta` row (backfill has not
reached it), the window is **not full**: `window_full` is false, `on_track` is
false, the owed text says `history still building`, and the strip's in-play
cells are `pending`. A hole is never scored as a zero. A stored zero is a zero.

**Scoring.** Nothing stores a verdict. Each rule is evaluated on read from the
aggregate tables. For each day, the rule is evaluated over the window ending
that day. `on_track` is the window ending today, so it includes today's partial
data; the headline says `on track`, not `passing today`, because today has no
verdict yet. Today gets an owed/left sentence (e.g.
`today 4.2 / 7.0h · 2.8h left`) instead of a verdict, and is excluded from the
streak. Streak is the run of consecutive passing days; best streak is the
maximum; compliance is passing days ÷ active (non-paused) days. A finished
challenge is scored through `ends_on` as a completed day.

The waking denominator for the productive/travel rules is **classified
non-sleep** time; unclassified `''` time is excluded. This deliberately differs
from `scorer.py`, which included every non-sleep event. The divergence is
documented in `rules/score.rs`.

**Seeded rules.** A new user gets eight rules, back-dated 120 days so the strips
fill immediately. Seeds have deterministic ids and `INSERT OR IGNORE`, so two
concurrent first loads cannot double-seed. A seed whose category is missing is
skipped with a warning.

| Rule                | Kind                 | Params                                                                             |
| ------------------- | -------------------- | ---------------------------------------------------------------------------------- |
| Fitness             | `category_days`      | fitness + children, 60d, ≥60 min on ≥12 days                                       |
| Productive weekdays | `category_daily_avg` | all productive, 60d, Mon–Fri, mean ≥7.0h; drop trip/travel days over 50% of waking |
| Learning            | `category_total`     | learning + children, 7d, ≥4h                                                       |
| Sleep duration      | `sleep_duration`     | 60d, ≥14 nights, median ≥6.5h                                                      |
| Bedtime             | `sleep_timing`       | bedtime median ≤630 min past noon (22:30)                                          |
| Bedtime spread      | `sleep_timing`       | bedtime sample stdev ≤45 min                                                       |
| Wake                | `sleep_timing`       | wake median ≤300 min past midnight (05:00)                                         |
| Wake spread         | `sleep_timing`       | wake sample stdev ≤60 min                                                          |

The old single "Sleep timing" rule is split into four so a failure says which
part failed and each piece can be archived independently.

### 6. The Record

`GET /api/reports/record?range=7d|30d|90d|ytd`, default `30d`, with a previous
period of the same length for deltas. It reads the daily aggregates; it never
waits for the digest.

| Report           | What it shows                                                                                                                 |
| ---------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Days             | Daily stacked bars by root category in the real category colours; a full column is 24h. Empty days are zero-filled.           |
| Ranking          | Categories sorted by hours, with share and delta vs the previous period. Neutral delta, no red/green.                         |
| Productive trend | Daily productive hours with 30-day and 60-day moving averages. The scale includes the daily values so a spike is not clipped. |
| Hour × weekday   | Mean productive time per clock hour, from `daily_hour_seconds`; today is excluded.                                            |
| Data quality     | Mean untracked hours/day, estimated (non-paired) nights, and the top unclassified titles. This is a warning, not a score.     |
| Years            | The whole history, not the range: productive h/day, weekday vs weekend, sleep median.                                         |

The reports are hand-rolled SVG; no chart library was added. While reclassify
or backfill is running, an open Record polls and shows `Building history…`.

### 7. What was deliberately left behind

| Left behind                          | Why                                                                     |
| ------------------------------------ | ----------------------------------------------------------------------- |
| Today pie                            | Archived in the old project; Home already covers today.                 |
| Category × date heatmap              | Repeats the stacked daily bars.                                         |
| TeachxAI tracker, 10km challenge     | One-offs. A generic category drill-down remains a later idea.           |
| Debug, renamer, bokeh notebooks      | Not reports.                                                            |
| Generic rule builder                 | Typed kinds first; extend with new kinds and JSON params.               |
| Re-score everything on edit          | Rewrites history; revisions keep it honest.                             |
| One global timezone                  | Per-day inference from that day's events; travel is a first-class case. |
| 2-hour sleep pairing floor           | Touching `Asleep`/`Woke Up` markers are real nights.                    |
| Latest-wake-first sleep pairing      | Swallows naps into a 16-hour night.                                     |
| RRULE expansion                      | The weekly script materialises past instances.                          |
| Storing rule verdicts                | Revisions and pauses change how past days score; compute on read.       |
| Chart library                        | Four chart shapes do not justify the PWA bundle cost.                   |
| Separate `/consistency/record` route | Collapsed section below the scoreboard is enough.                       |

## Consequences

- The scoreboard is the only surface that judges; The Record cannot quietly
  become a second scoreboard.
- History is honest: changing a target does not turn last month green, and
  archiving a rule does not rewrite the all-pass streak.
- Reads are cheap because the aggregates are precomputed, but the system now
  has a maintenance path: dirty days, reclassify, backfill, and an
  `algo_version` rebuild.
- The first visit to `/consistency` queues reclassify and then a full history
  backfill. For the real history this takes roughly 1–2 hours; the page shows
  `Building history…` and scores nothing as failed while the window is short.
- The 400-day lookback cap bounds CPU and keeps the queue honest; a window that
  runs past it is shown as not full, never scored short.
- Per-day timezone inference is more correct for travel than a global setting,
  but it is an invention, not a port of the old notebooks. Dates are always
  decided by the event's own zone.

## Known gaps

- The UI has not been used in a logged-in browser against the real history.
- A per-rule pause with an end date exists on the API
  (`POST /api/rules/:id/pause { from, to }`) but the card only offers an
  open-ended pause. Global pauses have date ranges in the UI. Ranged pauses on
  a finished or archived rule return 409.
- Untracked time is an hour off on the two daylight-saving days (flat 86,400s).
- Streaks and windows look back at most 400 days.
- Legacy rows keep `is_self_organized = 1` and `event_type = ''` until Google
  sends that event again; unknown event types are included.
- Recurring sleep markers depend on the weekly materialisation script. A
  recurring series that was never materialised is excluded as a master.
- The seed silently skips Fitness/Learning if those categories are missing; the
  skip is logged as a warning.
- Migrations `0019`–`0021` still have to be applied to production D1, followed
  by a deploy. The merge only changed code.

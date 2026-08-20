# ADR 0003: Title holes (fill, extract, classify lock)

Status: Accepted
Date: 2026-08-20

> This ADR **documents what shipped**, not a plan: it is the lock of the title
> hole work landed on branch `regex-magic` (PR #32).
>
> Built by:
>
> - `2c60579 feat(api): add regex hole fill and extract`
> - `da60b12 feat(api): return classify affixes and honor category lock`
> - `cf39c36 feat(api): add display_title to task views`
> - plus the TaskModal / CategoryPicker slices.
>
> There is no implementation slice left to schedule — every § below is a
> frozen contract the current code already satisfies.

## Context

Tasks have **no** `category_id` (and no `list_id`). A task's category is
**computed from its title** at every read and write by running the title
against the user's category patterns — the `classify` reduction in
`packages/api-core/src/categories.rs`, wrapped for the worker by
`classify_title` in `apps/worker/src/tasks.rs`. The stored row never names a
category; the title is the single authority. This is the rule the whole
feature is built around, and this ADR deliberately refuses to weaken it (see
§ Storage and the first rejected alternative).

Categories carry **ordered regex patterns** (`task_categories` →
`task_category_patterns`, ordered by `sort_order ASC`). The first-visit seed
(`ensure_taxonomy` in `packages/api-core/src/categories.rs`) gives every root
two patterns, `^{Name}$` and `^.* [|] {Name}$` (emitted through
`regex::escape`, so the literal name is escaped), which is exactly the pair
this ADR keeps reasoning about. Users may author arbitrary patterns beyond the
seed, with the single guard that each pattern compiles, is non-empty, and is
at most 256 chars (`validate_pattern` / `MAX_PATTERN_LEN`).

- **Create/update reject an unfiled title.** `resolve_category` in
  `packages/api-core/src/tasks.rs` requires the title to uniquely match a
  single non-untracked category: 0 matches → `title does not match a
category`, several → `title matches multiple categories`, a match on the
  `untracked` sink → `title matches untracked`. All 400s. The write side
  never invents a suffix, never "helps" the user — it only accepts what
  already files.
- **Calendar start uses the stored title exactly.** `start_task`
  (`packages/api-core/src/tasks.rs`) opens the Google event with
  `summary: task.title.clone()` — nothing more, nothing less. The old comment
  "no `| Category` suffix" meant "don't invent a suffix at write time". It did
  **not** mean "a stored title may never contain a suffix": after this ADR the
  stored title _may already contain_ `| Category` because _fill_ wrote it at
  create time. `start_task` is unchanged either way.
- **The pain**: to file `Review` under the database-style category name
  `SpicyHome`, the user had to type the full `Review | SpicyHome` by hand —
  every time — and the card then showed that suffix on screen _right next to_
  a category pill already saying SpicyHome. Worse, the database-style spelling
  (`SpicyHome`) and the human-composed spelling (`Spicy Home`, used in the
  seeded chrome) are different strings that both match the same pattern. The
  UI could neither relabel the title nor guess which spelling to store.
- **Matching vs generation are different problems.** A regex is not a string
  with `.*` standing in for a free slot; it is a tree (an HIR built by
  `regex-syntax`). In that tree `.*` is a `Repetition { sub: Dot }` node, not
  a template token. Replacing the source text `str.replace(".*", X)` is wrong
  on three counts: `[.*]` is a character class of the two characters `.` and
  `*` (not a hole); `\.*` is zero-or-more literal dots (not a hole); and a
  pattern with several `.*` has no way to say which one X fills. This ADR
  therefore defines generation **on the HIR**, not the source.

## Decision

Lock all of the following. Each subsection is a frozen contract; the shipped
code (`PR #32`) is the reference implementation.

### Storage (unchanged schema)

- `tasks.title` stays the **full string** and remains the classify authority.
  `TaskView.title` (and the web `TaskRecord.title`) is the stored full string;
  a PATCH round-trips it verbatim.
- **No `tasks.category_id` column. Ever, per this ADR.** Category stays a pure
  function of the title, so reclassifying is free (rename patterns, retitle
  tasks) and there is exactly one source of truth.
- Calendar event summary = `tasks.title` unchanged (see Context). A title that
  was filled at create may carry a ` | Category` suffix into the event; that
  is correct and intended.

### Hole definition (`pattern_gen.rs`)

`packages/api-core/src/pattern_gen.rs` owns the hole vocabulary. A **hole**
is an unbounded repetition of a dot:

- `Repetition { max: None }` (i.e. `min` with no upper bound) whose sub equals
  `Hir::dot(Dot::AnyCharExceptLF)` (the default `.`) **or**
  `Hir::dot(Dot::AnyChar)` (`(?s:.)`). Concretely: `.*` , `.+` , `(?s:.*)`.
- These are **NOT** holes (all must keep working and never swallow the fill):
  - `[.*]` — a character class containing `.` and `*` (a class is a class, not
    a repetition),
  - `\.*` — zero-or-more **literal dots** (the sub is a Literal, not a Dot),
  - `. *` — a dot immediately followed by a separate space-repeat (`\s*`);
    the dot itself is not repeated,
  - `a*` — a repetition of a literal `a`,
  - `{0,}` as a synonym of `*` on a non-dot sub — a bounded-from-zero repeat of
    anything that is not a dot is still not a hole.
- `Concat` and `Capture` are **flattened** so the first hole is a _sibling_:
  `flatten` unwraps `Concat` and `Capture` non-recursively into a flat node
  list, letting `split_hole`/`emit_affixes` index the first hole among peers
  instead of diving into nested trees.
- **First hole only.** Later holes are not filled: a later `.*` emits its
  minimum `""`, a later `.+` emits `"x"` (an arbitrary single unit). The
  result must always be a member of `L(pattern)`.
- Default `.` rejects `\n` in the fill: `x.contains('\n')` with a
  non-`(?s:.)` hole is `FillError::NewlineInHole` → surfaced as a 400 "X
  contains \n but the hole is default `.`". A `(?s:.)` hole accepts newlines.
- **Identity**: if `X` already matches `pattern`, `fill_regex` returns `X`
  **unchanged** — no re-emission, no canonicalization. This is what keeps the
  database spelling `SpicyHome` alive: filling `Hello! | SpicyHome` is a no-op.

### Two pattern picks (do not collapse)

Fill and extract/display look at **different** patterns of the same category,
because nothing guarantees one pattern is both _matching_ and _hole-bearing_.

| Verb                                       | Which pattern                                                                                               |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| Extract / `display_title` / actual affixes | first in `sort_order` that **matches** the stored/typed title (`first_matching_pattern` in `categories.rs`) |
| Fill / empty-lock chrome                   | first in `sort_order` that **has a hole** (`emit_affixes` is `Some`)                                        |

`first_matching_pattern` walks patterns in stored `sort_order`, skipping
invalid stored regexes (same compile/skip rule as matching); it is the _match_
pick. The fill pick is a separate walk that asks only "does this pattern have
a hole" via `emit_affixes(...) == Ok(Some(_))`.

The two picks must stay distinct. `Review` matches neither `^Work$` nor
`^.* [|] Work$`, so a fill that used "first pattern that matches" would have
_nothing_ to fill ("first that matches" is undefined on an empty matches set —
see the rejected alternative). The only sane fill target for a short title is
"first pattern that has a slot", which is exactly the hole-bearing walk.

### Operations

Each primitive is a pure function in `pattern_gen.rs`:

- **`fill_regex(R, X) → S ∈ L(R)`.** Walks the parsed HIR and emits: `Look` →
  `""`; `Literal` → its text (non-UTF-8 literal → `BadLiteral`); `Class` →
  the lowest code point of its **first** range (empty class → `EmptyClass`);
  `Concat` → the concat of its children; `Alternation` → the **first** branch
  (`subs[0]` — never a union, never a "best" pick); the **first hole** → `X`
  (or `"x"` when `X` is empty and the hole is a `+`); any **later hole** → its
  minimum (`""` for `*`, `"x"` for `+`); any other `Repetition` → `min` copies
  of its sub. The result is guaranteed (and `debug_assert`-ed) to match.
- **`split_hole(R, S) → { prefix, hole, suffix }`.** Requires `S ∈ L(R)`
  (`NoMatch` otherwise) and a flattened first hole (`NoHole` otherwise).
  Anchors the flattened prefix from the start (`\A(?:…)`) and the flattened
  suffix to the end (`\A(?:…)\z`), finds the legal cut, and returns the
  **actual slices of `S`** — the text the hole really consumed, with the
  prefix/suffix as the user spelled them. **Greedy** holes take the rightmost
  legal cut, ungreedy the leftmost. **No capture groups are ever consulted.**
- **`extract_hole(R, S)` = `split_hole(R, S).hole`** — exactly the consumed
  span's text.
- **`emit_affixes(R) → Option<(prefix, suffix)>`.** Around the first hole of
  the _hole-bearing_ walk: canonical emit of all nodes before it (prefix) and
  after it (suffix), with every hole suppressed to its minimum (so affixes are
  pure chrome). `None` when the pattern has no hole. This is the "empty-title +
  lock" chrome: for `^.* [|] Work$` it returns
  `Some(("", " | Work"))`.

**Round-trips** (the crucial, easy-to-get-wrong pair):

- `extract(fill(R, X)) == X` — filling then extracting recovers the filled
  value exactly. `extract_fill_round_trips` locks it.
- `fill(extract(R, S))` is **not** identity on `S`. Because `Alternation`
  emits the **first** branch, a filled `Review | SpicyHome` comes apart as
  `hole = "Review"`, `suffix = " | SpicyHome"`, but re-fill re-emits the
  pattern's first alternative → `Review | Spicy Home` (the canonical chrome).
  The database spelling is not reconstructible from the hole alone. The edit
  modal therefore classifies the **stored full title** on open (see § TaskModal
  UX), never the hole, so we never canonicalize an untouched title.

### `display_title` on every `TaskView`

Every `TaskView` (and every web `TaskRecord`) carries a computed
`display_title` that is **never null**:

- For a `Matched`, non-untracked category it is the **hole** split off the
  stored title under the category's **first matching pattern** — "Review Q3 |
  Work" → "Review Q3" (`to_view` in `packages/api-core/src/tasks.rs`, via
  `split_affixes`).
- When there is no matching hole pattern — a patternless match (e.g. `Work`
  against `^Work$`), untracked, or a conflict — `display_title` keeps `title`
  verbatim.
- Cards/lists render `display_title`: `TaskCard` in the board shows it as the
  card body, and the Lists page shows it for the chip and its start/stop/
  pause/complete/discard `aria-label`s. The tooltip (`title=` attribute on
  `TaskCard`) keeps the **full** `task.title`, so the complete string is one
  hover away.
- The mock timeline `TaskRow` (`apps/web/app/components/TaskRow.tsx`) renders
  the **mock** `Task` type, not `TaskRecord` — it is out of scope (see § Out of
  scope); it never reads `display_title`.

### Classify API

`GET /api/tasks/classify?title={raw}&category_id={optional}` (handler:
`classify_title` in `apps/worker/src/tasks.rs`; logic:
`api_core::classify_title` in `packages/api-core/src/tasks.rs`). A **read**
that runs the exact matcher the create/update endpoints enforce, so the modal
can preview a filing without writing. It seeds the taxonomy (count-gated) like
`list_tasks`, so a first-visit caller still has a matcher.

Request contract:

- `title` is **always present** in the query string (the modal always sends
  it, even as `""`). It is trimmed server-side.
- Empty title + **no** lock → `400 title must not be empty` (the create rule,
  surfaced as a preview instead of a write that would fail later).
- Empty title + **lock** → allowed: preview the category's canonical chrome,
  `display_title = ""`, `persist_title = prefix + suffix`. The FE keeps Save
  disabled until the hole is non-empty.
- A `category_id` that is missing / another user's / the `untracked` sink →
  `400 category not found`. Deliberately **not** `TasksError::NotFound` — the
  worker maps that variant to `404 "task not found"`, which would be a lying
  error message for a bad category reference. `Invalid("category not found")`
  is the correct vocabulary (the categories endpoints already use "category
  not found" for the same condition).

Response — an **externally tagged** serde enum; **both** variants always carry
all four strings (empty string = no chrome), so the modal can render the same
one-field titled-input shape in every state:

```
Matched   { category, prefix, suffix, persist_title, display_title }
Untracked { conflict, categories, prefix, suffix, persist_title, display_title }
```

`Matched`/`Untracked` are the _enum variant keys_ on the wire (e.g.
`{"Matched": {...}}`). `persist_title` is what a save would store — a string
guaranteed to file to the category. `display_title` is what the input field
should show. `Untracked.categories` names every reduced match when
`conflict = true`, so the modal can say "Matches A and B — be more specific".

#### Unlocked (no `category_id`)

Today's classify plus affixes: `classify_unlocked` returns `Matched` with the
affixes from `split_hole` on the **first matching pattern** of the matched
category; `persist_title` is the title as typed (identity — no lock, nothing
to invent). When no pattern matches or several do, `Untracked` (no chrome).

- `Test | Work` → persist `Test | Work` (as typed), display `Test`,
  suffix ` | Work` (actually split, never canonical re-emit).
- `Hello! | SpicyHome` → the **actual** split suffix ` | SpicyHome` sticks —
  never the first-alternative ` | Spicy Home`. Because split walks the
  _matching_ pattern with real slices, the database spelling survives the
  unlocked preview.

#### Locked (with `category_id` = C)

A selected category is a **lock, not a hint**: the classify is forced to file
into C, and the chrome shown is C's. The user must clear the lock (the picker
X or the "No category" row) to get title-only classify back. Rules, in order
(`classify_locked`):

1. **Identity**: a non-blank title that **already uniquely files to C** keeps
   its exact spelling — `persist_title = title` — with affixes as _actually_
   split under C's first matching pattern ("… | SpicyHome" stays compact,
   never re-emitted as "… | Spicy Home").
2. Otherwise fill C's **first hole-bearing** pattern (the fill pick of § Two
   pattern picks). `Work` + lock SpicyHome → fill into `^.* [|] SpicyHome$` →
   `Work | SpicyHome` (or the first alternative of that pattern). It does
   **not** snap to the Work root — the lock is C, period.
3. A no-hole-only category (nothing to fill) + a title that does not file to C
   → `Untracked { conflict: false }` with no chrome.
4. Fill errors (e.g. a newline in the hole) → `400` carrying the fill engine's
   message — the same 400 channel as every other invalid title.
5. After filling, confirm the filled title uniquely files to C; a sibling or
   other category that also matches it reports `Untracked { conflict: true }`
   naming the actual categories.

Blank + lock: the no-hole branch is skipped (there is nothing to file), and
the fill branch previews `(prefix, suffix)` chrome with `persist = prefix +
suffix`, `display = ""` (Save disabled until filled).

### TaskModal UX (as shipped)

The shipped `apps/web/app/components/TaskModal.tsx` + `CategoryPicker.tsx`:

- **Task Name first**, the Category picker **below** the title + its classify
  hints, then Description — the title reads before the filing chrome. (Old
  order had the category above; reverted in `8fcedc1`.)
- The visible input is the **hole**: on create it starts empty; on edit it
  opens with the computed `display_title`, never the stored full title.
  Prefix/suffix from the latest classify are rendered as frozen
  `select-none` spans wrapping the single flex input — one field that reads
  `[prefix][hole][suffix]` (chrome empty for no-chrome states).
- Two pieces of picker state, kept distinct:
  - **`lockId`** — an explicit user pick only; this is the _only_ value ever
    sent as `category_id`. Unlocked autofill must not lock, or a title-only
    match would freeze the chrome.
  - **`pickerSelectedId = lockId ?? (matched ? matched.category.id : null)`**
    — an unlocked unique classify match **autofills the combobox** without
    locking, so the user sees where the title will land even when they never
    touched the picker.
- **Clear**: an X on the trigger only when `lockId !== null`, plus the always-
  visible "No category" row in the panel; both set `lockId = null`. A
  classify-only suggestion shows **no X** (changing the title away from its
  match clears it naturally). `Untracked` is never a picker row.
- The `CategoryPicker` panel is **not portaled** to `document.body`: a portal
  escapes DialogContent, the Radix Dialog counts it as an outside click, and
  the clear X was being eaten by the dialog's close handler. The panel renders
  `absolute` under the trigger inside the modal's `relative` wrapper, scrolls
  with the content container, and its own `mousedown` listener closes on true
  outside clicks.
- **Classify fires on title blur and on lock change — not on every keystroke.**
  The live input is tracked so a response that lands after the user typed past
  its snapshot is dropped; an empty title fires **only when locked** (empty +
  no lock stays idle, matching the server's 400 rule).
- **Edit-open**: the first classify sends the stored **full `task.title`**
  (marked as the seed, exempt from stale-drop) while the input shows
  `display_title`. This is what prevents first-alternative canonicalization:
  classifying the hole would fill it and re-emit `SpicyHome` as `Spicy Home`.
- **Save sends `persist_title` from classify.** If the last classify was not
  issued for the current input+lock, **or** the matched `category.id !==
lockId`, Save awaits one fresh classify and POSTs _its_ `persist_title`.
  The raw hole is never POSTed when chrome exists.
- **Stale-lock guard**: selecting a category fires its classify only inside an
  effect, after paint — until it settles, the cached result is stale for the
  new lock. Save's `corresponds` check (`lastClassifyRef.lock === lockId &&
last.input === title.trim()`) plus the `category.id === lockId` clause means
  a changed lock can never persist the _previous_ lock's title (e.g. `Work`
  instead of `Work | SpicyHome`).

## Out of scope

- Adding `category_id` (or `list_id`) to `tasks` — explicitly reversed all the
  way down in § Storage.
- Changing the Google Calendar summary (`start_task` still posts
  `summary = task.title` exactly).
- Enumerating all of `L(R)` — an `exrex`-/McIlroy-style language sampler is a
  different tool; here we only ever need _one_ member (fill) plus _the actual_
  consumed span (extract).
- Any client-side HIR walk — the FE only concatenates strings the classify
  endpoint already returned (prefix + hole + suffix); it never parses or
  rewrites patterns.
- Rewriting users' patterns with capture groups to name the slot.
- `TaskRow` (the mock timeline `Task` type) gaining `display_title`.
- An NFA/DFA (regex-automata) implementation of fill/extract — the regex-crate
  engine already sits under the HIR; generating on the HIR is sufficient.

## Rejected alternatives

- **Store the hole + `category_id`** (undo the category-is-computed-from-the-
  label rule): it reverses the locked invariant "category is computed from
  title", it would make calendar events lose the suffix (or require the event
  write to recompose it, reinventing fill on the write path), and it creates
  two sources of truth (stored FK vs. matcher) that can drift the moment a
  pattern is edited. The whole point of the existing model is that renaming a
  category pattern re-files every task for free; a stored FK throws that away.
- **`str.replace(".*", X)`**: `.*` is not a unique token (see Context — `[.*]`,
  `\.*`, multiple holes), `X` can contain regex metacharacters and re-enter
  pattern space (fixed only by escaping, which the source-string approach
  never does), and after replacing one `.*` the _rest of the pattern is still a
  regex_ whose residual `.*`/anchors may not match the injected text.
- **`title_trimmed` as the display field name**: reads as whitespace-trimmed,
  not "title with the category suffix split off". We use `display_title`.
- **Fill using the first matching pattern**: `Review` matches neither `^Work$`
  nor `^.* [|] Work$`, so "first that matches" is an empty set — a fill with no
  target. Fill must pick "first that has a hole", independent of matching. See
  § Two pattern picks.
- **Always canonicalize on save** (re-emit first alternatives): a silent,
  untouched edit of `Review | SpicyHome` would rewrite it to `Review | Spicy
Home` on the server, destroying the user's spelling with no visible cause.
  The identity rule (fill returns `X` when it already matches) plus
  edit-open-classifies-full-title both exist to make this impossible.
- **Classify lock as a hint that snaps** to a different unique match (lock
  SpicyHome + type `Work` → snaps to Work): a lock must be a lock. The user
  picked SpicyHome explicitly; auto-unlocking because the title matches
  something else was tried and **reverted after grilling** — the shipped
  behavior is identity-if-files-to-C else fill-under-C, never a silent
  re-target.
- **Capture groups for extracting the hole:** groups only _name_ a span of the
  matched text; they add no information here, because we already know _which
  HIR node_ is the hole and can slice exactly its consumed span via anchored
  prefix/suffix regexes. Groups would additionally burden pattern authors.
- **Portaling the `CategoryPicker` panel to `document.body`:** React portals
  render outside `DialogContent`, so the Radix Dialog's outside-click handler
  treats a click on the panel as a click outside and closes/clears behind the
  user's back (it "ate the clear"). Absolute positioning inside the dialog is
  the fix (see § TaskModal UX).

## Consequences

- Every task API consumer must read `display_title` (never null) as the
  canonical visible title; `title` remains the full stored string. Old clients
  that ignore the new field keep working unchanged — they simply show the full
  title (suffix visible) as before.
- The classify JSON grew four fields (`prefix`, `suffix`, `persist_title`,
  `display_title`) on **both** variants — additive for readers that only used
  `category` / `conflict` / `categories`.
- `regex-syntax` is now a **direct** `api-core` dependency
  (`packages/api-core/Cargo.toml`); it was already transitive via `regex`.
- Users/systems authoring category patterns that are **not generatable** (no
  hole, empty class) can still _classify_ into them, but can no longer
  pick-then-type a short title for them in the modal: with nothing to fill,
  the lock falls back to identity or `Untracked`. The pickers label this by
  simply not offering chrome.
- Edit-open now classifies the full stored title rather than the visible hole —
  an invisible-but-real behavior change that keeps user spelling intact.
- The `TaskModal` gains two coupled pieces of state (`lockId` +
  `pickerSelectedId`) and a frostier classify contract (blur + lock-change
  firing, seed exemption, stale-lock guard on Save); future modal consumers
  must implement the same discipline or reuse the hook.

## Edge-case catalogue

All cases below assume the seeded pair `^Work$`, `^.* [|] Work$` for the Work
root and `^.* [|] (SpicyHome|Spicy Home)$`-style patterns for SpicyHome, with
"lock C" meaning `category_id=C` was passed:

| Situation                                                                                    | persist / display / picker                                                                                                                                                                     |
| -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unlocked `Work`                                                                              | persist=Work, display=Work, no chrome (`^Work$` has no hole); picker autofills Work after blur (but never locks)                                                                               |
| Unlocked `Test \| Work`                                                                      | persist as typed `Test \| Work`, display=Test, suffix=` \| Work` (actually split); picker autofills Work                                                                                       |
| Unlocked `Hello! \| SpicyHome`                                                               | persist as typed, display=Hello!, the **actual** split suffix ` \| SpicyHome` — never the first-alt ` \| Spicy Home`                                                                           |
| Unlocked `asdf`                                                                              | Untracked (no conflict), no chrome; Save fails with "Title does not match a category"                                                                                                          |
| Unlocked two-root conflict (both Work and SpicyHome match)                                   | Untracked{conflict}, `categories` names both; Save fails with "Matches A and B — be more specific"                                                                                             |
| Lock SpicyHome + `Work`                                                                      | fill SpicyHome's first hole-bearing pattern → `Work \| SpicyHome` (or its first alternative `Work \| Spicy Home`); does **not** snap to the Work root                                          |
| Lock SpicyHome + `""`                                                                        | `(prefix, suffix)` chrome preview (`" \| Spicy Home"` canonical), persist=`" \| Spicy Home"`, display=`""`; Save disabled until the hole is non-empty                                          |
| Lock SpicyHome + `Hello! \| SpicyHome`                                                       | identity (already files to SpicyHome): persist keeps `Hello! \| SpicyHome`, affixes as spelled → suffix ` \| SpicyHome` (compact spelling survives)                                            |
| Lock Work + `Review`                                                                         | fill first hole-bearing → `Review \| Work`; display=Review, picker shows the locked Work                                                                                                       |
| Lock Work + `Work`                                                                           | identity on `^Work$`: persist=Work, no chrome (no hole)                                                                                                                                        |
| Lock no-hole-only category + `other text`                                                    | no hole to fill and does not file → Untracked{conflict:false}, no chrome                                                                                                                       |
| Untracked task edit                                                                          | no lock (untracked has no patterns / is not a lock target); must retitle to a living category to save; move/delete still work                                                                  |
| Newline in the hole (`\n` in a default-`.` pattern)                                          | `400` with the fill engine's message ("X contains \n but the hole is default `.`")                                                                                                             |
| `^.*Work.*$` extracting `FooWorkBar`                                                         | hole=Foo — the first hole is the leading `.*`; the second `.*` _stays in the suffix_ (`WorkBar` is not touched)                                                                                |
| Paste the full matching title into the hole (`Work \| SpicyHome` typed as the whole "title") | identity when the composed/raw string is already in `L(R)` for the target — persist keeps it, no double chrome                                                                                 |
| Change lock, then Save before the classify for the new lock returns                          | the stale-lock guard fires: Save awaits a fresh classify (or refuses the stale `persist_title`) — it must not persist the _previous_ lock's title (e.g. `Work` instead of `Work \| SpicyHome`) |

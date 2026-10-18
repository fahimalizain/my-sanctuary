import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { createPortal } from 'react-dom';
import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { Category, TaskList } from '@/app/types';
import {
  buildCategoryTree,
  categoryTriggerLabel,
  isImpliedByParent,
  selectedCategoryRows,
  type SelectedCategoryRow,
} from './board-filters';

// A picker row in visual order (list → root → children). `checked` includes
// rows implied by a selected parent; `implied` rows render disabled and every
// toggle on them is a no-op (ADR 0002 § Filters).
type Row = {
  id: string;
  title: string;
  color: string;
  checked: boolean;
  implied: boolean;
};

// One list group of the panel: heading + its rows in visual order, each with
// its global flat index (used by the keyboard highlight).
type Section = {
  listId: string;
  listName: string;
  entries: { row: Row; indented: boolean; index: number }[];
};

// One pinned row of the "Selected" block above the tree: an explicitly
// selected category as resolved by `selectedCategoryRows` — always checked
// by definition. `implied` extras render disabled and every toggle on them
// is a no-op (same rule as the tree rows).
type PinnedRow = SelectedCategoryRow & {
  checked: boolean;
  implied: boolean;
};

/** The board's category filter (ADR 0002 § UI): a searchable checkbox
 *  combobox grouped by list → root → children. Only explicit ids are stored
 *  in the URL; a parent selection implies its children (checked + disabled)
 *  without ever writing them. */
export function CategoryFilter({
  lists,
  categories,
  selectedIds,
  onToggle,
  onClear,
  portalContainer,
}: {
  lists: TaskList[];
  categories: Category[];
  selectedIds: string[];
  onToggle: (id: string) => void;
  onClear: () => void;
  /** Where the panel is portaled. Omitted = the desktop row's document.body
   *  panel at viewport coordinates (unchanged). `null` = the caller's host
   *  element is not mounted yet — render NOTHING rather than fall back to
   *  the body. An element = the mobile sheet's host: portal into it, so the
   *  panel is a descendant of the dialog content (see the note above the
   *  portal below). */
  portalContainer?: HTMLElement | null;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  // Index into `rows` of the keyboard-highlighted row (-1 = none).
  const [highlight, setHighlight] = useState(-1);
  // Fixed position of the portaled panel, from the trigger rect.
  const [pos, setPos] = useState<{
    top: number;
    left: number;
    width: number;
  } | null>(null);

  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Where the panel is portaled, and therefore how its coordinates are read:
  // - `undefined` (the desktop filter row): `document.body` + `position:
  //   fixed` at viewport coordinates — today's behavior, unchanged.
  // - `null` (the mobile sheet): the host element has not mounted yet. Hide
  //   the panel; NEVER fall back to document.body. Inside a Radix Dialog the
  //   body is `pointer-events: none`, the dialog's focus scope cannot reach a
  //   node outside its content, and a body-level panel counts as an outside
  //   click that would dismiss the sheet.
  // - an element (the sheet's host): portal INTO it with `position:
  //   absolute`, measured against its border box, so the panel is a real
  //   descendant of the dialog content.
  const panelHost =
    portalContainer === undefined ? document.body : portalContainer;
  // Host-relative (and clamped) coordinates whenever a host was handed in —
  // including the not-ready `null`, which renders nothing at all.
  const inHost = portalContainer !== undefined;

  const { sections, rows } = useMemo(() => {
    const groups = buildCategoryTree(lists, categories, query);
    const toRow = (cat: Category): Row => {
      const implied = isImpliedByParent(cat.id, selectedIds, categories);
      return {
        id: cat.id,
        title: cat.title,
        color: cat.color,
        checked: selectedIds.includes(cat.id) || implied,
        implied,
      };
    };
    let index = 0;
    const sections: Section[] = groups.map((group) => ({
      listId: group.list.id,
      listName: group.list.name,
      entries: group.roots.flatMap((root) => [
        { row: toRow(root.category), indented: false, index: index++ },
        ...root.children.map((child) => ({
          row: toRow(child),
          indented: true,
          index: index++,
        })),
      ]),
    }));
    const rows = sections.flatMap((section) =>
      section.entries.map((entry) => entry.row),
    );
    return { sections, rows };
  }, [lists, categories, query, selectedIds]);

  // Explicit selections pinned above the tree (ADR 0002 § UI), in picker-tree
  // order. Deliberately not keyed on the query: the pinned block ignores the
  // search box and `selectedCategoryRows` already walks the empty-query tree.
  const pinned = useMemo<PinnedRow[]>(
    () =>
      selectedCategoryRows(selectedIds, lists, categories).map((entry) => {
        const implied = isImpliedByParent(entry.id, selectedIds, categories);
        return { ...entry, checked: true, implied };
      }),
    [selectedIds, lists, categories],
  );

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    // Next open shows the full tree again.
    setQuery('');
    if (restoreFocus) triggerRef.current?.focus();
  }, []);

  const firstEnabledIndex = useCallback(
    () => rows.findIndex((row) => !row.implied),
    [rows],
  );

  /** Move the highlight to the next/previous ENABLED row, clamping at the
   *  ends of the list. */
  const moveHighlight = useCallback(
    (dir: 1 | -1) => {
      setHighlight((prev) => {
        const enabled = rows
          .map((row, index) => (row.implied ? -1 : index))
          .filter((index) => index >= 0);
        if (enabled.length === 0) return -1;
        const current = prev >= 0 ? enabled.indexOf(prev) : -1;
        const next =
          dir === 1
            ? current === -1
              ? 0
              : Math.min(current + 1, enabled.length - 1)
            : current === -1
              ? enabled.length - 1
              : Math.max(current - 1, 0);
        return enabled[next];
      });
    },
    [rows],
  );

  // Position the panel from the trigger rect. Recompute on open, scroll
  // (capture: the board scroller — and the sheet's body — scroll too),
  // resize, query changes (the list height changes with the results),
  // selection changes (toggling adds/removes the pinned "Selected" block) and
  // a change of portal host (the sheet's host mounts after the first commit).
  //
  // Two coordinate systems:
  // - desktop row (no host): viewport coordinates for a `fixed` panel,
  //   min-width 18rem, flip above when there is not enough room below.
  // - sheet host: the panel is `absolute` inside the host, so measure against
  //   the host's border box and clamp to it — the 18rem min-width is capped
  //   at the host's width (it cannot overflow a narrow sheet) and the flip is
  //   decided by the space left INSIDE the host, not in the viewport.
  useLayoutEffect(() => {
    if (!open || panelHost === null) return;
    const position = () => {
      const trigger = triggerRef.current;
      const panel = panelRef.current;
      if (!trigger || !panel || !panelHost) return;
      const rect = trigger.getBoundingClientRect();
      const gap = 8;
      const height = panel.offsetHeight;
      if (!inHost) {
        const width = Math.max(rect.width, 18 * 16); // min-width 18rem
        const spaceBelow = window.innerHeight - rect.bottom;
        const top =
          spaceBelow >= height + gap
            ? rect.bottom + gap
            : Math.max(gap, rect.top - height - gap);
        setPos({ top, left: rect.left, width });
        return;
      }
      const box = panelHost.getBoundingClientRect();
      const width = Math.min(Math.max(rect.width, 18 * 16), box.width);
      const spaceBelow = box.bottom - rect.bottom;
      const top =
        spaceBelow >= height + gap
          ? rect.bottom - box.top + gap
          : Math.max(0, rect.top - box.top - height - gap);
      const left = Math.min(
        Math.max(0, rect.left - box.left),
        box.width - width,
      );
      setPos({ top, left, width });
    };
    position();
    window.addEventListener('scroll', position, true);
    window.addEventListener('resize', position);
    return () => {
      window.removeEventListener('scroll', position, true);
      window.removeEventListener('resize', position);
    };
  }, [open, query, selectedIds, panelHost, inHost]);

  // Outside click (mousedown, ignoring trigger + panel) closes without
  // stealing focus from the control the user actually clicked.
  useEffect(() => {
    if (!open) return;
    const onMouseDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (triggerRef.current?.contains(target)) return;
      if (panelRef.current?.contains(target)) return;
      close(false);
    };
    document.addEventListener('mousedown', onMouseDown);
    return () => document.removeEventListener('mousedown', onMouseDown);
  }, [open, close]);

  // Keyboard while open: Escape closes; ArrowUp/Down move the highlight among
  // enabled rows; Enter toggles the highlighted row (search field included,
  // nothing is submitted), Space only outside the search input (so typing a
  // space still works). Focused buttons (the "Clear all" link-style button)
  // keep their native Enter/Space activation.
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      const onControl = document.activeElement instanceof HTMLButtonElement;
      if (event.key === 'Escape') {
        event.preventDefault();
        close(true);
      } else if (event.key === 'ArrowDown') {
        event.preventDefault();
        moveHighlight(1);
      } else if (event.key === 'ArrowUp') {
        event.preventDefault();
        moveHighlight(-1);
      } else if (event.key === 'Enter' && !onControl) {
        const row = highlight >= 0 ? rows[highlight] : undefined;
        if (row && !row.implied) {
          event.preventDefault();
          onToggle(row.id);
        }
      } else if (
        event.key === ' ' &&
        document.activeElement !== searchRef.current &&
        !onControl
      ) {
        const row = highlight >= 0 ? rows[highlight] : undefined;
        if (row && !row.implied) {
          event.preventDefault();
          onToggle(row.id);
        }
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [open, highlight, rows, onToggle, moveHighlight, close]);

  // Focus the search field on open.
  useEffect(() => {
    if (open) searchRef.current?.focus();
  }, [open]);

  // New query → new tree: highlight the first enabled row (or -1).
  useEffect(() => {
    setHighlight(firstEnabledIndex());
    // Reset on the query only — selection changes keep the highlight unless
    // it lands on a row the toggle just made implied (handled below).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  // A toggle can make the highlighted row implied (parent selected): drop the
  // highlight to the first enabled row instead of leaving it on a dead row.
  useEffect(() => {
    setHighlight((prev) =>
      prev >= 0 && (!rows[prev] || rows[prev].implied)
        ? firstEnabledIndex()
        : prev,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedIds]);

  // Keep the highlighted row visible inside the scrolling list.
  useEffect(() => {
    if (highlight < 0) return;
    const el = listRef.current?.querySelector(
      `[data-row-index="${highlight}"]`,
    );
    el?.scrollIntoView({ block: 'nearest' });
  }, [highlight]);

  const renderRow = (row: Row, index: number, indented: boolean) => (
    <div
      key={row.id}
      role="option"
      aria-selected={row.checked}
      aria-disabled={row.implied}
      data-row-index={index}
      onClick={() => {
        if (!row.implied) onToggle(row.id);
      }}
      className={cn(
        'flex items-center gap-2 rounded-lg pr-2 py-1.5 text-sm text-foreground',
        indented ? 'pl-6' : 'pl-2',
        index === highlight && 'bg-muted',
        row.implied
          ? 'cursor-default opacity-60'
          : 'cursor-pointer hover:bg-muted',
      )}
    >
      <input
        type="checkbox"
        checked={row.checked}
        disabled={row.implied}
        onChange={() => {
          if (!row.implied) onToggle(row.id);
        }}
        onClick={(event) => event.stopPropagation()}
        tabIndex={-1}
        className="h-4 w-4 shrink-0 accent-primary"
      />
      <span
        className="h-2 w-2 shrink-0 rounded-full"
        style={{ backgroundColor: row.color }}
        aria-hidden
      />
      <span className="min-w-0 flex-1 truncate">{row.title}</span>
    </div>
  );

  // Pinned-row chrome mirrors `renderRow` (checkbox, color dot, title) minus
  // everything that couples rows to the listbox keyboard model: no
  // `role="option"`, no `data-row-index`, no `bg-muted` highlight, no indent.
  // Mouse-only — ArrowUp/Down + Enter/Space keep walking the tree rows. The
  // child's parent title trails as secondary muted text instead of being
  // smashed into the title string.
  const renderPinnedRow = (row: PinnedRow) => (
    <div
      key={row.id}
      aria-disabled={row.implied}
      onClick={() => {
        if (!row.implied) onToggle(row.id);
      }}
      className={cn(
        'flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm text-foreground',
        row.implied
          ? 'cursor-default opacity-60'
          : 'cursor-pointer hover:bg-muted',
      )}
    >
      <input
        type="checkbox"
        checked={row.checked}
        disabled={row.implied}
        onChange={() => {
          if (!row.implied) onToggle(row.id);
        }}
        onClick={(event) => event.stopPropagation()}
        tabIndex={-1}
        className="h-4 w-4 shrink-0 accent-primary"
      />
      <span
        className="h-2 w-2 shrink-0 rounded-full"
        style={{ backgroundColor: row.color }}
        aria-hidden
      />
      <span className="min-w-0 flex-1 truncate">{row.title}</span>
      {row.parentTitle && (
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {row.parentTitle}
        </span>
      )}
    </div>
  );

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="Filter by category"
        className={cn(
          'flex min-w-[11rem] items-center justify-between gap-2 rounded-xl border-2 px-3 py-1.5 text-sm font-medium transition-all',
          selectedIds.length > 0
            ? 'bg-primary border-primary text-primary-foreground'
            : 'bg-background border-input text-muted-foreground hover:border-primary/30',
        )}
      >
        <span className="truncate">
          {categoryTriggerLabel(selectedIds, categories)}
        </span>
        <ChevronDown
          className={cn(
            'h-4 w-4 shrink-0 transition-transform',
            open && 'rotate-180',
          )}
          aria-hidden
        />
      </button>

      {/* `panelHost === null` — the sheet's host has not mounted yet — renders
          NOTHING (never a body-level fallback; see the note by `panelHost`). */}
      {open &&
        panelHost !== null &&
        createPortal(
          <div
            ref={panelRef}
            role="listbox"
            aria-multiselectable="true"
            data-category-filter-panel
            className={cn(
              'rounded-xl border border-border bg-card p-2 shadow-lg',
              // Sheet host: `absolute` inside a host that is a child of the
              // dialog content (Radix focus and outside-click still see a
              // descendant), plus `pointer-events-auto` because the host itself
              // ignores pointers so taps reach the form underneath.
              inHost ? 'absolute z-50 pointer-events-auto' : 'fixed z-50',
            )}
            style={
              pos
                ? { top: pos.top, left: pos.left, width: pos.width }
                : { visibility: 'hidden' }
            }
          >
            {/* Search + "Clear all" (above the scrolling list, always
                visible). Clear all clears categories only. */}
            <div className="flex items-center gap-2 pb-2">
              <input
                ref={searchRef}
                type="text"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search categories"
                aria-label="Search categories"
                className="w-full rounded-xl border border-input bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              {selectedIds.length > 0 && (
                <button
                  type="button"
                  onClick={onClear}
                  className="shrink-0 text-sm font-medium text-primary hover:underline"
                >
                  Clear all
                </button>
              )}
            </div>

            {/* Pinned explicit selections (ADR 0002 § UI): above the
                scrolling tree so a multi-select is visible without hunting
                through the taxonomy; ignores the search query. Rows stay in
                the tree too — pinning does not move them out. Hidden
                entirely while nothing is selected. */}
            {pinned.length > 0 && (
              <div
                role="group"
                aria-label="Selected categories"
                className="border-b border-border pb-2 mb-1"
              >
                <div className="px-2 pt-2 pb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                  Selected
                </div>
                {pinned.map(renderPinnedRow)}
              </div>
            )}

            {/* List headings are labels only — not selectable. */}
            <div ref={listRef} className="max-h-64 overflow-y-auto">
              {rows.length === 0 ? (
                <p className="px-2 py-3 text-sm text-muted-foreground italic">
                  No categories match
                </p>
              ) : (
                sections.map((section) => (
                  <div key={section.listId}>
                    <div className="px-2 pt-2 pb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                      {section.listName}
                    </div>
                    {section.entries.map((entry) =>
                      renderRow(entry.row, entry.index, entry.indented),
                    )}
                  </div>
                ))
              )}
            </div>
          </div>,
          panelHost,
        )}
    </>
  );
}

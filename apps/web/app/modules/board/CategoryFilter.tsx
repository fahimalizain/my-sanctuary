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
}: {
  lists: TaskList[];
  categories: Category[];
  selectedIds: string[];
  onToggle: (id: string) => void;
  onClear: () => void;
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
  // (capture: the board scroller scrolls too), resize and query changes (the
  // list height changes with the results). Flip above when there is not
  // enough room below.
  useLayoutEffect(() => {
    if (!open) return;
    const position = () => {
      const trigger = triggerRef.current;
      const panel = panelRef.current;
      if (!trigger || !panel) return;
      const rect = trigger.getBoundingClientRect();
      const width = Math.max(rect.width, 18 * 16); // min-width 18rem
      const gap = 8;
      const height = panel.offsetHeight;
      const spaceBelow = window.innerHeight - rect.bottom;
      const top =
        spaceBelow >= height + gap
          ? rect.bottom + gap
          : Math.max(gap, rect.top - height - gap);
      setPos({ top, left: rect.left, width });
    };
    position();
    window.addEventListener('scroll', position, true);
    window.addEventListener('resize', position);
    return () => {
      window.removeEventListener('scroll', position, true);
      window.removeEventListener('resize', position);
    };
  }, [open, query]);

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

      {open &&
        createPortal(
          <div
            ref={panelRef}
            role="listbox"
            aria-multiselectable="true"
            className="fixed z-50 rounded-xl border border-border bg-card p-2 shadow-lg"
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
          document.body,
        )}
    </>
  );
}

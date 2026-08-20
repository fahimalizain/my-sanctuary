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
import { buildCategoryTree } from '@/app/modules/board/board-filters';

// The single-select task-modal category lock. Selecting a row REPLACES the
// selection (there are no checkboxes and no implied children — the category
// is a lock the modal classifies under). The explicit "No category" row is
// the clear/lock-off option. `Untracked` is never a picker row.
//
// Visual cousin of the board's `CategoryFilter` (search, portaled panel,
// keyboard), sharing its grouped list → root → indented children tree via
// `buildCategoryTree` — but single select.

type Row = {
  id: string;
  title: string;
  color: string;
};

type Section = {
  listId: string;
  listName: string;
  entries: { row: Row; indented: boolean; index: number }[];
};

interface CategoryPickerProps {
  lists: TaskList[];
  categories: Category[];
  /** The locked category id, or `null` for "No category" (unlocked). */
  selectedId: string | null;
  /** `null` clears the lock. */
  onSelect: (id: string | null) => void;
  disabled?: boolean;
}

export function CategoryPicker({
  lists,
  categories,
  selectedId,
  onSelect,
  disabled,
}: CategoryPickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  // Index into the panel's flat list: 0 = the "No category" row, 1+ = the
  // tree rows in visual order (-1 = none).
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

  const selected = selectedId
    ? (categories.find((cat) => cat.id === selectedId) ?? null)
    : null;

  // Tree rows with flat indices starting at 1 (0 is reserved for the
  // "No category" row). The tree itself excludes untracked.
  const { sections, rows, treeRowCount } = useMemo(() => {
    const groups = buildCategoryTree(lists, categories, query);
    let index = 1;
    const sections: Section[] = groups.map((group) => ({
      listId: group.list.id,
      listName: group.list.name,
      entries: group.roots.flatMap((root) => [
        {
          row: {
            id: root.category.id,
            title: root.category.title,
            color: root.category.color,
          },
          indented: false,
          index: index++,
        },
        ...root.children.map((child) => ({
          row: { id: child.id, title: child.title, color: child.color },
          indented: true,
          index: index++,
        })),
      ]),
    }));
    const rows = sections.flatMap((section) =>
      section.entries.map((entry) => entry.row),
    );
    return { sections, rows, treeRowCount: index - 1 };
  }, [lists, categories, query]);

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    // Next open shows the full tree again.
    setQuery('');
    if (restoreFocus) triggerRef.current?.focus();
  }, []);

  /** Pick the highlighted row: 0 = clear, 1+ = its tree row. */
  const applyHighlight = useCallback(() => {
    if (highlight === 0) {
      onSelect(null);
      close(true);
      return;
    }
    const row = highlight > 0 ? rows[highlight - 1] : undefined;
    if (row) {
      onSelect(row.id);
      close(true);
    }
  }, [highlight, rows, onSelect, close]);

  /** Move the highlight among [0..treeRowCount], clamping at the ends. */
  const moveHighlight = useCallback(
    (dir: 1 | -1) => {
      setHighlight((prev) => {
        const total = treeRowCount + 1; // include the "No category" row
        if (total === 0) return -1;
        if (prev < 0) return dir === 1 ? 0 : total - 1;
        const next = prev + dir;
        if (next < 0) return 0;
        if (next >= total) return total - 1;
        return next;
      });
    },
    [treeRowCount],
  );

  // Position the panel from the trigger rect. Recompute on open, scroll
  // (capture), resize and query changes (the list height changes with the
  // results). Flips above when there is not enough room below.
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

  // Keyboard while open: Escape closes; ArrowUp/Down move the highlight;
  // Enter selects the highlighted row (search field included, nothing is
  // submitted); Space only outside the search input.
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
        event.preventDefault();
        applyHighlight();
      } else if (
        event.key === ' ' &&
        document.activeElement !== searchRef.current &&
        !onControl
      ) {
        event.preventDefault();
        applyHighlight();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [open, highlight, moveHighlight, applyHighlight, close]);

  // Focus the search field on open.
  useEffect(() => {
    if (open) searchRef.current?.focus();
  }, [open]);

  // New query → new tree: highlight the "No category" row (a safe default).
  useEffect(() => {
    setHighlight(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  // Keep the highlighted row visible inside the scrolling list (0 = the
  // "No category" row, which lives above the scroller in a fixed header).
  useEffect(() => {
    if (highlight <= 0) return;
    const el = listRef.current?.querySelector(
      `[data-row-index="${highlight}"]`,
    );
    el?.scrollIntoView({ block: 'nearest' });
  }, [highlight]);

  // Selecting a row always replaces the selection — single select.
  const handleRowClick = (row: Row) => {
    onSelect(row.id);
    close(true);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="Lock a category"
        className={cn(
          'flex w-full items-center justify-between gap-2 rounded-xl border-2 px-4 py-3 text-sm font-medium transition-all',
          disabled && 'cursor-not-allowed opacity-60',
          selected
            ? 'bg-primary border-primary text-primary-foreground'
            : 'bg-background border-input text-muted-foreground hover:border-primary/30',
        )}
      >
        <span className="truncate">{selected?.title ?? 'No category'}</span>
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
            aria-label="Categories"
            className="fixed z-50 rounded-xl border border-border bg-card p-2 shadow-lg"
            style={
              pos
                ? { top: pos.top, left: pos.left, width: pos.width }
                : { visibility: 'hidden' }
            }
          >
            {/* Search + the explicit clear row (above the tree, always
                visible so the lock can be released without a match). */}
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
            </div>

            {/* "No category" — the explicit empty state / clear row. */}
            <div
              role="option"
              aria-selected={selectedId === null}
              data-row-index={0}
              onClick={() => {
                onSelect(null);
                close(true);
              }}
              className={cn(
                'flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm border-b border-border mb-1',
                highlight === 0 && 'bg-muted',
                selectedId === null
                  ? 'text-primary font-medium'
                  : 'cursor-pointer text-muted-foreground hover:bg-muted',
              )}
            >
              <span
                className="h-2 w-2 shrink-0 rounded-full bg-muted-foreground/40"
                aria-hidden
              />
              <span className="min-w-0 flex-1 truncate">No category</span>
            </div>

            {/* List headings are labels only — not selectable. */}
            <div ref={listRef} className="max-h-56 overflow-y-auto">
              {rows.length === 0 ? (
                <p className="px-2 py-3 text-sm text-muted-foreground italic">
                  {query ? 'No categories match' : 'No categories yet'}
                </p>
              ) : (
                sections.map((section) => (
                  <div key={section.listId}>
                    <div className="px-2 pt-2 pb-1 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                      {section.listName}
                    </div>
                    {section.entries.map((entry) => (
                      <div
                        key={entry.row.id}
                        role="option"
                        aria-selected={selectedId === entry.row.id}
                        data-row-index={entry.index}
                        onClick={() => handleRowClick(entry.row)}
                        className={cn(
                          'flex items-center gap-2 rounded-lg pr-2 py-1.5 text-sm text-foreground cursor-pointer',
                          entry.indented ? 'pl-6' : 'pl-2',
                          entry.index === highlight && 'bg-muted',
                          selectedId === entry.row.id &&
                            'text-primary font-medium',
                          entry.index !== highlight &&
                            selectedId !== entry.row.id &&
                            'hover:bg-muted',
                        )}
                      >
                        <span
                          className="h-2 w-2 shrink-0 rounded-full"
                          style={{ backgroundColor: entry.row.color }}
                          aria-hidden
                        />
                        <span className="min-w-0 flex-1 truncate">
                          {entry.row.title}
                        </span>
                      </div>
                    ))}
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

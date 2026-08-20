import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronDown, X } from 'lucide-react';
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
  /** The id shown as selected: an explicit user lock, or (when unlocked) the
   *  category a unique classify match found. Never sent to the API by itself
   *  — only the lock is. */
  selectedId: string | null;
  /** True only for an explicit user lock (`lockId !== null`) — drives the
   *  clear X on the trigger. A classify-only suggestion has no X. */
  hasLock: boolean;
  /** `null` clears the lock. */
  onSelect: (id: string | null) => void;
  disabled?: boolean;
}

export function CategoryPicker({
  lists,
  categories,
  selectedId,
  hasLock,
  onSelect,
  disabled,
}: CategoryPickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  // Index into the panel's flat list: 0 = the "No category" row, 1+ = the
  // tree rows in visual order (-1 = none).
  const [highlight, setHighlight] = useState(-1);

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

  // The panel renders `absolute` under the trigger inside its `relative`
  // wrapper, so it scrolls with the modal's content container and needs no
  // viewport math. Clicks on it stay inside DialogContent, so the Radix
  // dialog's outside-click handler never closes the modal on us.

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

  /** Clear the lock from the trigger X. `stopPropagation` keeps the click
   *  from reaching the trigger's own onClick, so the panel never toggles. */
  const handleClearLock = (event: React.MouseEvent) => {
    event.stopPropagation();
    onSelect(null);
  };

  return (
    <div className="relative">
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
        <span className="min-w-0 flex-1 truncate">
          {selected?.title ?? 'No category'}
        </span>
        <span className="flex shrink-0 items-center gap-1">
          {hasLock && (
            <span
              role="button"
              tabIndex={-1}
              aria-label="Remove category lock"
              onClick={handleClearLock}
              className="rounded-full p-1 -mr-1 text-primary-foreground/80 transition-colors hover:bg-black/10 hover:text-primary-foreground"
            >
              <X className="h-4 w-4 shrink-0" aria-hidden />
            </span>
          )}
          <ChevronDown
            className={cn(
              'h-4 w-4 shrink-0 transition-transform',
              open && 'rotate-180',
            )}
            aria-hidden
          />
        </span>
      </button>

      {open && (
        <div
          ref={panelRef}
          role="listbox"
          aria-label="Categories"
          className="absolute left-0 right-0 z-50 mt-2 rounded-xl border border-border bg-card p-2 shadow-lg"
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
              'flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-sm border-b border-border mb-1',
              highlight === 0 && 'bg-muted',
              selectedId === null
                ? 'text-primary font-medium'
                : 'text-muted-foreground hover:bg-muted',
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
        </div>
      )}
    </div>
  );
}

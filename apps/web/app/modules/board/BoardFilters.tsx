import { useMemo } from 'react';
import { cn } from '@/lib/utils';
import {
  TASK_PRIORITIES,
  TASK_PRIORITY_LABELS,
  type Category,
  type TaskDifficulty,
  type TaskList,
  type TaskPriority,
} from '@/app/types';
import { CategoryFilter } from './CategoryFilter';
import { FilterPill } from './FilterPill';
import { countActiveFilterGroups } from './board-filters';

/** The board's filter controls (ADR 0002 § UI), shared by the desktop row
 *  (`layout="row"`) and the mobile bottom sheet (`layout="stack"`).
 *
 *  Priority and Difficulty are single-select All+value pills; Category is a
 *  searchable checkbox combobox grouped by list → root → children; Search is
 *  a free-text title/description filter driven by `?search=` (each keystroke
 *  rewrites the param, whitespace-only removes it). Clear filters sits at the
 *  end of the same row, aligned with the controls. Filters combine with AND.
 *
 *  Presentational only: it never reads the URL or navigates — every write
 *  goes through the caller's `on*` handlers, so both layouts stay wired to
 *  the same page state. */
interface BoardFiltersProps {
  /** Desktop wrapping row vs. the mobile sheet's full-width stack. */
  layout: 'row' | 'stack';
  priority: TaskPriority | undefined;
  difficulty: TaskDifficulty | undefined;
  /** Raw `?search=` — the component trims only for its own derived state. */
  search: string | undefined;
  lists: TaskList[];
  categories: Category[];
  /** Resolved explicit ids: unknown URL ids already dropped (never the raw
   *  `?category=` string). */
  selectedCategoryIds: string[];
  onPriority: (value: TaskPriority | undefined) => void;
  onDifficulty: (value: TaskDifficulty | undefined) => void;
  onToggleCategory: (id: string) => void;
  /** Categories only — NOT the same as `onClearFilters` (ADR 0002 § UI). */
  onClearCategories: () => void;
  onSearch: (value: string) => void;
  /** Clears priority + difficulty + categories + search. */
  onClearFilters: () => void;
  /** Input/label id pair. The row keeps the default; the sheet passes its
   *  own so two mounted copies can never collide on `htmlFor`. */
  searchInputId?: string;
}

export function BoardFilters({
  layout,
  priority,
  difficulty,
  search,
  lists,
  categories,
  selectedCategoryIds,
  onPriority,
  onDifficulty,
  onToggleCategory,
  onClearCategories,
  onSearch,
  onClearFilters,
  searchInputId = 'board-task-search',
}: BoardFiltersProps) {
  // Untracked is the system sink: never a picker row and never a filter chip.
  // A `?category=` naming it is still resolved by the page (and matches
  // untracked cards, which stay visible in their columns) — this only decides
  // whether the combobox, or the "No categories yet" fallback, is rendered.
  const filterableCategories = useMemo(
    () => categories.filter((cat) => !cat.is_untracked),
    [categories],
  );

  // Derived from the same inputs the controls write, never handed in as a
  // boolean: a prop could drift from the URL it describes. The mobile badge
  // reads the same helper, so the two can never disagree.
  const hasActiveFilters =
    countActiveFilterGroups({
      priority,
      difficulty,
      categoryIds: selectedCategoryIds,
      search,
    }) > 0;

  return (
    <div
      className={cn(
        layout === 'row'
          ? 'flex flex-wrap items-end gap-x-6 gap-y-3'
          : 'flex flex-col items-stretch gap-5',
      )}
    >
      <div className="flex flex-col items-start gap-1.5">
        <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Priority
        </span>
        <div className="flex flex-wrap items-center gap-2">
          <FilterPill
            selected={priority === undefined}
            onClick={() => onPriority(undefined)}
          >
            All
          </FilterPill>
          {TASK_PRIORITIES.map((value) => (
            <FilterPill
              key={value}
              selected={priority === value}
              onClick={() => onPriority(value)}
            >
              {TASK_PRIORITY_LABELS[value]}
            </FilterPill>
          ))}
        </div>
      </div>

      <div className="flex flex-col items-start gap-1.5">
        <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Difficulty
        </span>
        <div className="flex flex-wrap items-center gap-2">
          <FilterPill
            selected={difficulty === undefined}
            onClick={() => onDifficulty(undefined)}
          >
            All
          </FilterPill>
          {(['easy', 'medium', 'hard'] as TaskDifficulty[]).map((value) => (
            <FilterPill
              key={value}
              selected={difficulty === value}
              onClick={() => onDifficulty(value)}
            >
              {value[0].toUpperCase() + value.slice(1)}
            </FilterPill>
          ))}
        </div>
      </div>

      <div className="flex flex-col items-start gap-1.5">
        <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Category
        </span>
        {filterableCategories.length > 0 ? (
          <CategoryFilter
            lists={lists}
            categories={categories}
            selectedIds={selectedCategoryIds}
            onToggle={onToggleCategory}
            onClear={onClearCategories}
          />
        ) : (
          <span className="text-sm text-muted-foreground italic">
            No categories yet
          </span>
        )}
      </div>

      <div className="flex flex-col items-start gap-1.5">
        <label
          htmlFor={searchInputId}
          className="text-xs font-medium uppercase tracking-wide text-muted-foreground"
        >
          Search
        </label>
        {/* type="text" (never "search" — that paints a native cancel
            button); every keystroke writes `?search=` via the caller's
            handler (replace: true), which treats whitespace-only as delete.
            Focus border only — never the primary fill. */}
        <input
          id={searchInputId}
          type="text"
          value={search ?? ''}
          onChange={(event) => onSearch(event.target.value)}
          placeholder="Search tasks"
          className={cn(
            'min-w-0 rounded-xl border-2 border-input bg-background px-3 py-1.5 text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:border-primary transition-all',
            layout === 'row' ? 'w-56' : 'w-full',
          )}
        />
      </div>

      {hasActiveFilters && (
        <button
          type="button"
          onClick={onClearFilters}
          className="shrink-0 py-1.5 text-sm font-medium text-primary hover:underline"
        >
          Clear filters
        </button>
      )}
    </div>
  );
}

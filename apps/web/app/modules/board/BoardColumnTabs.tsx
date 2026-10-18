import { cn } from '@/lib/utils';
import type { TaskStatus } from '@/app/types';

/** One control per column, shown below md only, directly under the board
 *  header and above the column scroller. Tapping a control smooth-scrolls
 *  the scroller to that column; `activeStatus` tracks whichever column is
 *  currently in view (IntersectionObserver in BoardPage), so the highlight
 *  follows both taps and manual swipes.
 *
 *  The five full titles must stay on ONE line at 390px: the nav itself pans
 *  horizontally (`overflow-x-auto`) and every control is `shrink-0
 *  whitespace-nowrap`, so titles never wrap or truncate. From md up the row
 *  is hidden — the columns are all visible and it would waste height.
 *
 *  Presentational only: counts and titles come from the caller (the same
 *  displayed array the column header pills count), and it never scrolls
 *  anything itself. Thin by design — this is not a `FilterPill` and not the
 *  `Badge` component. */
interface BoardColumnTabsProps {
  /** One entry per board column, in COLUMNS order. */
  tabs: { status: TaskStatus; title: string; count: number }[];
  /** Column currently in view (or just tapped). */
  activeStatus: TaskStatus;
  /** Tap → smooth-scroll the scroller to that column. */
  onSelect: (status: TaskStatus) => void;
}

export function BoardColumnTabs({
  tabs,
  activeStatus,
  onSelect,
}: BoardColumnTabsProps) {
  return (
    <nav
      aria-label="Columns"
      className="mb-2 flex shrink-0 flex-nowrap items-center gap-1 overflow-x-auto overscroll-x-contain px-4 md:hidden"
    >
      {tabs.map((tab) => {
        const selected = tab.status === activeStatus;
        return (
          <button
            key={tab.status}
            type="button"
            aria-pressed={selected}
            // The visible text already carries the count, but each control
            // is a standalone target for screen readers — spell both parts
            // out so "3" never arrives without its column.
            aria-label={`${tab.title}, ${tab.count} tasks`}
            onClick={() => onSelect(tab.status)}
            className={cn(
              'shrink-0 whitespace-nowrap rounded-full px-2.5 py-1 text-xs font-medium',
              selected
                ? 'bg-primary text-primary-foreground'
                : 'text-muted-foreground',
            )}
          >
            {tab.title}
            <span className="ml-1 tabular-nums">{tab.count}</span>
          </button>
        );
      })}
    </nav>
  );
}

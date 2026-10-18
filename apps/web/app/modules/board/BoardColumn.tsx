import { useDndContext, useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { Plus } from 'lucide-react';
import { useLayoutEffect, useRef, useState, type RefObject } from 'react';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';
import type { TaskRecord, TaskStatus } from '@/app/types';
import { COLUMN_ID_PREFIX } from './board-model';
import type { BoardColumn } from './board-model';
import { scrollEdges } from './board-scroll';
import { SortableTaskCard } from './TaskCard';

/** Top / bottom fade flags for the card list. Re-measures on scroll, on a
 *  resize of the list (viewport or header chrome) and whenever `contentKey`
 *  changes — the list's border box does not resize when cards are added or
 *  filtered, so the caller passes the card count to cover that. Stays
 *  opt-in: a flip renders once, never once per scroll frame. */
function useColumnScrollEdges(
  ref: RefObject<HTMLDivElement | null>,
  contentKey: number,
): { top: boolean; bottom: boolean } {
  const [edges, setEdges] = useState({ top: false, bottom: false });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;

    const update = () => {
      const next = scrollEdges(el.scrollTop, el.clientHeight, el.scrollHeight);
      // Same flags → keep the old object so React bails out. A fresh object
      // on every scroll event would re-render the column per frame.
      setEdges((prev) =>
        prev.top === next.top && prev.bottom === next.bottom ? prev : next,
      );
    };

    update();
    el.addEventListener('scroll', update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(el);

    return () => {
      el.removeEventListener('scroll', update);
      observer.disconnect();
    };
  }, [ref, contentKey]);

  return edges;
}

/** One board column: neutral surface with a 2px status accent on the header
 *  only. The count is the number of cards currently shown (after filter +
 *  cap) — never a `20 / 64` overflow hint. The section stretches to the
 *  row's leftover viewport height (`h-full`) and clips for its rounded
 *  corners; the only scrolling box is the CARD LIST, which is also the
 *  column droppable (`column:<status>`) — its border box is the visible
 *  column body that `pointerWithin` measures. The list fills the remaining
 *  height of that bounded section (a `flex-1` wrapper around the `h-full`
 *  scroller) even with one card, so the empty space below the cards (and
 *  whole empty columns) still accepts a drop. The cards inside remain a
 *  vertical SortableContext. A pointer-transparent fade marks each edge
 *  with cards scrolled out past it; the fades are siblings of the scroller
 *  (not inside it, where they would scroll away) and are neither
 *  droppables nor sortables.
 *  `85vw` below md (the scroller snap-scrolls those and the next column
 *  peeks in) and a fixed 260px from md up, where `snap-start` is inert
 *  because the scroller is not a snap container — and it is inert during a
 *  drag at every width, since the scroller only turns snapping on while
 *  `activeDrag === null`. */
export function BoardColumnView({
  column,
  tasks,
  items,
  movingIds,
  onEditTask,
  onToggleFocus,
  focusDisabled,
  onAddTask,
}: {
  column: BoardColumn;
  tasks: TaskRecord[];
  /** Displayed task ids — matched 1:1 with `tasks`, feeds SortableContext. */
  items: string[];
  /** Cards with a /move in flight: their drag handlers are disabled. */
  movingIds: Set<string>;
  onEditTask: (task: TaskRecord) => void;
  /** Focus-pin tap → BoardPage.handleToggleFocus (see TaskCard). */
  onToggleFocus?: (task: TaskRecord) => void;
  /** A focus request is in flight: every pin is disabled. */
  focusDisabled?: boolean;
  /** Column-header + : opens New Task locked to this column's status. */
  onAddTask: (status: TaskStatus) => void;
}) {
  const columnDroppableId = `${COLUMN_ID_PREFIX}${column.status}`;
  const { setNodeRef, isOver } = useDroppable({
    id: columnDroppableId,
    data: { type: 'column', status: column.status },
  });
  // `isOver` is only true when the winning collision is the column itself
  // (the empty body). On a card, `over` is that card — the ring must still
  // light the parent column, so any card of this column counts as over.
  const { over } = useDndContext();
  const overThisColumn =
    isOver || (over != null && items.includes(String(over.id)));

  // One callback for the scroller: the measurement hook needs the element,
  // and `setNodeRef` still has to see the same node for the droppable (and
  // `null` on unmount).
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const setScrollNode = (node: HTMLDivElement | null) => {
    scrollRef.current = node;
    setNodeRef(node);
  };
  // Card count is the content key: adding / removing / filtering cards
  // shifts scrollHeight without resizing the list's own box, and a pure
  // reorder never flips an edge.
  const edges = useColumnScrollEdges(scrollRef, tasks.length);

  return (
    <section
      // Anchor for the mobile column tabs (BoardColumnTabs): a bare status
      // id is space-free, so `querySelector` can address the column
      // directly. The value is a TaskStatus, stable across renders.
      data-board-column={column.status}
      className={cn(
        'flex h-full min-h-0 w-[85vw] shrink-0 snap-start flex-col overflow-hidden rounded-xl border bg-card transition-colors md:w-[260px]',
        overThisColumn
          ? 'border-primary/60 ring-2 ring-primary/20'
          : 'border-border',
      )}
    >
      <header className="shrink-0 border-b border-border">
        <div className={cn('h-0.5', column.accent)} />
        <div className="flex items-center justify-between gap-2 px-4 py-3">
          {/* Title + count grouped on the left (New Task + is on the right) */}
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="font-heading text-sm font-semibold text-foreground truncate">
              {column.title}
            </h3>
            <span
              className="flex-shrink-0 rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground"
              aria-label={`${tasks.length} tasks`}
            >
              {tasks.length}
            </span>
          </div>
          {/* Column + — the header is outside the column droppable (the
              card list is), so stopPropagation is just belt-and-braces */}
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-label={`Add task to ${column.title}`}
            className="h-7 w-7 flex-shrink-0"
            onClick={(event) => {
              event.stopPropagation();
              onAddTask(column.status);
            }}
          >
            <Plus className="h-4 w-4" />
          </Button>
        </div>
      </header>

      <SortableContext items={items} strategy={verticalListSortingStrategy}>
        {/* Wrapper owns the flex sizing; the scroller below fills it and
            stays the droppable (`setNodeRef` must not move up here). */}
        <div className="relative min-h-0 flex-1">
          <div
            ref={setScrollNode}
            className={cn(
              'h-full min-h-0 space-y-2 overflow-y-auto overscroll-contain p-3 transition-colors',
              overThisColumn && tasks.length === 0 && 'bg-muted/40 rounded-lg',
            )}
          >
            {tasks.length > 0 ? (
              tasks.map((task) => (
                <SortableTaskCard
                  key={task.id}
                  task={task}
                  columnStatus={column.status}
                  onEdit={onEditTask}
                  onToggleFocus={onToggleFocus}
                  focusDisabled={focusDisabled}
                  disabled={movingIds.has(task.id)}
                />
              ))
            ) : (
              <p className="text-sm text-muted-foreground italic">No tasks</p>
            )}
          </div>
          <div
            aria-hidden
            className={cn(
              'pointer-events-none absolute inset-x-0 top-0 z-10 h-3 bg-gradient-to-b from-card to-transparent',
              !edges.top && 'opacity-0',
            )}
          />
          <div
            aria-hidden
            className={cn(
              'pointer-events-none absolute inset-x-0 bottom-0 z-10 h-3 bg-gradient-to-t from-card to-transparent',
              !edges.bottom && 'opacity-0',
            )}
          />
        </div>
      </SortableContext>
    </section>
  );
}

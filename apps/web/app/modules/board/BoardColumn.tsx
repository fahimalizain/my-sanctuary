import { useDndContext, useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/lib/utils';
import type { TaskRecord, TaskStatus } from '@/app/types';
import { COLUMN_ID_PREFIX } from './board-model';
import type { BoardColumn } from './board-model';
import { SortableTaskCard } from './TaskCard';

/** One board column: neutral surface with a 2px status accent on the header
 *  only. The count is the number of cards currently shown (after filter +
 *  cap) — never a `20 / 64` overflow hint. The CARD LIST is the column
 *  droppable (`column:<status>`), so the empty space below the cards (and
 *  whole empty columns) accept a drop; the cards inside remain a vertical
 *  SortableContext. */
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
    data: { type: 'column' },
  });
  // `isOver` is only true when the winning collision is the column itself
  // (the empty body). On a card, `over` is that card — the ring must still
  // light the parent column, so any card of this column counts as over.
  const { over } = useDndContext();
  const overThisColumn =
    isOver || (over != null && items.includes(String(over.id)));

  return (
    <section
      className={cn(
        'flex w-[260px] shrink-0 flex-col overflow-hidden rounded-xl border bg-card transition-colors',
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
        <div
          ref={setNodeRef}
          className={cn(
            'flex-1 space-y-2 p-3 transition-colors',
            overThisColumn &&
              tasks.length === 0 &&
              'bg-muted/40 rounded-lg',
          )}
        >
          {tasks.length > 0 ? (
            tasks.map((task) => (
              <SortableTaskCard
                key={task.id}
                task={task}
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
      </SortableContext>
    </section>
  );
}

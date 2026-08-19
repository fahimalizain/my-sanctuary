import { useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { cn } from '@/lib/utils';
import type { TaskRecord } from '@/app/types';
import { COLUMN_ID_PREFIX } from './board-model';
import type { BoardColumn } from './board-model';
import { SortableTaskCard } from './TaskCard';

/** One board column: neutral surface with a 2px status accent on the header
 *  only. The count is the number of cards currently shown (after filter +
 *  cap) — never a `20 / 64` overflow hint. The whole section is the column
 *  droppable (`column:<status>`), so EMPTY columns accept a drop; the cards
 *  inside are a vertical SortableContext. */
export function BoardColumnView({
  column,
  tasks,
  items,
  movingIds,
  onEditTask,
}: {
  column: BoardColumn;
  tasks: TaskRecord[];
  /** Displayed task ids — matched 1:1 with `tasks`, feeds SortableContext. */
  items: string[];
  /** Cards with a /move in flight: their drag handlers are disabled. */
  movingIds: Set<string>;
  onEditTask: (task: TaskRecord) => void;
}) {
  const columnDroppableId = `${COLUMN_ID_PREFIX}${column.status}`;
  const { setNodeRef, isOver } = useDroppable({
    id: columnDroppableId,
    data: { type: 'column' },
  });

  return (
    <section
      ref={setNodeRef}
      className={cn(
        'flex min-w-[260px] flex-1 flex-col overflow-hidden rounded-xl border bg-card transition-colors',
        isOver ? 'border-primary/60 ring-2 ring-primary/20' : 'border-border',
      )}
    >
      <header className="shrink-0 border-b border-border">
        <div className={cn('h-0.5', column.accent)} />
        <div className="flex items-center justify-between gap-2 px-4 py-3">
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
      </header>

      <SortableContext items={items} strategy={verticalListSortingStrategy}>
        <div
          className={cn(
            'flex-1 space-y-2 p-3 transition-colors',
            isOver && tasks.length === 0 && 'bg-muted/40 rounded-lg',
          )}
        >
          {tasks.length > 0 ? (
            tasks.map((task) => (
              <SortableTaskCard
                key={task.id}
                task={task}
                onEdit={onEditTask}
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

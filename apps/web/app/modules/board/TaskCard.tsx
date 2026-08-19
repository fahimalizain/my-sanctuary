import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { cn } from '@/lib/utils';
import type { TaskRecord } from '@/app/types';

/** The draggable wrapper of a card: applies the sortable transform while the
 *  card itself stays the plain TaskCard chip (click-to-edit, no timer
 *  buttons). While dragging, the original is dimmed — the DragOverlay copy
 *  is the card the pointer actually holds. */
export function SortableTaskCard({
  task,
  onEdit,
  disabled,
}: {
  task: TaskRecord;
  onEdit: (task: TaskRecord) => void;
  disabled: boolean;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({
    id: task.id,
    disabled,
    data: { type: 'task' },
  });

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className={cn(isDragging && 'opacity-40')}
      {...attributes}
      {...listeners}
    >
      <TaskCard task={task} onEdit={onEdit} />
    </div>
  );
}

/** A light-surface task chip (the Lists chip on a light card, not the dark
 *  list-colored chip): title + duration + difficulty badge (medium/hard
 *  only) + priority dot + category swatch. The whole chip is the click
 *  target that opens the edit modal — no timer buttons in this slice. The
 *  drag overlay renders the same chip elevated (shadow/opacity), without a
 *  click target. */
export function TaskCard({
  task,
  onEdit,
}: {
  task: TaskRecord;
  onEdit?: (task: TaskRecord) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onEdit?.(task)}
      title={`${task.title} — ${task.duration_minutes} min, ${task.priority}${
        task.difficulty !== 'easy' ? `, ${task.difficulty}` : ''
      }`}
      className="flex w-full cursor-pointer items-center gap-1.5 rounded-lg border border-border/60 bg-background px-2.5 py-2 text-left transition-colors hover:border-primary/40 hover:bg-muted/40"
    >
      <span className="flex-1 min-w-0 text-sm text-foreground truncate">
        {task.title}
      </span>
      <span className="flex-shrink-0 text-[10px] text-muted-foreground">
        {task.duration_minutes} min
      </span>
      {task.difficulty === 'hard' || task.difficulty === 'medium' ? (
        <span
          className={cn(
            'flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] uppercase tracking-wide',
            task.difficulty === 'hard'
              ? 'bg-foreground/10 font-semibold text-foreground'
              : 'bg-muted font-medium text-muted-foreground',
          )}
        >
          {task.difficulty === 'hard' ? 'HARD' : 'MED'}
        </span>
      ) : null}
      <span
        className={cn(
          'h-1.5 w-1.5 rounded-full flex-shrink-0',
          task.priority === 'high'
            ? 'bg-red-400'
            : task.priority === 'medium'
              ? 'bg-amber-400'
              : 'bg-sky-400',
        )}
        aria-hidden
      />
      <span
        className="h-2 w-2 rounded-full flex-shrink-0"
        style={{ backgroundColor: task.category.color }}
        title={task.category.title}
        aria-hidden
      />
    </button>
  );
}

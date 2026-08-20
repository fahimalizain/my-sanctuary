import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { cn } from '@/lib/utils';
import { TASK_PRIORITY_LABELS, type TaskRecord } from '@/app/types';

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
 *  list-colored chip): category color as a left-edge ribbon; title wraps
 *  up to two lines then ellipsizes; duration + difficulty badge
 *  (medium/hard only) + P0/P1 (P2 hidden like easy) + category pill
 *  (untracked hidden) on the row below.
 *  The whole chip is the click target that opens the edit modal — no timer
 *  buttons in this slice. The drag overlay renders the same chip elevated
 *  (shadow/opacity), without a click target. */
export function TaskCard({
  task,
  onEdit,
}: {
  task: TaskRecord;
  onEdit?: (task: TaskRecord) => void;
}) {
  const categoryColor = task.category.color.trim();

  return (
    <button
      type="button"
      onClick={() => onEdit?.(task)}
      title={`${task.title} — ${task.duration_minutes} min${
        task.priority !== 'low'
          ? `, ${TASK_PRIORITY_LABELS[task.priority]}`
          : ''
      }${task.difficulty !== 'easy' ? `, ${task.difficulty}` : ''}${
        task.category.title ? `, ${task.category.title}` : ''
      }`}
      className="flex w-full cursor-pointer overflow-hidden rounded-lg border border-border/60 bg-background text-left transition-colors hover:border-primary/40 hover:bg-muted/40"
    >
      {/* Blank color (untracked sink / some children) falls back to muted. */}
      <span
        className={cn(
          'w-1 shrink-0 self-stretch',
          !categoryColor && 'bg-muted-foreground/40',
        )}
        style={categoryColor ? { backgroundColor: categoryColor } : undefined}
        aria-hidden
      />
      <span className="flex min-w-0 flex-1 flex-col gap-1 px-2.5 py-2">
        <span className="min-w-0 text-sm text-foreground line-clamp-2 break-words">
          {task.display_title}
        </span>
        <span className="flex min-w-0 items-center gap-1.5">
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
          {task.priority === 'high' || task.priority === 'medium' ? (
            <span
              className={cn(
                'flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] font-semibold tracking-wide',
                task.priority === 'high'
                  ? 'bg-red-400/15 text-red-500'
                  : 'bg-amber-400/15 text-amber-600',
              )}
            >
              {TASK_PRIORITY_LABELS[task.priority]}
            </span>
          ) : null}
          {task.category.title && !task.category.is_untracked ? (
            <span className="ml-auto min-w-0 truncate rounded-full bg-muted px-1.5 py-0.5 text-[9px] font-medium text-muted-foreground">
              {task.category.title}
            </span>
          ) : null}
        </span>
      </span>
    </button>
  );
}

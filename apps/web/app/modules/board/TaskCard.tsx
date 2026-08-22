import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { Pin } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  TASK_PRIORITY_LABELS,
  type TaskRecord,
  type TaskStatus,
} from '@/app/types';
import { focusPinVisibility } from './focus-pin';

/** The draggable wrapper of a card: applies the sortable transform while the
 *  card itself stays the plain TaskCard chip (click-to-edit, focus pin). While
 *  dragging, the original is dimmed — the DragOverlay copy is the card the
 *  pointer actually holds. */
export function SortableTaskCard({
  task,
  columnStatus,
  onEdit,
  onToggleFocus,
  focusDisabled,
  disabled,
}: {
  task: TaskRecord;
  /** The column rendering this card — NOT task.status, which stays the
   *  source while the live preview shows the card inside another column. */
  columnStatus: TaskStatus;
  onEdit: (task: TaskRecord) => void;
  onToggleFocus?: (task: TaskRecord) => void;
  focusDisabled?: boolean;
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
    data: { type: 'task', status: columnStatus },
  });

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className={cn(isDragging && 'opacity-40')}
      {...attributes}
      {...listeners}
    >
      <TaskCard
        task={task}
        onEdit={onEdit}
        onToggleFocus={onToggleFocus}
        focusDisabled={focusDisabled}
      />
    </div>
  );
}

/** A light-surface task chip (the Lists chip on a light card, not the dark
 *  list-colored chip): category color as a left-edge ribbon; title wraps
 *  up to two lines then ellipsizes; duration + difficulty badge
 *  (medium/hard only) + P0/P1 (P2 hidden like easy) + category pill
 *  (untracked hidden) on the row below.
 *
 *  The root is a `<div>` — NOT a button — so the focus pin can be a real
 *  `<button>` inside it (no nested-button a11y violation). The click-to-edit
 *  lives on the main body; the pin swallows its own pointerdown/click so it
 *  neither opens the modal nor starts a drag. The drag overlay renders the
 *  same chip elevated (shadow/opacity) with `onToggleFocus` omitted — the
 *  pin shows but stays inert (disabled).
 *
 *  Focus chrome (task-focus, slice 4): the focused card carries an always-on
 *  primary ring/border and an always-visible filled pin; the unfocused IP
 *  pin fades in on card hover (fine pointers) and is always visible on
 *  coarse / no-hover pointers. */
export function TaskCard({
  task,
  onEdit,
  onToggleFocus,
  focusDisabled,
}: {
  task: TaskRecord;
  onEdit?: (task: TaskRecord) => void;
  onToggleFocus?: (task: TaskRecord) => void;
  focusDisabled?: boolean;
}) {
  const categoryColor = task.category.color.trim();
  // Focus is IN_PROGRESS-only (the API 400s every other status), so the pin
  // only exists on IP cards — nothing to render elsewhere.
  const showPin = task.status === 'IN_PROGRESS';
  const focused = task.focused;

  return (
    <div
      className={cn(
        'group flex w-full overflow-hidden rounded-lg border bg-background transition-colors hover:bg-muted/40',
        focused
          ? 'border-primary/40 ring-2 ring-primary/50'
          : 'border-border/60 hover:border-primary/40',
      )}
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
      {/* The click-to-edit target: the card body, not the decorative ribbon
          and never the pin (the pin stops propagation). */}
      <div
        className="flex min-w-0 flex-1 cursor-pointer flex-col gap-1 px-2.5 py-2 text-left"
        onClick={() => onEdit?.(task)}
        title={`${task.title} — ${task.duration_minutes} min${
          task.priority !== 'low'
            ? `, ${TASK_PRIORITY_LABELS[task.priority]}`
            : ''
        }${task.difficulty !== 'easy' ? `, ${task.difficulty}` : ''}${
          task.category.title ? `, ${task.category.title}` : ''
        }`}
      >
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
          {/* Right-aligned group: the category pill, then the focus pin. */}
          <span className="ml-auto flex min-w-0 items-center gap-1">
            {task.category.title && !task.category.is_untracked ? (
              <span className="min-w-0 truncate rounded-full bg-muted px-1.5 py-0.5 text-[9px] font-medium text-muted-foreground">
                {task.category.title}
              </span>
            ) : null}
            {showPin && (
              <button
                type="button"
                aria-label={focused ? 'Unfocus' : 'Focus'}
                aria-pressed={focused}
                disabled={focusDisabled || !onToggleFocus}
                // Pin presses must NOT open the edit modal and must NOT start
                // a drag: pointerdown would reach the dnd-kit listeners on
                // SortableTaskCard and click would reach the body's
                // click-to-edit, so both stop propagation.
                onPointerDown={(event) => event.stopPropagation()}
                onClick={(event) => {
                  event.stopPropagation();
                  onToggleFocus?.(task);
                }}
                className={cn(
                  'flex h-5 w-5 flex-shrink-0 items-center justify-center rounded-md text-muted-foreground transition-opacity disabled:cursor-not-allowed',
                  focused && 'text-primary',
                  focusPinVisibility(focused),
                )}
              >
                <Pin className={cn('h-3.5 w-3.5', focused && 'fill-current')} />
              </button>
            )}
          </span>
        </span>
      </div>
    </div>
  );
}

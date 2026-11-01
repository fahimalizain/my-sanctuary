// One agenda row for Home (ADR 0004 § Surfaces — Home): the occurrence
// variant (checkbox toggles done ↔ pending, status chip, skip toggles
// skipped ↔ pending hidden while done, start, pause while in progress —
// ADR 0004 `/pause` lands pending + clears chip ids — rename, reschedule
// popover) and the
// task variant (checkbox completes a living row and reopens a finished one,
// start, pause while in progress — ADR 0002 `/pause` lands PLANNED —
// reschedule popover, tap → TaskModal). Living rows lead with a grab handle
// (ADR 0004 amendment — reorder is a handle): the grip is the ONLY drag
// activator, so a tap on checkbox / title / skip / play / pause / calendar
// never starts a drag. Parked rows (the Completed dump — ADR 0004 amendment)
// render the same card WITHOUT the grip. A parked `DISCARDED` task is
// finished too: struck through like a done row, its check circle replaced by
// a red X — clicking the X reopens it to PLANNED (ADR 0002), it never
// completes it. Occurrence rows of a soft-deleted routine
// (`routine_deleted`) are read-only history: no checkbox, skip, reschedule,
// rename, play or grip — just the muted hint. While one of the row's own
// mutations is in flight (`pending`) that slot swaps its glyph for a spinner
// in the same box, and the other mutating controls on the row go disabled so
// a second click can never race it. Pure presentational — every mutation lives
// in HomePage; HomePage owns the verb branch (reopen vs complete/skip).

import { Children, type ReactNode } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type {
  DraggableAttributes,
  DraggableSyntheticListeners,
} from '@dnd-kit/core';
import {
  CalendarX2,
  Check,
  GripVertical,
  Loader2,
  Pause,
  Pencil,
  Play,
  X,
} from 'lucide-react';
import type {
  AgendaItemRecord,
  OccurrenceRecord,
  TaskCategorySummary,
  TaskPriority,
  TaskRecord,
} from '../../types';
import { TASK_PRIORITY_LABELS } from '../../types';
import { cn } from '@/lib/utils';
import { canReschedule } from './agenda-helpers';
import { RescheduleControl } from './RescheduleControl';

interface AgendaItemRowProps {
  item: AgendaItemRecord;
  /** dnd-kit activator props for the grip — spread ONLY on the handle so a
   *  tap on checkbox / title / skip / play / pause / calendar never starts
   *  a drag. Present on draggable rows (the SortableAgendaItemRow wrapper
   *  supplies them); parked rows in the Completed dump render without a
   *  grip and omit them. */
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  /** Render the grab handle? Defaults to true. HomePage passes
   *  `showHandle={false}` for parked rows and for living rows a drag may not
   *  grab (done/skipped occurrences, deleted routines, orphans). */
  showHandle?: boolean;
  /** The slot whose mutation is in flight for this row (HomePage owns the
   *  in-flight set). That control swaps its glyph for a spinner in the same
   *  box and goes `aria-busy`; every other mutating control on the row is
   *  disabled so a second click cannot race it. `null` (the default) is a
   *  quiet row. */
  pending?: 'check' | 'play' | 'calendar' | 'grip' | null;
  /** The server's civil today — the Play gate (start is today-only on the
   *  API) and the RescheduleControl's floor. */
  serverToday: string;
  /** Move the slot to another day (`YYYY-MM-DD`) — HomePage fires
   *  `POST /api/agenda/items/:id/reschedule` optimistically. */
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  /** Drop a task back into the Backlog (`{ backlog: true }`) — HomePage owns
   *  the optimistic paint + success notice. Never offered for occurrences. */
  onMoveToBacklog: (item: AgendaItemRecord) => void;
  /** Park a dated `PLANNED` task in the Planned warehouse (`{ unschedule: true
   *  }`, ADR 0010) — the status, the Board rank and the logs are untouched.
   *  Never offered for occurrences. */
  onUnschedule: (item: AgendaItemRecord) => void;
  /** Briefly ring the row after the success notice's "Show" scrolled to it. */
  highlighted?: boolean;
  onCompleteTask: (item: AgendaItemRecord) => void;
  onStartTask: (item: AgendaItemRecord) => void;
  /** Pause an in-progress task (ADR 0002 `/pause` → PLANNED). */
  onPauseTask: (item: AgendaItemRecord) => void;
  onOpenTask: (task: TaskRecord) => void;
  onCompleteOccurrence: (item: AgendaItemRecord) => void;
  onSkipOccurrence: (item: AgendaItemRecord) => void;
  onStartOccurrence: (item: AgendaItemRecord) => void;
  /** Pause an in-progress occurrence (ADR 0004 `/pause` → pending + clear
   *  chip ids). No today gate — a civil-day rollover can still be running. */
  onPauseOccurrence: (item: AgendaItemRecord) => void;
  onRenameOccurrence: (item: AgendaItemRecord) => void;
}

/** Shared icon-button: 24px circle in the mobile action pill, compact on desktop. */
const ICON_BTN =
  'flex h-6 w-6 items-center justify-center rounded-full text-muted-foreground hover:bg-background/80 hover:text-foreground transition-colors flex-shrink-0 sm:h-auto sm:w-auto sm:rounded-md sm:p-1.5 sm:hover:bg-muted';

const TITLE_TEXT =
  'block min-w-0 truncate text-sm font-medium leading-snug text-foreground';

function ColorMark({
  color,
  className,
}: {
  color: string;
  className?: string;
}) {
  const fill = color.trim();
  return (
    <span
      className={cn(!fill && 'bg-muted-foreground/40', className)}
      style={fill ? { backgroundColor: fill } : undefined}
      aria-hidden
    />
  );
}

/** Two-line below sm (title, then meta/actions), single row at sm+.
 *  Category color is a TaskCard-style left ribbon at every breakpoint. */
function AgendaRowShell({
  crossed,
  handle,
  check,
  color,
  title,
  meta,
  highlighted = false,
  domId,
}: {
  crossed: boolean;
  handle: ReactNode;
  check: ReactNode;
  color: string;
  title: ReactNode;
  meta: ReactNode;
  highlighted?: boolean;
  domId: string;
}) {
  return (
    <div
      id={domId}
      className={cn(
        'overflow-hidden rounded-xl border border-border/60 bg-background transition-opacity',
        crossed && 'opacity-60',
        highlighted && 'ring-2 ring-primary/40',
      )}
    >
      <div className="flex">
        <ColorMark color={color} className="w-1 shrink-0 self-stretch" />
        <div className="flex min-w-0 flex-1 items-center gap-1 px-2 pt-2 pb-1 sm:gap-2.5 sm:px-3 sm:py-2.5">
          {handle}
          {check}
          <div className="flex min-w-0 flex-1 flex-col ml-1 gap-0 sm:flex-row sm:items-center sm:gap-2.5">
            {title}
            {meta}
          </div>
        </div>
      </div>
    </div>
  );
}

/** Mobile: the meta line tucks up under the title (a small negative top
 *  margin) so the row reads as two tight lines instead of a tall card; the
 *  action pill still sets the line height. At sm+ it flattens into the row. */
function AgendaRowMeta({ children }: { children: ReactNode }) {
  return (
    <div className="flex items-center gap-1.5 -mt-1 sm:contents">
      {children}
    </div>
  );
}

function AgendaRowActions({ children }: { children: ReactNode }) {
  if (Children.toArray(children).length === 0) return null;
  return (
    <div className="ml-auto flex items-center rounded-full bg-muted/70 p-0.5 sm:contents">
      {children}
    </div>
  );
}

function MinutesMark({ minutes }: { minutes: number }) {
  return (
    <span className="flex-shrink-0 text-[11px] tabular-nums text-muted-foreground">
      {minutes} min
    </span>
  );
}

/** The computed category name, muted — the left ribbon carries its color, so
 *  the text stays quiet. The `untracked` sink is hidden everywhere else in the
 *  app (Board pills, Routines), so it is hidden here too. Long names truncate
 *  before the row's action pill ever gets squeezed. */
function CategoryMark({ category }: { category: TaskCategorySummary }) {
  if (!category.title || category.is_untracked) return null;
  return (
    <span
      className="min-w-0 flex-shrink truncate text-[11px] text-muted-foreground"
      title={category.title}
    >
      {category.title}
    </span>
  );
}

function PriorityMark({ priority }: { priority: TaskPriority }) {
  if (priority === 'low') return null;
  return (
    <span
      className={cn(
        'flex-shrink-0 rounded-full px-1.5 py-0.5 text-[9px] font-semibold tracking-wide',
        priority === 'high'
          ? 'bg-red-400/15 text-red-500'
          : 'bg-amber-400/15 text-amber-600',
      )}
    >
      {TASK_PRIORITY_LABELS[priority]}
    </span>
  );
}

/** Round check circle — the row's done toggle (tasks and occurrences both
 *  cross off through it; clicking a checked circle unchecks — HomePage
 *  decides reopen vs complete/skip). Visual stays 20px; padding is hit slop
 *  only (negative margin keeps the layout size). While this slot is saving
 *  (`pending`) the circle turns neutral and holds a spinner, so the row never
 *  jumps and the check/X never reads as settled. */
function CheckCircle({
  checked,
  label,
  onClick,
  pending = false,
  disabled = false,
}: {
  checked: boolean;
  label: string;
  onClick: () => void;
  /** This slot is the one in flight — spinner instead of the mark. */
  pending?: boolean;
  /** Some row action is in flight (this slot or another). */
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-busy={pending || undefined}
      aria-label={checked ? `Undo done — ${label}` : `Complete ${label}`}
      className="flex-shrink-0 p-2.5 -m-2.5 sm:p-0 sm:m-0"
    >
      <span
        className={cn(
          'h-5 w-5 rounded-full border-2 flex items-center justify-center transition-colors',
          pending
            ? 'border-input'
            : checked
              ? 'bg-primary border-primary text-primary-foreground'
              : 'border-input hover:border-primary',
        )}
      >
        {pending ? (
          <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />
        ) : (
          checked && <Check className="h-3 w-3" strokeWidth={3} />
        )}
      </span>
    </button>
  );
}

/** The abandoned twin of the check circle — a parked `DISCARDED` task's
 *  toggle. Same geometry and hit slop as `CheckCircle` so the column still
 *  lines up, but the mark is a red X on a faint rose disc: the row is
 *  finished, just not done. Clicking it reopens to PLANNED (HomePage owns
 *  that branch); it never completes the task again. Busy swaps the X for the
 *  same neutral spinner the check circle uses. */
function DiscardCircle({
  label,
  onClick,
  pending = false,
  disabled = false,
}: {
  label: string;
  onClick: () => void;
  pending?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-busy={pending || undefined}
      aria-label={`Undo discard — ${label}`}
      className="flex-shrink-0 p-2.5 -m-2.5 sm:p-0 sm:m-0"
    >
      <span
        className={cn(
          'h-5 w-5 rounded-full border-2 flex items-center justify-center transition-colors',
          pending
            ? 'border-input'
            : 'border-rose-500 bg-rose-500/15 text-rose-500',
        )}
      >
        {pending ? (
          <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />
        ) : (
          <X className="h-3 w-3 text-rose-500" strokeWidth={3} />
        )}
      </span>
    </button>
  );
}

/** The reorder grip — the FIRST control on a LIVING row (before the
 *  checkbox) and the only drag activator: the dnd-kit listeners live here
 *  and nowhere else, so row taps (checkbox, title, skip, play, pause,
 *  calendar) never start a drag. `touch-none` stops a grab from scrolling the
 *  page; the cursor flips to grabbing while the drag is live. Rows in the
 *  Completed dump and read-only history rows never render it
 *  (`showHandle={false}` in the rows below), so its props are optional here
 *  too. A busy row drops the listeners entirely — a drag must never start on
 *  top of an in-flight mutation — and the in-flight grip shows the spinner. */
function DragHandle({
  attributes,
  listeners,
  label,
  isDragging,
  pending = false,
  disabled = false,
}: {
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  label: string;
  isDragging: boolean;
  pending?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={`Reorder ${label}`}
      title="Reorder"
      disabled={disabled}
      aria-busy={pending || undefined}
      className={cn(
        'flex-shrink-0 touch-none rounded-md p-1 -ml-0.5 text-muted-foreground/50 hover:bg-muted hover:text-muted-foreground sm:ml-0 sm:p-1.5',
        isDragging ? 'cursor-grabbing' : 'cursor-grab',
      )}
      {...(!disabled && attributes ? attributes : {})}
      {...(!disabled && listeners ? listeners : {})}
    >
      {pending ? (
        <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
      ) : (
        <GripVertical className="h-3.5 w-3.5 text-muted-foreground/50" />
      )}
    </button>
  );
}

/** Shared Play ↔ Pause swap for both row kinds. Play when `canStart`;
 *  Pause when `running`; nothing otherwise — except while this slot saves:
 *  then the button stays in its box as a disabled spinner (deriving a
 *  `Saving …` label when the optimistic status already hid the glyph). */
function PlayPauseControl({
  label,
  canStart,
  running,
  onStart,
  onPause,
  pending = false,
  disabled = false,
}: {
  label: string;
  canStart: boolean;
  running: boolean;
  onStart: () => void;
  onPause: () => void;
  pending?: boolean;
  disabled?: boolean;
}) {
  if (pending) {
    return (
      <button
        type="button"
        disabled
        aria-busy="true"
        aria-label={
          running
            ? `Pause ${label}`
            : canStart
              ? `Start ${label}`
              : `Saving ${label}`
        }
        title={running ? 'Pause' : canStart ? 'Start' : 'Saving'}
        className={ICON_BTN}
      >
        <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
      </button>
    );
  }
  if (running) {
    return (
      <button
        type="button"
        onClick={onPause}
        disabled={disabled}
        aria-label={`Pause ${label}`}
        title="Pause"
        className={ICON_BTN}
      >
        <Pause className="h-3.5 w-3.5" />
      </button>
    );
  }
  if (canStart) {
    return (
      <button
        type="button"
        onClick={onStart}
        disabled={disabled}
        aria-label={`Start ${label}`}
        title="Start"
        className={ICON_BTN}
      >
        <Play className="h-3.5 w-3.5" />
      </button>
    );
  }
  return null;
}

/** Status chip: done/skipped/in_progress for occurrences, done/in_progress
 *  for tasks — the same friendly labels the board uses. */
function StatusChip({
  label,
  tone,
}: {
  label: string;
  tone: 'done' | 'skipped' | 'running';
}) {
  return (
    <span
      className={cn(
        'flex-shrink-0 rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide',
        tone === 'done' && 'bg-emerald-100 text-emerald-700',
        tone === 'skipped' && 'bg-muted text-muted-foreground',
        tone === 'running' && 'bg-sky-100 text-sky-700',
      )}
    >
      {label}
    </span>
  );
}

function OccurrenceRow({
  item,
  occurrence,
  attributes,
  listeners,
  isDragging,
  showHandle,
  pending,
  serverToday,
  onReschedule,
  highlighted,
  domId,
  onComplete,
  onSkip,
  onStart,
  onPause,
  onRename,
}: {
  item: AgendaItemRecord;
  occurrence: OccurrenceRecord;
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  showHandle: boolean;
  pending: 'check' | 'play' | 'calendar' | 'grip' | null;
  serverToday: string;
  onReschedule: AgendaItemRowProps['onReschedule'];
  highlighted: boolean;
  domId: string;
  onComplete: () => void;
  onSkip: () => void;
  onStart: () => void;
  onPause: () => void;
  onRename: () => void;
}) {
  const crossed =
    occurrence.status === 'done' || occurrence.status === 'skipped';
  // A soft-deleted routine's row is history: no verbs, no grip, no rename.
  const readOnly = occurrence.routine_deleted;
  // Any in-flight row action quiets the controls a second click could reach.
  const busy = pending !== null;
  // Start is today-only on the API (it opens a one-shot Google log "now"), so
  // Play lives on the row whose civil day IS the server's today.
  const canStart =
    occurrence.status === 'pending' && item.local_date === serverToday;
  return (
    <AgendaRowShell
      crossed={crossed}
      color={occurrence.category.color}
      highlighted={highlighted}
      domId={domId}
      handle={
        !readOnly && (showHandle || pending === 'grip') ? (
          <DragHandle
            attributes={attributes}
            listeners={listeners}
            label={occurrence.resolved_title}
            isDragging={isDragging}
            pending={pending === 'grip'}
            disabled={busy}
          />
        ) : null
      }
      check={
        readOnly ? null : (
          <CheckCircle
            checked={occurrence.status === 'done'}
            label={occurrence.resolved_title}
            onClick={onComplete}
            pending={pending === 'check'}
            disabled={busy}
          />
        )
      }
      title={
        readOnly ? (
          <span className={cn(TITLE_TEXT, crossed && 'line-through')}>
            {occurrence.resolved_title}
          </span>
        ) : (
          <button
            type="button"
            onClick={onRename}
            aria-label={`Rename ${occurrence.resolved_title}`}
            className="group flex min-w-0 items-center gap-1 text-left sm:flex-1"
          >
            <span className={cn(TITLE_TEXT, crossed && 'line-through')}>
              {occurrence.resolved_title}
            </span>
            <Pencil className="h-3 w-3 flex-shrink-0 text-muted-foreground/50 group-hover:text-muted-foreground [@media(hover:none)]:text-muted-foreground" />
          </button>
        )
      }
      meta={
        <AgendaRowMeta>
          <CategoryMark category={occurrence.category} />
          <MinutesMark minutes={occurrence.estimated_minutes} />
          {occurrence.status !== 'pending' && (
            <StatusChip
              label={
                occurrence.status === 'done'
                  ? 'Done'
                  : occurrence.status === 'skipped'
                    ? 'Skipped'
                    : 'In progress'
              }
              tone={
                occurrence.status === 'done'
                  ? 'done'
                  : occurrence.status === 'skipped'
                    ? 'skipped'
                    : 'running'
              }
            />
          )}
          {readOnly && (
            <span className="flex-shrink-0 text-[11px] italic text-muted-foreground/70">
              routine deleted
            </span>
          )}
          {!readOnly && (
            <AgendaRowActions>
              <PlayPauseControl
                label={occurrence.resolved_title}
                canStart={canStart}
                running={occurrence.status === 'in_progress'}
                onStart={onStart}
                onPause={onPause}
                pending={pending === 'play'}
                disabled={busy}
              />
              {occurrence.status !== 'done' && (
                <button
                  type="button"
                  onClick={onSkip}
                  disabled={busy}
                  aria-label={
                    occurrence.status === 'skipped'
                      ? `Undo skip ${occurrence.resolved_title}`
                      : `Skip ${occurrence.resolved_title}`
                  }
                  title={occurrence.status === 'skipped' ? 'Undo skip' : 'Skip'}
                  className={ICON_BTN}
                >
                  <CalendarX2 className="h-3.5 w-3.5" />
                </button>
              )}
              {(canReschedule(item) || pending === 'calendar') && (
                <RescheduleControl
                  item={item}
                  label={occurrence.resolved_title}
                  serverToday={serverToday}
                  triggerClassName={ICON_BTN}
                  busy={pending === 'calendar'}
                  disabled={busy && pending !== 'calendar'}
                  onReschedule={onReschedule}
                />
              )}
            </AgendaRowActions>
          )}
        </AgendaRowMeta>
      }
    />
  );
}

function TaskRow({
  item,
  task,
  attributes,
  listeners,
  isDragging,
  showHandle,
  pending,
  serverToday,
  onReschedule,
  onMoveToBacklog,
  onUnschedule,
  highlighted,
  domId,
  onComplete,
  onStart,
  onPause,
  onOpen,
}: {
  item: AgendaItemRecord;
  task: TaskRecord;
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  showHandle: boolean;
  pending: 'check' | 'play' | 'calendar' | 'grip' | null;
  serverToday: string;
  onReschedule: AgendaItemRowProps['onReschedule'];
  onMoveToBacklog: AgendaItemRowProps['onMoveToBacklog'];
  onUnschedule: AgendaItemRowProps['onUnschedule'];
  highlighted: boolean;
  domId: string;
  onComplete: () => void;
  onStart: () => void;
  onPause: () => void;
  onOpen: () => void;
}) {
  // Both terminal statuses read as finished: the row dims and the title is
  // struck through. Only COMPLETED wears the check; DISCARDED wears the X.
  const completed = task.status === 'COMPLETED';
  const discarded = task.status === 'DISCARDED';
  const crossed = completed || discarded;
  // Any in-flight row action quiets the controls a second click could reach.
  const busy = pending !== null;
  // Start is today-only on the API (it opens the running Google event "now").
  const canStart =
    (task.status === 'OPEN' || task.status === 'PLANNED') &&
    item.local_date === serverToday;
  return (
    <AgendaRowShell
      crossed={crossed}
      color={task.category.color}
      highlighted={highlighted}
      domId={domId}
      handle={
        showHandle || pending === 'grip' ? (
          <DragHandle
            attributes={attributes}
            listeners={listeners}
            label={task.display_title}
            isDragging={isDragging}
            pending={pending === 'grip'}
            disabled={busy}
          />
        ) : null
      }
      check={
        discarded ? (
          <DiscardCircle
            label={task.display_title}
            onClick={onComplete}
            pending={pending === 'check'}
            disabled={busy}
          />
        ) : (
          <CheckCircle
            checked={completed}
            label={task.display_title}
            onClick={onComplete}
            pending={pending === 'check'}
            disabled={busy}
          />
        )
      }
      title={
        <button
          type="button"
          onClick={onOpen}
          aria-label={`Edit ${task.display_title}`}
          className="min-w-0 text-left sm:flex-1"
        >
          <span className={cn(TITLE_TEXT, crossed && 'line-through')}>
            {task.display_title}
          </span>
        </button>
      }
      meta={
        <AgendaRowMeta>
          <CategoryMark category={task.category} />
          <PriorityMark priority={task.priority} />
          <MinutesMark minutes={task.duration_minutes} />
          {task.status === 'COMPLETED' && (
            <StatusChip label="Done" tone="done" />
          )}
          {task.status === 'IN_PROGRESS' && (
            <StatusChip label="In progress" tone="running" />
          )}
          <AgendaRowActions>
            <PlayPauseControl
              label={task.display_title}
              canStart={canStart}
              running={task.status === 'IN_PROGRESS'}
              onStart={onStart}
              onPause={onPause}
              pending={pending === 'play'}
              disabled={busy}
            />
            {(canReschedule(item) || pending === 'calendar') && (
              <RescheduleControl
                item={item}
                label={task.display_title}
                serverToday={serverToday}
                triggerClassName={ICON_BTN}
                busy={pending === 'calendar'}
                disabled={busy && pending !== 'calendar'}
                onReschedule={onReschedule}
                onMoveToBacklog={() => onMoveToBacklog(item)}
                onUnschedule={() => onUnschedule(item)}
              />
            )}
          </AgendaRowActions>
        </AgendaRowMeta>
      }
    />
  );
}

export function AgendaItemRow(props: AgendaItemRowProps) {
  const { item } = props;
  const showHandle = props.showHandle ?? true;
  const highlighted = props.highlighted ?? false;
  const pending = props.pending ?? null;
  const domId = `agenda-item-${item.id}`;
  if (item.kind === 'occurrence' && item.occurrence) {
    return (
      <OccurrenceRow
        item={item}
        occurrence={item.occurrence}
        attributes={props.attributes}
        listeners={props.listeners}
        isDragging={props.isDragging}
        showHandle={showHandle}
        pending={pending}
        serverToday={props.serverToday}
        onReschedule={props.onReschedule}
        highlighted={highlighted}
        domId={domId}
        onComplete={() => props.onCompleteOccurrence(item)}
        onSkip={() => props.onSkipOccurrence(item)}
        onStart={() => props.onStartOccurrence(item)}
        onPause={() => props.onPauseOccurrence(item)}
        onRename={() => props.onRenameOccurrence(item)}
      />
    );
  }
  if (item.kind === 'task' && item.task) {
    return (
      <TaskRow
        item={item}
        task={item.task}
        attributes={props.attributes}
        listeners={props.listeners}
        isDragging={props.isDragging}
        showHandle={showHandle}
        pending={pending}
        serverToday={props.serverToday}
        onReschedule={props.onReschedule}
        onMoveToBacklog={props.onMoveToBacklog}
        onUnschedule={props.onUnschedule}
        highlighted={highlighted}
        domId={domId}
        onComplete={() => props.onCompleteTask(item)}
        onStart={() => props.onStartTask(item)}
        onPause={() => props.onPauseTask(item)}
        onOpen={() => props.onOpenTask(item.task!)}
      />
    );
  }
  // The API never returns a member row without its embed (orphans are
  // omitted); a defensive null keeps a malformed row from crashing the list.
  return null;
}

/** The draggable wrapper of an agenda row (same idea as the board's
 *  SortableTaskCard): applies the sortable transform + transition on an
 *  outer div while the row itself stays the plain presentational card. While
 *  dragging, the source row dims — HomePage's DragOverlay shows the floating
 *  title. The activator props are handed to the row, which spreads them on
 *  the grip ONLY, so the rest of the row never starts a drag. */
export function SortableAgendaItemRow(
  props: Omit<AgendaItemRowProps, 'attributes' | 'listeners' | 'isDragging'>,
) {
  const { item } = props;
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: item.id });

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className={cn(isDragging && 'opacity-40')}
    >
      <AgendaItemRow
        {...props}
        pending={props.pending ?? null}
        attributes={attributes}
        listeners={listeners}
        isDragging={isDragging}
      />
    </div>
  );
}

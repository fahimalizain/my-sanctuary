// One agenda row for Home (ADR 0004 § Surfaces — Home): the occurrence
// variant (checkbox toggles done ↔ pending, status chip, skip toggles
// skipped ↔ pending hidden while done, start, pause while in progress —
// ADR 0004 `/pause` lands pending + clears chip ids — rename, reschedule
// popover) and the
// task variant (checkbox toggles completed ↔ planned, start, pause while
// in progress — ADR 0002 `/pause` lands PLANNED — remove-from-day,
// reschedule popover, tap → TaskModal). Living rows lead with a grab handle
// (ADR 0004 amendment — reorder is a handle, chevrons are gone): the grip is
// the ONLY drag activator, so a tap on checkbox / title / skip / play /
// pause / calendar / unpin never starts a drag. Parked rows (the Completed dump —
// ADR 0004 amendment) render the same card WITHOUT the grip: the dump is not
// a drop target, so `showHandle={false}` omits the DragHandle and HomePage
// renders the plain component, never the sortable wrapper. Pure
// presentational — every mutation lives in HomePage; HomePage owns the verb
// branch (reopen vs complete/skip).

import { Children, useState, type ReactNode } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type {
  DraggableAttributes,
  DraggableSyntheticListeners,
} from '@dnd-kit/core';
import {
  Calendar,
  CalendarClock,
  CalendarX2,
  Check,
  GripVertical,
  Pause,
  Pencil,
  Play,
  X,
} from 'lucide-react';
import type {
  AgendaItemRecord,
  OccurrenceRecord,
  TaskPriority,
  TaskRecord,
} from '../../types';
import { TASK_PRIORITY_LABELS } from '../../types';
import { cn } from '@/lib/utils';
import { canReschedule } from './agenda-helpers';
import { addCivilDays } from '../routines/rrule-preview';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover';

interface AgendaItemRowProps {
  item: AgendaItemRecord;
  /** dnd-kit activator props for the grip — spread ONLY on the handle so a
   *  tap on checkbox / title / skip / play / pause / calendar / unpin never starts
   *  a drag. Present on living rows (the SortableAgendaItemRow wrapper
   *  supplies them); parked rows in the Completed dump render without a
   *  grip and omit them. */
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  /** Render the grab handle? Defaults to true. HomePage passes
   *  `showHandle={false}` for parked rows — the Completed dump is not
   *  sortable, so there is nothing to grab. */
  showHandle?: boolean;
  /** Move the slot to another day (`YYYY-MM-DD`) — HomePage fires
   *  `POST /api/agenda/items/:id/reschedule` optimistically. */
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  onCompleteTask: (item: AgendaItemRecord) => void;
  onStartTask: (item: AgendaItemRecord) => void;
  /** Pause an in-progress task (ADR 0002 `/pause` → PLANNED). */
  onPauseTask: (item: AgendaItemRecord) => void;
  onRemoveTask: (item: AgendaItemRecord) => void;
  onOpenTask: (task: TaskRecord) => void;
  onCompleteOccurrence: (item: AgendaItemRecord) => void;
  onSkipOccurrence: (item: AgendaItemRecord) => void;
  onStartOccurrence: (item: AgendaItemRecord) => void;
  /** Pause an in-progress occurrence (ADR 0004 `/pause` → pending + clear
   *  chip ids). No today gate — a civil-day rollover can still be running. */
  onPauseOccurrence: (item: AgendaItemRecord) => void;
  onRenameOccurrence: (occurrence: OccurrenceRecord) => void;
  /** Start is today-only (ADR 0004): Home renders Play for pending
   *  occurrences only when the selected date is the civil today. */
  showStartOccurrence?: boolean;
}

/** Shared icon-button: 28px circle in the mobile action pill, compact on desktop. */
const ICON_BTN =
  'flex h-7 w-7 items-center justify-center rounded-full text-muted-foreground hover:bg-background/80 hover:text-foreground transition-colors flex-shrink-0 sm:h-auto sm:w-auto sm:rounded-md sm:p-1.5 sm:hover:bg-muted';

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
}: {
  crossed: boolean;
  handle: ReactNode;
  check: ReactNode;
  color: string;
  title: ReactNode;
  meta: ReactNode;
}) {
  return (
    <div
      className={cn(
        'overflow-hidden rounded-xl border border-border/60 bg-background transition-opacity',
        crossed && 'opacity-60',
      )}
    >
      <div className="flex">
        <ColorMark color={color} className="w-1 shrink-0 self-stretch" />
        <div className="flex min-w-0 flex-1 items-center gap-1 px-2 py-1.5 sm:gap-2.5 sm:px-3 sm:py-2.5">
          {handle}
          {check}
          <div className="flex min-w-0 flex-1 flex-col ml-1 gap-0.5 sm:flex-row sm:items-center sm:gap-2.5">
            {title}
            {meta}
          </div>
        </div>
      </div>
    </div>
  );
}

function AgendaRowMeta({ children }: { children: ReactNode }) {
  return (
    <div className="flex items-center gap-1.5 sm:contents">{children}</div>
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
 *  only (negative margin keeps the layout size). */
function CheckCircle({
  checked,
  label,
  onClick,
}: {
  checked: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={checked ? `Undo done — ${label}` : `Complete ${label}`}
      className="flex-shrink-0 p-2.5 -m-2.5 sm:p-0 sm:m-0"
    >
      <span
        className={cn(
          'h-5 w-5 rounded-full border-2 flex items-center justify-center transition-colors',
          checked
            ? 'bg-primary border-primary text-primary-foreground'
            : 'border-input hover:border-primary',
        )}
      >
        {checked && <Check className="h-3 w-3" strokeWidth={3} />}
      </span>
    </button>
  );
}

/** The reorder grip — the FIRST control on a LIVING row (before the
 *  checkbox) and the only drag activator: the dnd-kit listeners live here
 *  and nowhere else, so row taps (checkbox, title, skip, play, pause,
 *  calendar, unpin) never start a drag. `touch-none` stops a grab from scrolling the
 *  page; the cursor flips to grabbing while the drag is live. Rows in the
 *  Completed dump never render it (`showHandle={false}` in the rows below),
 *  so its props are optional here too. */
function DragHandle({
  attributes,
  listeners,
  label,
  isDragging,
}: {
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  label: string;
  isDragging: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={`Reorder ${label}`}
      title="Reorder"
      className={cn(
        'flex-shrink-0 touch-none rounded-md p-1 -ml-0.5 text-muted-foreground/50 hover:bg-muted hover:text-muted-foreground sm:ml-0 sm:p-1.5',
        isDragging ? 'cursor-grabbing' : 'cursor-grab',
      )}
      {...attributes}
      {...listeners}
    >
      <GripVertical className="h-3.5 w-3.5 text-muted-foreground/50" />
    </button>
  );
}

/** Move-to-another-day control (ADR 0004 amendment — reschedule): a
 *  calendar icon opens a small popover with a primary "Tomorrow" action
 *  plus a compact native date picker for any other day. Both fire
 *  immediately — no confirm — so the row's optimistic drop in HomePage is
 *  instant; failures revert + banner. "Tomorrow" is the ROW's own date +1
 *  civil day (moving from a future day still means "the next day"), never
 *  the browser's today. The popover closes and the picker resets after
 *  either action so it never reads as a filter. Skip stays its own control
 *  (ADR 0004 amendment — the calendar popover is Tomorrow + pick-a-day
 *  only). */
function RescheduleControl({
  item,
  label,
  onReschedule,
}: {
  item: AgendaItemRecord;
  label: string;
  onReschedule: AgendaItemRowProps['onReschedule'];
}) {
  const [open, setOpen] = useState(false);
  const [pickDate, setPickDate] = useState('');
  const tomorrow = addCivilDays(item.local_date, 1);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        aria-label={`Move ${label} to another day`}
        title={`Move ${label} to another day`}
        className={ICON_BTN}
      >
        <Calendar className="h-3.5 w-3.5" />
      </PopoverTrigger>
      <PopoverContent className="w-[min(12rem,calc(100vw-2rem))] flex flex-col gap-1">
        <button
          type="button"
          onClick={() => {
            setOpen(false);
            onReschedule(item, tomorrow);
          }}
          aria-label={`Move ${label} to tomorrow`}
          title="Move to tomorrow"
          className="inline-flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-xs text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
        >
          <CalendarClock className="h-3.5 w-3.5" />
          Tomorrow
        </button>
        <input
          type="date"
          value={pickDate}
          onChange={(e) => {
            const next = e.target.value;
            // No confirm: picking a day reschedules immediately, then the
            // control resets so it never reads as a filter.
            if (next) {
              setPickDate('');
              setOpen(false);
              onReschedule(item, next);
            }
          }}
          aria-label={`Move ${label} to another day`}
          title="Move to another day"
          className="w-full rounded-md border border-input bg-background px-1 py-0.5 text-xs text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
        />
      </PopoverContent>
    </Popover>
  );
}

/** Shared Play ↔ Pause swap for both row kinds. Play when `canStart`;
 *  Pause when `running`; nothing otherwise. */
function PlayPauseControl({
  label,
  canStart,
  running,
  onStart,
  onPause,
}: {
  label: string;
  canStart: boolean;
  running: boolean;
  onStart: () => void;
  onPause: () => void;
}) {
  if (running) {
    return (
      <button
        type="button"
        onClick={onPause}
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
  onReschedule,
  onComplete,
  onSkip,
  onStart,
  onPause,
  showStart,
  onRename,
}: {
  item: AgendaItemRecord;
  occurrence: OccurrenceRecord;
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  showHandle: boolean;
  onReschedule: AgendaItemRowProps['onReschedule'];
  onComplete: () => void;
  onSkip: () => void;
  onStart: () => void;
  onPause: () => void;
  showStart: boolean;
  onRename: () => void;
}) {
  const crossed =
    occurrence.status === 'done' || occurrence.status === 'skipped';
  return (
    <AgendaRowShell
      crossed={crossed}
      color={occurrence.category.color}
      handle={
        showHandle ? (
          <DragHandle
            attributes={attributes}
            listeners={listeners}
            label={occurrence.resolved_title}
            isDragging={isDragging}
          />
        ) : null
      }
      check={
        <CheckCircle
          checked={occurrence.status === 'done'}
          label={occurrence.resolved_title}
          onClick={onComplete}
        />
      }
      title={
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
      }
      meta={
        <AgendaRowMeta>
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
          <AgendaRowActions>
            <PlayPauseControl
              label={occurrence.resolved_title}
              canStart={occurrence.status === 'pending' && showStart}
              running={occurrence.status === 'in_progress'}
              onStart={onStart}
              onPause={onPause}
            />
            {occurrence.status !== 'done' && (
              <button
                type="button"
                onClick={onSkip}
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
            {canReschedule(item) && (
              <RescheduleControl
                item={item}
                label={occurrence.resolved_title}
                onReschedule={onReschedule}
              />
            )}
          </AgendaRowActions>
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
  onReschedule,
  onComplete,
  onStart,
  onPause,
  onRemove,
  onOpen,
}: {
  item: AgendaItemRecord;
  task: TaskRecord;
  attributes?: DraggableAttributes;
  listeners?: DraggableSyntheticListeners;
  isDragging: boolean;
  showHandle: boolean;
  onReschedule: AgendaItemRowProps['onReschedule'];
  onComplete: () => void;
  onStart: () => void;
  onPause: () => void;
  onRemove: () => void;
  onOpen: () => void;
}) {
  const crossed = task.status === 'COMPLETED';
  return (
    <AgendaRowShell
      crossed={crossed}
      color={task.category.color}
      handle={
        showHandle ? (
          <DragHandle
            attributes={attributes}
            listeners={listeners}
            label={task.display_title}
            isDragging={isDragging}
          />
        ) : null
      }
      check={
        <CheckCircle
          checked={crossed}
          label={task.display_title}
          onClick={onComplete}
        />
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
              canStart={task.status === 'OPEN' || task.status === 'PLANNED'}
              running={task.status === 'IN_PROGRESS'}
              onStart={onStart}
              onPause={onPause}
            />
            <button
              type="button"
              onClick={onRemove}
              aria-label={`Remove ${task.display_title} from this day`}
              title="Remove from this day"
              className={cn(
                ICON_BTN,
                'hover:bg-destructive/10 hover:text-destructive',
              )}
            >
              <X className="h-3.5 w-3.5" />
            </button>
            {canReschedule(item) && (
              <RescheduleControl
                item={item}
                label={task.display_title}
                onReschedule={onReschedule}
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
  if (item.kind === 'occurrence' && item.occurrence) {
    return (
      <OccurrenceRow
        item={item}
        occurrence={item.occurrence}
        attributes={props.attributes}
        listeners={props.listeners}
        isDragging={props.isDragging}
        showHandle={showHandle}
        onReschedule={props.onReschedule}
        onComplete={() => props.onCompleteOccurrence(item)}
        onSkip={() => props.onSkipOccurrence(item)}
        onStart={() => props.onStartOccurrence(item)}
        onPause={() => props.onPauseOccurrence(item)}
        showStart={props.showStartOccurrence ?? false}
        onRename={() => props.onRenameOccurrence(item.occurrence!)}
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
        onReschedule={props.onReschedule}
        onComplete={() => props.onCompleteTask(item)}
        onStart={() => props.onStartTask(item)}
        onPause={() => props.onPauseTask(item)}
        onRemove={() => props.onRemoveTask(item)}
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
        attributes={attributes}
        listeners={listeners}
        isDragging={isDragging}
      />
    </div>
  );
}

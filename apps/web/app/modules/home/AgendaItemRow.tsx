// One agenda row for Home (ADR 0004 § Surfaces — Home): the occurrence
// variant (checkbox toggles done ↔ pending, status chip, skip toggles
// skipped ↔ pending hidden while done, rename, move-to-another-day) and the
// task variant (checkbox toggles completed ↔ planned, start, remove-from-day,
// move-to-another-day, tap → TaskModal). Pure presentational — every
// mutation lives in HomePage; HomePage owns the verb branch (reopen vs
// complete/skip).

import { useState } from 'react';
import {
  Calendar,
  CalendarClock,
  CalendarX2,
  Check,
  ChevronDown,
  ChevronUp,
  Pencil,
  Play,
  X,
} from 'lucide-react';
import type {
  AgendaItemRecord,
  OccurrenceRecord,
  TaskRecord,
} from '../../types';
import { TASK_PRIORITY_LABELS } from '../../types';
import { cn } from '@/lib/utils';
import { canReschedule } from './agenda-helpers';
import { addCivilDays } from '../routines/rrule-preview';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';

interface AgendaItemRowProps {
  item: AgendaItemRecord;
  isFirst: boolean;
  isLast: boolean;
  onMove: (itemId: string, direction: 'up' | 'down') => void;
  /** Move the slot to another day (`YYYY-MM-DD`) — HomePage fires
   *  `POST /api/agenda/items/:id/reschedule` optimistically. */
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  onCompleteTask: (item: AgendaItemRecord) => void;
  onStartTask: (item: AgendaItemRecord) => void;
  onRemoveTask: (item: AgendaItemRecord) => void;
  onOpenTask: (task: TaskRecord) => void;
  onCompleteOccurrence: (item: AgendaItemRecord) => void;
  onSkipOccurrence: (item: AgendaItemRecord) => void;
  onStartOccurrence: (item: AgendaItemRecord) => void;
  onRenameOccurrence: (occurrence: OccurrenceRecord) => void;
  /** Start is today-only (ADR 0004): Home renders Play for pending
   *  occurrences only when the selected date is the civil today. */
  showStartOccurrence?: boolean;
}

/** Round check circle — the row's done toggle (tasks and occurrences both
 *  cross off through it; clicking a checked circle unchecks — HomePage
 *  decides reopen vs complete/skip). */
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
      className={cn(
        'h-5 w-5 rounded-full border-2 flex items-center justify-center flex-shrink-0 transition-colors',
        checked
          ? 'bg-primary border-primary text-primary-foreground'
          : 'border-input hover:border-primary',
      )}
    >
      {checked && <Check className="h-3 w-3" strokeWidth={3} />}
    </button>
  );
}

/** Up/down reorder stack — disabled at the pile's edges. */
function MoveButtons({
  isFirst,
  isLast,
  onMove,
  itemId,
}: {
  isFirst: boolean;
  isLast: boolean;
  onMove: (itemId: string, direction: 'up' | 'down') => void;
  itemId: string;
}) {
  return (
    <div className="flex flex-col flex-shrink-0">
      <button
        type="button"
        disabled={isFirst}
        onClick={() => onMove(itemId, 'up')}
        aria-label="Move up"
        title="Move up"
        className="p-0.5 rounded hover:bg-muted transition-colors disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-transparent"
      >
        <ChevronUp className="h-3.5 w-3.5 text-muted-foreground" />
      </button>
      <button
        type="button"
        disabled={isLast}
        onClick={() => onMove(itemId, 'down')}
        aria-label="Move down"
        title="Move down"
        className="p-0.5 rounded hover:bg-muted transition-colors disabled:opacity-25 disabled:cursor-not-allowed disabled:hover:bg-transparent"
      >
        <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
      </button>
    </div>
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
        className="p-1.5 rounded-md hover:bg-muted transition-colors flex-shrink-0"
      >
        <Calendar className="h-3.5 w-3.5 text-muted-foreground" />
      </PopoverTrigger>
      <PopoverContent className="w-48 flex flex-col gap-1">
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
  isFirst,
  isLast,
  onMove,
  onReschedule,
  onComplete,
  onSkip,
  onStart,
  showStart,
  onRename,
}: {
  item: AgendaItemRecord;
  occurrence: OccurrenceRecord;
  isFirst: boolean;
  isLast: boolean;
  onMove: AgendaItemRowProps['onMove'];
  onReschedule: AgendaItemRowProps['onReschedule'];
  onComplete: () => void;
  onSkip: () => void;
  onStart: () => void;
  showStart: boolean;
  onRename: () => void;
}) {
  const crossed =
    occurrence.status === 'done' || occurrence.status === 'skipped';
  return (
    <div
      className={cn(
        'rounded-lg border border-border bg-background p-3 transition-opacity',
        crossed && 'opacity-60',
      )}
    >
      <div className="flex items-center gap-2.5">
        <CheckCircle
          checked={occurrence.status === 'done'}
          label={occurrence.resolved_title}
          onClick={onComplete}
        />
        <span
          className="h-2.5 w-2.5 rounded-full flex-shrink-0"
          style={{ backgroundColor: occurrence.category.color }}
        />
        {/* Tap the title (or the pencil) to rename — a day-level override,
            never the routine definition */}
        <button
          type="button"
          onClick={onRename}
          aria-label={`Rename ${occurrence.resolved_title}`}
          className="group flex-1 min-w-0 flex items-center gap-1 text-left"
        >
          <span
            className={cn(
              'block truncate text-sm font-medium text-foreground',
              crossed && 'line-through',
            )}
          >
            {occurrence.resolved_title}
          </span>
          <Pencil className="h-3 w-3 text-muted-foreground/50 group-hover:text-muted-foreground flex-shrink-0" />
        </button>
        <span className="flex-shrink-0 text-xs text-muted-foreground">
          {occurrence.estimated_minutes} min
        </span>
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
        {/* Skip toggles skipped ↔ pending; hidden only while done (a done
            row unchecks first — two taps: uncheck, then skip). */}
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
            className="p-1.5 rounded-md hover:bg-muted transition-colors flex-shrink-0"
          >
            <CalendarX2 className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
        )}
        {/* Start is pending-only AND today-only: a one-shot Google log opens
            and the chip reads In progress (a repeat on in_progress would be
            a server-side 200 no-op, but the button is hidden anyway). */}
        {occurrence.status === 'pending' && showStart && (
          <button
            type="button"
            onClick={onStart}
            aria-label={`Start ${occurrence.resolved_title}`}
            title="Start"
            className="p-1.5 rounded-md hover:bg-muted transition-colors flex-shrink-0"
          >
            <Play className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
        )}
        {/* Move-to-another-day — pending/skipped only (in_progress/done
            → API 400, so the control hides with the chip). */}
        {canReschedule(item) && (
          <RescheduleControl
            item={item}
            label={occurrence.resolved_title}
            onReschedule={onReschedule}
          />
        )}
        <MoveButtons
          isFirst={isFirst}
          isLast={isLast}
          onMove={onMove}
          itemId={item.id}
        />
      </div>
    </div>
  );
}

function TaskRow({
  item,
  task,
  isFirst,
  isLast,
  onMove,
  onReschedule,
  onComplete,
  onStart,
  onRemove,
  onOpen,
}: {
  item: AgendaItemRecord;
  task: TaskRecord;
  isFirst: boolean;
  isLast: boolean;
  onMove: AgendaItemRowProps['onMove'];
  onReschedule: AgendaItemRowProps['onReschedule'];
  onComplete: () => void;
  onStart: () => void;
  onRemove: () => void;
  onOpen: () => void;
}) {
  const crossed = task.status === 'COMPLETED';
  return (
    <div
      className={cn(
        'rounded-lg border border-border bg-background p-3 transition-opacity',
        crossed && 'opacity-60',
      )}
    >
      <div className="flex items-center gap-2.5">
        <CheckCircle
          checked={crossed}
          label={task.display_title}
          onClick={onComplete}
        />
        <span
          className="h-2.5 w-2.5 rounded-full flex-shrink-0"
          style={{ backgroundColor: task.category.color }}
        />
        {/* Tap the row to edit in the existing TaskModal */}
        <button
          type="button"
          onClick={onOpen}
          aria-label={`Edit ${task.display_title}`}
          className="flex-1 min-w-0 text-left"
        >
          <span
            className={cn(
              'block truncate text-sm font-medium text-foreground',
              crossed && 'line-through',
            )}
          >
            {task.display_title}
          </span>
        </button>
        <span className="flex-shrink-0 text-xs text-muted-foreground">
          {TASK_PRIORITY_LABELS[task.priority]} · {task.duration_minutes} min
        </span>
        {task.status === 'COMPLETED' && <StatusChip label="Done" tone="done" />}
        {task.status === 'IN_PROGRESS' && (
          <StatusChip label="In progress" tone="running" />
        )}
        {(task.status === 'OPEN' || task.status === 'PLANNED') && (
          <button
            type="button"
            onClick={onStart}
            aria-label={`Start ${task.display_title}`}
            title="Start"
            className="p-1.5 rounded-md hover:bg-muted transition-colors flex-shrink-0"
          >
            <Play className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
        )}
        <button
          type="button"
          onClick={onRemove}
          aria-label={`Remove ${task.display_title} from this day`}
          title="Remove from this day"
          className="p-1.5 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors flex-shrink-0"
        >
          <X className="h-3.5 w-3.5 text-muted-foreground" />
        </button>
        {/* Move-to-another-day — living tasks only (COMPLETED/DISCARDED
            cards stay put: Home never invites moving a finished card). */}
        {canReschedule(item) && (
          <RescheduleControl
            item={item}
            label={task.display_title}
            onReschedule={onReschedule}
          />
        )}
        <MoveButtons
          isFirst={isFirst}
          isLast={isLast}
          onMove={onMove}
          itemId={item.id}
        />
      </div>
    </div>
  );
}

export function AgendaItemRow(props: AgendaItemRowProps) {
  const { item } = props;
  if (item.kind === 'occurrence' && item.occurrence) {
    return (
      <OccurrenceRow
        item={item}
        occurrence={item.occurrence}
        isFirst={props.isFirst}
        isLast={props.isLast}
        onMove={props.onMove}
        onReschedule={props.onReschedule}
        onComplete={() => props.onCompleteOccurrence(item)}
        onSkip={() => props.onSkipOccurrence(item)}
        onStart={() => props.onStartOccurrence(item)}
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
        isFirst={props.isFirst}
        isLast={props.isLast}
        onMove={props.onMove}
        onReschedule={props.onReschedule}
        onComplete={() => props.onCompleteTask(item)}
        onStart={() => props.onStartTask(item)}
        onRemove={() => props.onRemoveTask(item)}
        onOpen={() => props.onOpenTask(item.task!)}
      />
    );
  }
  // The API never returns a member row without its embed (orphans are
  // omitted); a defensive null keeps a malformed row from crashing the list.
  return null;
}

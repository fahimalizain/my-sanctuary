// Move-to-day menu for one agenda row (ADR 0004 amendment — reschedule,
// ADR 0007 — bounds / backlog): the calendar icon opens a compact menu with
// presets anchored to the SERVER's civil today, a month picker, and — for
// tasks — the Backlog. Every choice fires immediately; HomePage owns the
// optimistic move, the failure banner and the success notice. Presentational
// only.
//
// The presets deliberately shift from `serverToday`, never from the row's own
// date. "Tomorrow" on a row three days out must move it to TOMORROW, not one
// day after the row; and a past row must never offer two buttons that both
// land on today. See `reschedulePresets` for the dedupe rules.

import { useEffect, useRef, useState, type RefObject } from 'react';
import {
  Archive,
  Calendar,
  CalendarClock,
  CalendarDays,
  ChevronLeft,
  ChevronRight,
} from 'lucide-react';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover';
import { cn } from '@/lib/utils';
import type { AgendaItemRecord } from '../../types';
import { WEEK_DAYS, monthGridDays } from '../calendar/lib/week-layout';
import { addCivilDays } from '../routines/rrule-preview';
import {
  agendaDateLabel,
  agendaShortDateLabel,
  canMoveToBacklog,
  rescheduleMaxDate,
  reschedulePickerMonth,
  reschedulePresets,
} from './agenda-helpers';

const MONTH_LABELS = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
] as const;

const MENU_ROW =
  'flex min-h-10 w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30';
const NAV_BTN =
  'flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30';

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

function civilDateOf(day: Date): string {
  return `${day.getFullYear()}-${pad(day.getMonth() + 1)}-${pad(day.getDate())}`;
}

function monthStartOf(date: string): Date {
  const [y, m] = date.split('-').map(Number);
  return new Date(y, m - 1, 1);
}

/** The month the picker opens on. A backlog row sits at today+50y (far outside
 *  the picker's 10-year ceiling) and a history row is in the past; opening
 *  either on the row's own month would strand the user forty years from a
 *  selectable day. */
function monthForPicker(itemDate: string, today: string): Date {
  const month = reschedulePickerMonth(itemDate, today);
  if (!month) return new Date(1970, 0, 1);
  return monthStartOf(`${month}-01`);
}

export interface RescheduleControlProps {
  item: AgendaItemRecord;
  label: string;
  serverToday: string;
  /** Shared row action styling (the 24px icon button). */
  triggerClassName: string;
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  /** Available for task rows only; omitted for occurrences. */
  onMoveToBacklog?: (item: AgendaItemRecord) => void;
}

export function RescheduleControl({
  item,
  label,
  serverToday,
  triggerClassName,
  onReschedule,
  onMoveToBacklog,
}: RescheduleControlProps) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<'menu' | 'calendar'>('menu');
  const [viewMonth, setViewMonth] = useState<Date>(() =>
    monthForPicker(item.local_date, serverToday),
  );
  const pickDateRef = useRef<HTMLButtonElement>(null);
  const backButtonRef = useRef<HTMLButtonElement>(null);
  const wasCalendarRef = useRef(false);

  // The menu and the calendar are two views of the same popover; swapping
  // them unmounts the focused button. Move focus with the view so keyboard
  // users are never dropped back on <body>.
  useEffect(() => {
    if (!open) return;
    if (view === 'calendar') {
      backButtonRef.current?.focus();
      wasCalendarRef.current = true;
      return;
    }
    if (wasCalendarRef.current) {
      pickDateRef.current?.focus();
      wasCalendarRef.current = false;
    }
  }, [open, view]);

  const presets = reschedulePresets(item, serverToday);
  const maxDate = serverToday ? rescheduleMaxDate(serverToday) : '';
  const currentAbsolute = agendaShortDateLabel(item.local_date, serverToday);
  const currentRelative = agendaDateLabel(item.local_date, serverToday);
  const baseCurrentLabel =
    currentRelative === currentAbsolute
      ? currentAbsolute
      : `${currentRelative} · ${currentAbsolute}`;
  // Anything at/after today+10y is the backlog sentinel (today+50y), even if a
  // self-heal has not pulled a PLANNED row back yet. The picker's max is
  // today+10y−1, so this is exactly the "outside the date range" test.
  const beyondDateRange =
    !!serverToday && !!maxDate && item.local_date > maxDate;
  const inDatedBacklog =
    !!serverToday &&
    !beyondDateRange &&
    item.local_date > addCivilDays(serverToday, 6);
  const currentLabel = beyondDateRange
    ? 'Backlog'
    : `${baseCurrentLabel}${inDatedBacklog ? ' · Backlog' : ''}`;
  const canBacklog = !!onMoveToBacklog && canMoveToBacklog(item, serverToday);
  const openTaskHint = item.task?.status === 'OPEN';

  const choose = (date: string) => {
    if (!date || date === item.local_date) return;
    setOpen(false);
    setView('menu');
    wasCalendarRef.current = false;
    onReschedule(item, date);
  };

  const openCalendar = () => {
    setViewMonth(monthForPicker(item.local_date, serverToday));
    setView('calendar');
  };

  const handleBacklog = () => {
    setOpen(false);
    setView('menu');
    wasCalendarRef.current = false;
    onMoveToBacklog?.(item);
  };

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) {
          setView('menu');
          wasCalendarRef.current = false;
        }
      }}
    >
      <PopoverTrigger
        aria-label={`Move ${label} to another day`}
        title={`Move ${label} to another day`}
        className={triggerClassName}
      >
        <Calendar className="h-3.5 w-3.5" />
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="max-h-[calc(100dvh-3rem)] w-[min(19rem,calc(100vw-2rem))] overflow-y-auto p-1.5"
      >
        {view === 'menu' ? (
          <div className="flex flex-col">
            <header className="px-1.5 pb-1.5 pt-0.5">
              <p className="text-[10px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">
                Move to
              </p>
              <p
                className="truncate text-sm font-semibold text-foreground"
                title={label}
              >
                {label}
              </p>
              <p
                className="truncate text-[11px] text-muted-foreground"
                title={`Currently ${currentLabel}`}
              >
                Currently: {currentLabel}
              </p>
            </header>

            {presets.map((preset) => {
              const dateLabel = agendaShortDateLabel(preset.date, serverToday);
              return (
                <button
                  key={preset.id}
                  type="button"
                  onClick={() => choose(preset.date)}
                  aria-label={`Move ${label} to ${preset.label}, ${dateLabel}`}
                  className={MENU_ROW}
                >
                  <CalendarClock className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">
                    {preset.label}
                  </span>
                  <span className="shrink-0 text-[11px] tabular-nums text-muted-foreground">
                    {dateLabel}
                  </span>
                </button>
              );
            })}

            <button
              ref={pickDateRef}
              type="button"
              onClick={openCalendar}
              aria-label={`Pick another day for ${label}`}
              className={MENU_ROW}
            >
              <CalendarDays className="h-4 w-4 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate">Pick a date…</span>
              <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
            </button>

            {openTaskHint && (
              <p className="px-1.5 pb-0.5 pt-1 text-[11px] leading-snug text-muted-foreground/80">
                Moving to a day plans this task.
              </p>
            )}

            {canBacklog && (
              <>
                <div className="my-1 h-px bg-border/70" />
                <button
                  type="button"
                  onClick={handleBacklog}
                  aria-label={`Move ${label} to the backlog`}
                  className={MENU_ROW}
                >
                  <Archive className="h-4 w-4 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate">Backlog</span>
                  <span className="shrink-0 text-[11px] text-muted-foreground">
                    No date
                  </span>
                </button>
              </>
            )}
          </div>
        ) : (
          <RescheduleCalendar
            label={label}
            currentDate={item.local_date}
            today={serverToday}
            min={serverToday}
            max={maxDate}
            viewMonth={viewMonth}
            onViewMonthChange={setViewMonth}
            onBack={() => setView('menu')}
            backButtonRef={backButtonRef}
            onSelect={choose}
          />
        )}
      </PopoverContent>
    </Popover>
  );
}

function RescheduleCalendar({
  label,
  currentDate,
  today,
  min,
  max,
  viewMonth,
  onViewMonthChange,
  onBack,
  backButtonRef,
  onSelect,
}: {
  label: string;
  currentDate: string;
  today: string;
  min: string;
  max: string;
  viewMonth: Date;
  onViewMonthChange: (month: Date) => void;
  onBack: () => void;
  backButtonRef: RefObject<HTMLButtonElement | null>;
  onSelect: (date: string) => void;
}) {
  const gridDays = monthGridDays(viewMonth);
  const monthLabel = `${MONTH_LABELS[viewMonth.getMonth()]} ${viewMonth.getFullYear()}`;
  const previousMonthEnd = civilDateOf(
    new Date(viewMonth.getFullYear(), viewMonth.getMonth(), 0),
  );
  const nextMonthStart = civilDateOf(
    new Date(viewMonth.getFullYear(), viewMonth.getMonth() + 1, 1),
  );
  const canGoPrevious = !!min && previousMonthEnd >= min;
  const canGoNext = !!max && nextMonthStart <= max;
  const shiftMonth = (delta: number) => {
    onViewMonthChange(
      new Date(viewMonth.getFullYear(), viewMonth.getMonth() + delta, 1),
    );
  };

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-1 px-0.5">
        <button
          ref={backButtonRef}
          type="button"
          onClick={onBack}
          aria-label="Back to quick options"
          className="rounded-md px-1.5 py-1 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30"
        >
          Back
        </button>
        <span className="min-w-0 flex-1 truncate text-center text-sm font-semibold text-foreground">
          {monthLabel}
        </span>
        <button
          type="button"
          onClick={() => shiftMonth(-1)}
          disabled={!canGoPrevious}
          aria-label="Previous month"
          className={NAV_BTN}
        >
          <ChevronLeft className="h-4 w-4" />
        </button>
        <button
          type="button"
          onClick={() => shiftMonth(1)}
          disabled={!canGoNext}
          aria-label="Next month"
          className={NAV_BTN}
        >
          <ChevronRight className="h-4 w-4" />
        </button>
      </div>

      <div className="grid grid-cols-7 px-0.5" aria-hidden>
        {WEEK_DAYS.map((day) => (
          <div
            key={day}
            className="flex h-6 items-center justify-center text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
          >
            {day.slice(0, 2)}
          </div>
        ))}
      </div>

      <div className="grid grid-cols-7 px-0.5">
        {gridDays.map((day) => {
          const date = civilDateOf(day);
          const inMonth =
            day.getMonth() === viewMonth.getMonth() &&
            day.getFullYear() === viewMonth.getFullYear();
          const isToday = date === today;
          const isCurrent = date === currentDate;
          const disabled =
            !min || !max || date < min || date > max || isCurrent;
          return (
            <button
              key={date}
              type="button"
              disabled={disabled}
              onClick={() => onSelect(date)}
              aria-label={`Move ${label} to ${agendaShortDateLabel(date, today)}`}
              aria-current={isToday ? 'date' : undefined}
              className={cn(
                'flex h-9 w-full items-center justify-center rounded-full text-sm tabular-nums transition-colors',
                !inMonth && 'text-muted-foreground/40',
                inMonth && !isToday && !isCurrent && 'text-foreground',
                isCurrent &&
                  'font-semibold text-primary ring-2 ring-primary/40',
                isToday &&
                  !isCurrent &&
                  'bg-primary font-semibold text-primary-foreground hover:bg-primary/90',
                !disabled && !isToday && 'hover:bg-muted',
                disabled &&
                  !isCurrent &&
                  'cursor-not-allowed opacity-40 hover:bg-transparent',
                isCurrent && 'cursor-default',
              )}
            >
              {day.getDate()}
            </button>
          );
        })}
      </div>

      <p className="px-1 pb-0.5 text-[11px] text-muted-foreground">
        Pick a day to move it here.
      </p>
    </div>
  );
}

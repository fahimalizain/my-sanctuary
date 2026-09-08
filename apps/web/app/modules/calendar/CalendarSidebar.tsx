import { useEffect, useMemo, useState } from 'react';
import { ChevronLeft, ChevronRight, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { GoogleCalendar } from '@/app/types';
import { cn } from '@/lib/utils';
import {
  WEEK_DAYS,
  addDays,
  colorForCalendar,
  isSameDay,
  monthGridDays,
  monthGridStart,
  weekDays,
} from './week-layout';

const MONTH_LABEL = [
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

export interface CalendarSidebarProps {
  weekStart: Date;
  /** Jump the main week to the week containing this date. */
  onGoToDate: (date: Date) => void;
  calendars: GoogleCalendar[];
  calendarsLoading?: boolean;
  calendarsError?: string | null;
  onRetryCalendars?: () => void;
  selectedCalendarIds: Set<string>;
  onToggleCalendar: (calendarId: string) => void;
}

function sortCalendars(calendars: GoogleCalendar[]): GoogleCalendar[] {
  return [...calendars].sort((a, b) => {
    if (a.is_primary !== b.is_primary) return a.is_primary ? -1 : 1;
    const la = (a.summary || a.google_calendar_id).toLowerCase();
    const lb = (b.summary || b.google_calendar_id).toLowerCase();
    return la.localeCompare(lb);
  });
}

export function CalendarSidebar({
  weekStart,
  onGoToDate,
  calendars,
  calendarsLoading = false,
  calendarsError = null,
  onRetryCalendars,
  selectedCalendarIds,
  onToggleCalendar,
}: CalendarSidebarProps) {
  // Mini-month view: any date in the displayed month.
  const [viewMonth, setViewMonth] = useState(
    () => new Date(weekStart.getFullYear(), weekStart.getMonth(), 1),
  );

  const today = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate());
  }, []);

  const visibleWeekDays = useMemo(() => weekDays(weekStart), [weekStart]);
  const gridDays = useMemo(() => monthGridDays(viewMonth), [viewMonth]);

  // When the main week moves outside the mini-month grid, follow it.
  useEffect(() => {
    const gridStart = monthGridStart(viewMonth);
    const gridEnd = addDays(gridStart, 41);
    const weekEnd = addDays(weekStart, 6);
    const overlaps = weekStart.getTime() <= gridEnd.getTime() &&
      weekEnd.getTime() >= gridStart.getTime();
    if (!overlaps) {
      setViewMonth(
        new Date(weekStart.getFullYear(), weekStart.getMonth(), 1),
      );
    }
  }, [weekStart, viewMonth]);

  const monthLabel = `${MONTH_LABEL[viewMonth.getMonth()]} ${viewMonth.getFullYear()}`;
  const viewMonthIndex = viewMonth.getMonth();
  const viewYear = viewMonth.getFullYear();

  const sortedCalendars = useMemo(
    () => sortCalendars(calendars),
    [calendars],
  );

  const shiftMonth = (delta: number) => {
    setViewMonth(
      (prev) => new Date(prev.getFullYear(), prev.getMonth() + delta, 1),
    );
  };

  const handleDayClick = (day: Date) => {
    setViewMonth(new Date(day.getFullYear(), day.getMonth(), 1));
    onGoToDate(day);
  };

  return (
    <aside
      className="hidden md:flex w-[240px] shrink-0 flex-col border-r border-border/60 bg-cream overflow-y-auto"
      aria-label="Calendar sidebar"
    >
      {/* Mini-month */}
      <div className="px-3 pt-3 pb-2">
        <div className="flex items-center gap-1 mb-2">
          <h2 className="flex-1 min-w-0 text-sm font-semibold text-foreground truncate">
            {monthLabel}
          </h2>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={() => shiftMonth(-1)}
            aria-label="Previous month"
          >
            <ChevronLeft className="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={() => shiftMonth(1)}
            aria-label="Next month"
          >
            <ChevronRight className="h-3.5 w-3.5" />
          </Button>
        </div>

        {/* Weekday headers Mo–Su */}
        <div className="grid grid-cols-7 mb-0.5">
          {WEEK_DAYS.map((d) => (
            <div
              key={d}
              className="h-6 flex items-center justify-center text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
            >
              {d.slice(0, 2)}
            </div>
          ))}
        </div>

        {/* 6×7 day cells */}
        <div className="grid grid-cols-7">
          {gridDays.map((day) => {
            const inMonth =
              day.getMonth() === viewMonthIndex &&
              day.getFullYear() === viewYear;
            const isToday = isSameDay(day, today);
            const inViewedWeek = visibleWeekDays.some((d) =>
              isSameDay(d, day),
            );

            return (
              <button
                key={day.toISOString()}
                type="button"
                onClick={() => handleDayClick(day)}
                className={cn(
                  'h-8 w-full flex items-center justify-center rounded-full text-[12px] tabular-nums transition-colors',
                  'hover:bg-muted/80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/30',
                  !inMonth && 'text-muted-foreground/50',
                  inMonth && !isToday && 'text-foreground',
                  inViewedWeek && !isToday && 'bg-primary/10',
                  isToday &&
                    'bg-primary text-primary-foreground font-semibold hover:bg-primary/90',
                )}
                aria-label={day.toDateString()}
                aria-current={isToday ? 'date' : undefined}
              >
                {day.getDate()}
              </button>
            );
          })}
        </div>
      </div>

      {/* Calendar list */}
      <div className="px-3 pt-3 pb-4 border-t border-border/40 flex-1">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground mb-2">
          Calendars
        </h3>

        {calendarsLoading && sortedCalendars.length === 0 && (
          <div className="space-y-2" aria-busy="true" aria-label="Loading calendars">
            <div className="h-7 rounded-md bg-muted/60 animate-pulse" />
            <div className="h-7 rounded-md bg-muted/60 animate-pulse" />
            <div className="h-7 rounded-md bg-muted/40 animate-pulse w-4/5" />
          </div>
        )}

        {calendarsError && sortedCalendars.length === 0 && !calendarsLoading && (
          <div className="space-y-2">
            <p className="text-xs text-muted-foreground">
              Couldn&apos;t load calendars
            </p>
            {onRetryCalendars && (
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={onRetryCalendars}
              >
                <RefreshCw className="h-3 w-3 mr-1.5" />
                Retry
              </Button>
            )}
          </div>
        )}

        <ul className="space-y-0.5">
          {sortedCalendars.map((cal) => {
            const label =
              cal.summary.length > 0 ? cal.summary : cal.google_calendar_id;
            const color = colorForCalendar(cal.id);
            const checked =
              selectedCalendarIds.size === 0 ||
              selectedCalendarIds.has(cal.id);

            return (
              <li key={cal.id}>
                <label className="flex items-center gap-2 rounded-md px-1.5 py-1.5 cursor-pointer hover:bg-muted/50 transition-colors">
                  <input
                    type="checkbox"
                    className="sr-only peer"
                    checked={checked}
                    onChange={() => onToggleCalendar(cal.id)}
                  />
                  <span
                    className={cn(
                      'flex h-4 w-4 shrink-0 items-center justify-center rounded border transition-colors',
                      checked
                        ? 'border-transparent'
                        : 'border-border bg-background',
                    )}
                    style={
                      checked
                        ? { backgroundColor: color, borderColor: color }
                        : undefined
                    }
                    aria-hidden
                  >
                    {checked && (
                      <svg
                        viewBox="0 0 12 12"
                        className="h-2.5 w-2.5 text-white"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="2"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                      >
                        <path d="M2.5 6.5 L5 9 L9.5 3.5" />
                      </svg>
                    )}
                  </span>
                  <span
                    className="h-2 w-2 shrink-0 rounded-full"
                    style={{ backgroundColor: color }}
                    aria-hidden
                  />
                  <span className="min-w-0 flex-1 truncate text-[13px] text-foreground">
                    {label}
                  </span>
                </label>
              </li>
            );
          })}
        </ul>
      </div>
    </aside>
  );
}

import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  Clock,
  Loader2,
  RefreshCw,
} from 'lucide-react';
import { Button } from '@/components/ui/button';
import { API_BASE_URL } from '@/lib/api';
import type { CalendarEvent, CalendarEventsResponse } from '@/app/types';
import { cn } from '@/lib/utils';

const WEEK_DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

// Deterministic palette for event bars. Keyed off a hash of the event id so the
// same event always renders in the same color, but different events vary.
const EVENT_COLORS = [
  '#2a5c8a', // work-blue
  '#c45a2c', // gym-terracotta
  '#7a4a6a', // family-plum
  '#3a7a5a', // relax-green
  '#8a6a2c', // amber
  '#4a5c8a', // indigo
];

function colorForEvent(id: string): string {
  let hash = 0;
  for (let i = 0; i < id.length; i++) {
    hash = (hash << 5) - hash + id.charCodeAt(i);
    hash |= 0; // force int32
  }
  return EVENT_COLORS[Math.abs(hash) % EVENT_COLORS.length];
}

function formatTime(date: Date): string {
  return date.toLocaleTimeString('en-US', {
    hour: 'numeric',
    minute: '2-digit',
  });
}

function isSameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

function startOfWeek(date: Date): Date {
  const d = new Date(date);
  d.setHours(0, 0, 0, 0);
  // Treat Monday as the first day of the week (matches WEEK_DAYS).
  const jsDay = d.getDay(); // 0 = Sun ... 6 = Sat
  const offset = jsDay === 0 ? 6 : jsDay - 1;
  d.setDate(d.getDate() - offset);
  return d;
}

interface FetchState {
  events: CalendarEvent[];
  isLoading: boolean;
  error: string | null;
}

function useCalendarEvents() {
  const [state, setState] = useState<FetchState>({
    events: [],
    isLoading: true,
    error: null,
  });

  const load = useCallback(() => {
    setState((prev) => ({ ...prev, isLoading: true, error: null }));
    fetch(`${API_BASE_URL}/api/calendar/events`, {
      credentials: 'include',
    })
      .then(async (res) => {
        if (!res.ok) {
          throw new Error(`Request failed with status ${res.status}`);
        }
        const data = (await res.json()) as CalendarEventsResponse;
        setState({ events: data.events ?? [], isLoading: false, error: null });
      })
      .catch((err: unknown) => {
        const message =
          err instanceof Error ? err.message : 'Failed to load events';
        setState({ events: [], isLoading: false, error: message });
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return { ...state, retry: load };
}

interface MonthGridEventProps {
  event: CalendarEvent;
}

function MonthGridEvent({ event }: MonthGridEventProps) {
  return (
    <div
      className="rounded px-1.5 py-0.5 text-xs font-medium text-primary-foreground truncate"
      style={{ backgroundColor: colorForEvent(event.id) }}
      title={event.title}
    >
      {event.title}
    </div>
  );
}

interface TodaysEventProps {
  event: CalendarEvent;
}

function TodaysEvent({ event }: TodaysEventProps) {
  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  return (
    <div className="flex items-center gap-4 p-3 rounded-lg bg-muted">
      <div
        className="h-10 w-10 rounded-lg flex items-center justify-center shrink-0"
        style={{ backgroundColor: colorForEvent(event.id) }}
      >
        <Clock className="h-5 w-5 text-primary-foreground" />
      </div>
      <div className="flex-1 min-w-0">
        <p className="font-medium text-foreground truncate">{event.title}</p>
        <p className="text-sm text-muted-foreground">
          {formatTime(start)} - {formatTime(end)}
        </p>
      </div>
    </div>
  );
}

export function CalendarPage() {
  const { events, isLoading, error, retry } = useCalendarEvents();

  // Current view month/year. Starts at today; the prev/next buttons shift it.
  const [viewDate, setViewDate] = useState(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), 1);
  });

  const today = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate());
  }, []);

  // Index events by yyyy-mm-dd for O(1) lookups in the grid.
  const eventsByDay = useMemo(() => {
    const map = new Map<string, CalendarEvent[]>();
    for (const event of events) {
      const key = new Date(event.start_time).toDateString();
      const list = map.get(key);
      if (list) {
        list.push(event);
      } else {
        map.set(key, [event]);
      }
    }
    return map;
  }, [events]);

  const todaysEvents = useMemo(
    () =>
      (eventsByDay.get(today.toDateString()) ?? []).sort(
        (a, b) =>
          new Date(a.start_time).getTime() - new Date(b.start_time).getTime(),
      ),
    [eventsByDay, today],
  );

  // Upcoming events in the current week (Mon-Sun of `today`), excluding today.
  const weekEvents = useMemo(() => {
    const weekStart = startOfWeek(today);
    const weekEnd = new Date(weekStart);
    weekEnd.setDate(weekEnd.getDate() + 7);

    return events
      .filter((event) => {
        const start = new Date(event.start_time);
        return (
          start >= weekStart &&
          start < weekEnd &&
          !isSameDay(start, today) &&
          start >= today
        );
      })
      .sort(
        (a, b) =>
          new Date(a.start_time).getTime() - new Date(b.start_time).getTime(),
      );
  }, [events, today]);

  // Group week events by day name for the "This Week" panel.
  const weekEventsByDay = useMemo(() => {
    const groups: Record<string, CalendarEvent[]> = {};
    for (const event of weekEvents) {
      const dayName = new Date(event.start_time).toLocaleDateString('en-US', {
        weekday: 'long',
      });
      (groups[dayName] ??= []).push(event);
    }
    return groups;
  }, [weekEvents]);

  // Build the 6-row (42-cell) month grid starting on Monday.
  const gridCells = useMemo(() => {
    const firstOfMonth = new Date(viewDate.getFullYear(), viewDate.getMonth(), 1);
    const jsDay = firstOfMonth.getDay();
    const offset = jsDay === 0 ? 6 : jsDay - 1; // days to show from prev month
    const start = new Date(firstOfMonth);
    start.setDate(start.getDate() - offset);

    const cells: { date: Date; inMonth: boolean }[] = [];
    for (let i = 0; i < 42; i++) {
      const d = new Date(start);
      d.setDate(start.getDate() + i);
      cells.push({ date: d, inMonth: d.getMonth() === viewDate.getMonth() });
    }
    return cells;
  }, [viewDate]);

  const monthLabel = viewDate.toLocaleDateString('en-US', {
    month: 'long',
    year: 'numeric',
  });

  const shiftMonth = (delta: number) => {
    setViewDate((prev) => new Date(prev.getFullYear(), prev.getMonth() + delta, 1));
  };

  const goToToday = () => {
    const now = new Date();
    setViewDate(new Date(now.getFullYear(), now.getMonth(), 1));
  };

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              Calendar
            </h1>
            <p className="text-muted-foreground">{monthLabel}</p>
          </div>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="icon"
              onClick={() => shiftMonth(-1)}
              aria-label="Previous month"
            >
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <Button variant="outline" onClick={goToToday}>
              Today
            </Button>
            <Button
              variant="outline"
              size="icon"
              onClick={() => shiftMonth(1)}
              aria-label="Next month"
            >
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </header>

        {isLoading ? (
          <div className="flex flex-col items-center justify-center py-32 text-muted-foreground">
            <Loader2 className="h-8 w-8 animate-spin mb-3" />
            <p className="text-sm">Loading events...</p>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-32 text-center">
            <p className="text-foreground font-medium mb-2">
              Failed to load events
            </p>
            <p className="text-sm text-muted-foreground mb-4">{error}</p>
            <Button variant="outline" onClick={retry}>
              <RefreshCw className="h-4 w-4 mr-2" />
              Retry
            </Button>
          </div>
        ) : events.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-32 text-center">
            <CalendarDays className="h-10 w-10 text-muted-foreground mb-3" />
            <p className="text-foreground font-medium mb-2">
              No events synced yet
            </p>
            <p className="text-sm text-muted-foreground max-w-sm">
              Events sync from your connected Google Calendar automatically.
              Once synced they&apos;ll appear here.
            </p>
          </div>
        ) : (
          <>
            {/* Calendar Grid */}
            <div className="bg-card rounded-xl shadow-sm border border-border overflow-hidden">
              {/* Week Header */}
              <div className="grid grid-cols-7 border-b border-border">
                {WEEK_DAYS.map((day) => (
                  <div key={day} className="p-4 text-center">
                    <span className="text-sm font-medium text-muted-foreground">
                      {day}
                    </span>
                  </div>
                ))}
              </div>

              {/* Days Grid */}
              <div className="grid grid-cols-7 auto-rows-fr">
                {gridCells.map(({ date, inMonth }, index) => {
                  const isToday = isSameDay(date, today);
                  const dayEvents = eventsByDay.get(date.toDateString()) ?? [];
                  const visible = dayEvents.slice(0, 2);
                  const overflow = dayEvents.length - visible.length;

                  return (
                    <div
                      key={index}
                      className={cn(
                        'min-h-[120px] p-2 border-b border-r border-border',
                        !inMonth && 'bg-muted/50',
                        isToday && 'bg-primary/5',
                      )}
                    >
                      <span
                        className={cn(
                          'inline-flex h-7 w-7 items-center justify-center rounded-full text-sm',
                          isToday
                            ? 'bg-primary text-primary-foreground font-semibold'
                            : 'text-foreground',
                        )}
                      >
                        {date.getDate()}
                      </span>

                      {dayEvents.length > 0 && (
                        <div className="mt-2 space-y-1">
                          {visible.map((event) => (
                            <MonthGridEvent
                              key={event.id}
                              event={event}
                            />
                          ))}
                          {overflow > 0 && (
                            <p className="text-xs text-muted-foreground pl-1">
                              +{overflow} more
                            </p>
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>

            {/* Upcoming Events */}
            <div className="mt-8 grid grid-cols-1 lg:grid-cols-2 gap-6">
              <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
                <h2 className="font-heading text-lg font-semibold mb-4">
                  Today&apos;s Schedule
                </h2>
                {todaysEvents.length === 0 ? (
                  <p className="text-sm text-muted-foreground py-8 text-center">
                    Nothing on the calendar for today.
                  </p>
                ) : (
                  <div className="space-y-3">
                    {todaysEvents.map((event) => (
                      <TodaysEvent key={event.id} event={event} />
                    ))}
                  </div>
                )}
              </div>

              <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
                <h2 className="font-heading text-lg font-semibold mb-4">
                  This Week
                </h2>
                {weekEvents.length === 0 ? (
                  <p className="text-sm text-muted-foreground py-8 text-center">
                    No upcoming events this week.
                  </p>
                ) : (
                  <div className="space-y-4">
                    {WEEK_DAYS.map((dayShort) => {
                      const dayName =
                        dayShort === 'Mon'
                          ? 'Monday'
                          : dayShort === 'Tue'
                            ? 'Tuesday'
                            : dayShort === 'Wed'
                              ? 'Wednesday'
                              : dayShort === 'Thu'
                                ? 'Thursday'
                                : dayShort === 'Fri'
                                  ? 'Friday'
                                  : dayShort === 'Sat'
                                    ? 'Saturday'
                                    : 'Sunday';
                      const dayEvents = weekEventsByDay[dayName] ?? [];
                      if (dayEvents.length === 0) return null;
                      return (
                        <div key={dayShort} className="flex items-start gap-4">
                          <div className="w-16 shrink-0 text-sm font-medium text-muted-foreground pt-1">
                            {dayShort}
                          </div>
                          <div className="flex-1 space-y-1.5">
                            {dayEvents.map((event) => {
                              const start = new Date(event.start_time);
                              const end = new Date(event.end_time);
                              return (
                                <div
                                  key={event.id}
                                  className="flex items-center gap-2 rounded-md p-2"
                                  style={{
                                    backgroundColor: `${colorForEvent(event.id)}20`,
                                  }}
                                >
                                  <div
                                    className="h-2 w-2 rounded-full shrink-0"
                                    style={{
                                      backgroundColor: colorForEvent(event.id),
                                    }}
                                  />
                                  <span className="text-sm font-medium text-foreground truncate">
                                    {event.title}
                                  </span>
                                  <span className="text-xs text-muted-foreground ml-auto shrink-0">
                                    {formatTime(start)}-{formatTime(end)}
                                  </span>
                                </div>
                              );
                            })}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
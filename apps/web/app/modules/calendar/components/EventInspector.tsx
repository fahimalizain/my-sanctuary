import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { Clock, MoreHorizontal, Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from '@/components/ui/popover';
import type { CalendarEvent, GoogleCalendar } from '@/app/types';
import { cn } from '@/lib/utils';
import { eventChipColor } from '../lib/calendar-model';
import { isTempEventId } from '../lib/event-overlays';
import { DESKTOP_MQ, eventChipSelector } from '../lib/inspector-anchor';
import {
  allDayRangeToTimed,
  applyAllDayStartDate,
  applyStartDate,
  applyEndTime,
  applyStartTime,
  civilDateFromAllDayIso,
  formatEventDateLine,
  formatEventDuration,
  formatEventTime,
  formatEventTimeRange,
  timedRangeToAllDay,
  toDateInputValue,
  toTimeInputValue,
} from '../lib/week-layout';

const FALLBACK_TIME_ZONES = [
  'UTC',
  'America/New_York',
  'America/Chicago',
  'America/Denver',
  'America/Los_Angeles',
  'Europe/London',
  'Europe/Paris',
  'Asia/Riyadh',
  'Asia/Dubai',
  'Asia/Kolkata',
  'Asia/Tokyo',
  'Australia/Sydney',
] as const;

function browserTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}

function supportedTimeZones(): string[] {
  try {
    const intlWithZones = Intl as typeof Intl & {
      supportedValuesOf?: (key: string) => string[];
    };
    const values = intlWithZones.supportedValuesOf?.('timeZone');
    if (values && values.length > 0) return [...values];
  } catch {
    // fall through
  }
  return [...FALLBACK_TIME_ZONES];
}

function timeZoneOptions(current: string): string[] {
  const set = new Set(supportedTimeZones());
  const browser = browserTimeZone();
  if (browser) set.add(browser);
  if (current) set.add(current);
  return Array.from(set).sort((a, b) => a.localeCompare(b));
}

export interface EventInspectorProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  /** Writable calendars for the calendar select (local ids). */
  calendars?: GoogleCalendar[];
  /** Focus the title input on mount (click-to-create path). */
  focusTitle?: boolean;
  onClose: () => void;
  onSaveTitle: (summary: string) => void | Promise<void>;
  onSaveDescription: (description: string) => void | Promise<void>;
  onSaveTimes: (startIso: string, endIso: string) => void | Promise<void>;
  /** Toggle all-day; start/end are overlay ISO (UTC midnight civil or timed). */
  onSaveAllDay: (next: {
    isAllDay: boolean;
    startIso: string;
    endIso: string;
  }) => void | Promise<void>;
  /** Change event time zone (empty = local / omit zone on PATCH). */
  onSaveTimeZone: (
    timeZone: string,
    startIso: string,
    endIso: string,
  ) => void | Promise<void>;
  /** Move event to another calendar (local calendar id). */
  onSaveCalendar: (calendarId: string) => void | Promise<void>;
  onDelete: () => void | Promise<void>;
  isDeleting?: boolean;
  /**
   * While a drag is in progress, hide the inspector visually without
   * unmounting (unmount would call onClose and discard a draft).
   */
  isDragging?: boolean;
}

type ChipRect = {
  top: number;
  left: number;
  width: number;
  height: number;
};

/** Peek sheet height: ~10% of the viewport, never below a readable minimum. */
const PEEK_HEIGHT = 'max(76px, 10dvh)';
const PEEK_SHEET_HEIGHT = `calc(${PEEK_HEIGHT} + env(safe-area-inset-bottom))`;
const PEEK_SWIPE_PX = 32;
/** Room kept below the selected chip for its bottom touch handle. */
const PEEK_HANDLE_CLEARANCE_PX = 32;

/**
 * Pointer handlers for a sheet grabber: tap, or a vertical swipe past
 * PEEK_SWIPE_PX. A swipe suppresses the click that follows it.
 */
function useGrabber(cb: {
  onTap: () => void;
  onUp?: () => void;
  onDown?: () => void;
}) {
  const startY = useRef<number | null>(null);
  const swiped = useRef(false);
  return {
    onPointerDown: (e: ReactPointerEvent<HTMLElement>) => {
      startY.current = e.clientY;
      swiped.current = false;
    },
    onPointerUp: (e: ReactPointerEvent<HTMLElement>) => {
      const start = startY.current;
      startY.current = null;
      if (start === null) return;
      const dy = e.clientY - start;
      const handler =
        dy < -PEEK_SWIPE_PX ? cb.onUp : dy > PEEK_SWIPE_PX ? cb.onDown : null;
      if (handler) {
        swiped.current = true;
        handler();
      }
    },
    onClick: () => {
      if (swiped.current) {
        swiped.current = false;
        return;
      }
      cb.onTap();
    },
  };
}

function useIsDesktop(): boolean {
  const [isDesktop, setIsDesktop] = useState(
    () => window.matchMedia(DESKTOP_MQ).matches,
  );

  useEffect(() => {
    const mql = window.matchMedia(DESKTOP_MQ);
    const onChange = () => setIsDesktop(mql.matches);
    mql.addEventListener('change', onChange);
    return () => mql.removeEventListener('change', onChange);
  }, []);

  return isDesktop;
}

function useChipRect(eventId: string): ChipRect | null {
  const [rect, setRect] = useState<ChipRect | null>(null);

  useLayoutEffect(() => {
    let ro: ResizeObserver | null = null;
    let observed: Element | null = null;
    let raf = 0;
    let scroller: Element | null = null;

    const measure = (): Element | null => {
      const el = document.querySelector(eventChipSelector(eventId));
      if (!el) {
        setRect(null);
        return null;
      }
      const r = el.getBoundingClientRect();
      setRect({
        top: r.top,
        left: r.left,
        width: r.width,
        height: r.height,
      });
      return el;
    };

    const attachObserver = (el: Element) => {
      if (observed === el && ro) return;
      ro?.disconnect();
      ro = new ResizeObserver(() => {
        measure();
      });
      ro.observe(el);
      observed = el;
    };

    const sync = () => {
      const el = measure();
      if (el) attachObserver(el);
    };

    sync();
    if (!observed) {
      // Draft chip may not be painted yet — retry next frame.
      raf = requestAnimationFrame(sync);
    }

    scroller = document.querySelector('[data-calendar-scroller]');
    scroller?.addEventListener('scroll', sync, { passive: true });
    window.addEventListener('resize', sync);

    return () => {
      cancelAnimationFrame(raf);
      ro?.disconnect();
      scroller?.removeEventListener('scroll', sync);
      window.removeEventListener('resize', sync);
    };
  }, [eventId]);

  return rect;
}

/** Writable when calendar is omitted, or access_role is owner/writer. */
function isCalendarWritable(calendar?: GoogleCalendar): boolean {
  if (!calendar) return true;
  return calendar.access_role === 'owner' || calendar.access_role === 'writer';
}

/**
 * Whether the event should show a Repeat chip.
 * Non-empty recurring_event_id, or recurrence JSON that parses to a non-empty
 * array of non-blank strings. Invalid / empty / "[]" → false.
 */
function hasRepeat(event: CalendarEvent): boolean {
  if (event.recurring_event_id?.trim()) return true;
  const raw = event.recurrence?.trim();
  if (!raw) return false;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed) || parsed.length === 0) return false;
    return parsed.some(
      (item) => typeof item === 'string' && item.trim().length > 0,
    );
  } catch {
    return false;
  }
}

function isWritableCalendar(cal: GoogleCalendar): boolean {
  return cal.access_role === 'owner' || cal.access_role === 'writer';
}

interface InspectorFormProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  calendars: GoogleCalendar[];
  title: string;
  setTitle: (v: string) => void;
  titleRef: RefObject<HTMLInputElement | null>;
  commitTitle: () => void;
  description: string;
  setDescription: (v: string) => void;
  commitDescription: () => void;
  commitTimes: (next: { start: Date; end: Date } | null) => void;
  commitAllDayDate: (ymd: string) => void;
  onToggleAllDay: () => void;
  onChangeTimeZone: (zone: string) => void;
  onClose: () => void;
  onSaveCalendar: (calendarId: string) => void | Promise<void>;
  onDelete: () => void | Promise<void>;
  isDeleting: boolean;
  writable: boolean;
}

const timeInputClassName =
  'border-0 bg-transparent p-0 text-sm tabular-nums text-foreground focus:outline-none focus:ring-0 disabled:opacity-50';
const dateInputClassName =
  'border-0 bg-transparent p-0 text-sm text-muted-foreground focus:outline-none focus:ring-0 disabled:opacity-50';
const chipClassName =
  'rounded-full border border-border/70 px-2 py-0.5 text-xs text-muted-foreground';
const chipToggleClassName =
  'rounded-full border border-border/70 px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-muted/60 aria-pressed:border-foreground/40 aria-pressed:bg-muted/40 aria-pressed:text-foreground disabled:opacity-50';

function InspectorForm({
  event,
  calendar,
  calendars,
  title,
  setTitle,
  titleRef,
  commitTitle,
  description,
  setDescription,
  commitDescription,
  commitTimes,
  commitAllDayDate,
  onToggleAllDay,
  onChangeTimeZone,
  onClose,
  onSaveCalendar,
  onDelete,
  isDeleting,
  writable,
}: InspectorFormProps) {
  const [menuOpen, setMenuOpen] = useState(false);

  const isAllDay = event.is_all_day === true;
  const start = isAllDay
    ? (civilDateFromAllDayIso(event.start_time) ?? new Date(event.start_time))
    : new Date(event.start_time);
  const end = isAllDay
    ? (civilDateFromAllDayIso(event.end_time) ?? new Date(event.end_time))
    : new Date(event.end_time);
  const durationLabel = formatEventDuration(
    new Date(event.start_time),
    new Date(event.end_time),
  );
  const whenRange = `${formatEventTime(new Date(event.start_time))} → ${formatEventTime(new Date(event.end_time))} ${durationLabel}`;
  const dateLine = formatEventDateLine(start, end);
  const timesEditable = writable && !isAllDay;
  const allDayDateEditable = writable && isAllDay;
  const crossDay =
    start.getFullYear() !== end.getFullYear() ||
    start.getMonth() !== end.getMonth() ||
    start.getDate() !== end.getDate();

  const timeZone = event.start_time_zone?.trim() || '';
  const showRepeat = hasRepeat(event);
  const browserZone = browserTimeZone();
  const zoneOptions = timeZoneOptions(timeZone);
  // Writable: always show All-day toggle; zone select when timed.
  // Read-only: static chips only when set.
  const showChipRow = writable || isAllDay || Boolean(timeZone) || showRepeat;

  const descriptionDisplay = description.trim();
  const calendarLabel = calendar?.summary || 'Calendar';
  const swatch = eventChipColor(event);

  const writableCalendars = calendars.filter(isWritableCalendar);
  const currentInWritable = writableCalendars.some(
    (c) => c.id === event.calendar_id,
  );
  // Reader calendar not in writable list: keep a disabled option so value
  // does not reset.
  const calendarSelectOptions =
    !currentInWritable && calendar
      ? [calendar, ...writableCalendars]
      : writableCalendars;

  const allDayDateValue = isAllDay
    ? civilDateFromAllDayIso(event.start_time)
      ? toDateInputValue(civilDateFromAllDayIso(event.start_time)!)
      : toDateInputValue(start)
    : toDateInputValue(start);

  return (
    <>
      <div className="flex h-10 shrink-0 items-center justify-end gap-0.5 border-b border-border/60 px-2">
        {writable ? (
          <div className="relative">
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="h-8 w-8 shrink-0"
              onClick={() => setMenuOpen((open) => !open)}
              aria-label="More actions"
            >
              <MoreHorizontal className="h-4 w-4" />
            </Button>
            {menuOpen && (
              <>
                <div
                  className="fixed inset-0 z-10"
                  onClick={() => setMenuOpen(false)}
                />
                <div className="absolute right-0 top-9 z-20 w-36 rounded-lg border border-border bg-popover py-1 shadow-lg">
                  <button
                    type="button"
                    aria-label="Delete event"
                    disabled={isDeleting}
                    onClick={() => {
                      setMenuOpen(false);
                      void onDelete();
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-destructive transition-colors hover:bg-destructive/10 disabled:opacity-50"
                  >
                    <Trash2 className="h-4 w-4" />
                    {isDeleting ? 'Deleting…' : 'Delete'}
                  </button>
                </div>
              </>
            )}
          </div>
        ) : null}
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-8 w-8 shrink-0"
          onClick={onClose}
          aria-label="Close inspector"
        >
          <X className="h-4 w-4" />
        </Button>
      </div>

      <div className="flex-1 min-h-0 space-y-3 overflow-y-auto px-3 py-3">
        {/* Title — large, unlabeled */}
        {writable ? (
          <input
            ref={titleRef}
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            onBlur={commitTitle}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                (e.target as HTMLInputElement).blur();
              }
            }}
            aria-label="Title"
            className="w-full border-0 bg-transparent p-0 text-lg font-semibold text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-0 disabled:opacity-50"
          />
        ) : (
          <p className="text-lg font-semibold text-foreground">{title}</p>
        )}

        {/* When — clock-led row; timed+writable uses native time/date inputs */}
        <div className="flex gap-2.5">
          <Clock
            className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground"
            aria-hidden
          />
          <div className="min-w-0 space-y-0.5">
            {timesEditable ? (
              <>
                <div className="flex flex-wrap items-baseline gap-x-1 text-sm tabular-nums text-foreground">
                  <input
                    type="time"
                    step={60}
                    aria-label="Start time"
                    value={toTimeInputValue(start)}
                    onChange={(e) => {
                      commitTimes(applyStartTime(start, end, e.target.value));
                    }}
                    onBlur={(e) => {
                      commitTimes(applyStartTime(start, end, e.target.value));
                    }}
                    className={timeInputClassName}
                  />
                  <span aria-hidden>→</span>
                  <input
                    type="time"
                    step={60}
                    aria-label="End time"
                    value={toTimeInputValue(end)}
                    onChange={(e) => {
                      commitTimes(applyEndTime(start, end, e.target.value));
                    }}
                    onBlur={(e) => {
                      commitTimes(applyEndTime(start, end, e.target.value));
                    }}
                    className={timeInputClassName}
                  />
                  <span className="text-sm tabular-nums text-foreground">
                    {durationLabel}
                  </span>
                </div>
                <div className="flex flex-wrap items-baseline gap-x-1.5">
                  <input
                    type="date"
                    aria-label="Date"
                    value={toDateInputValue(start)}
                    onChange={(e) => {
                      commitTimes(applyStartDate(start, end, e.target.value));
                    }}
                    onBlur={(e) => {
                      commitTimes(applyStartDate(start, end, e.target.value));
                    }}
                    className={dateInputClassName}
                  />
                  {crossDay ? (
                    <span className="text-sm text-muted-foreground">
                      {dateLine}
                    </span>
                  ) : null}
                </div>
              </>
            ) : allDayDateEditable ? (
              <div className="flex flex-wrap items-baseline gap-x-1.5">
                <input
                  type="date"
                  aria-label="Date"
                  value={allDayDateValue}
                  onChange={(e) => {
                    commitAllDayDate(e.target.value);
                  }}
                  onBlur={(e) => {
                    commitAllDayDate(e.target.value);
                  }}
                  className={dateInputClassName}
                />
              </div>
            ) : (
              <>
                <p className="text-sm tabular-nums text-foreground">
                  {whenRange}
                </p>
                <p className="text-sm text-muted-foreground">{dateLine}</p>
              </>
            )}
            {showChipRow ? (
              <div className="flex flex-wrap items-center gap-1.5 pt-1">
                {writable ? (
                  <button
                    type="button"
                    aria-label="All-day"
                    aria-pressed={isAllDay}
                    onClick={onToggleAllDay}
                    className={chipToggleClassName}
                  >
                    All-day
                  </button>
                ) : isAllDay ? (
                  <span className={chipClassName}>All-day</span>
                ) : null}
                {writable && !isAllDay ? (
                  <select
                    aria-label="Time zone"
                    value={timeZone}
                    onChange={(e) => {
                      onChangeTimeZone(e.target.value);
                    }}
                    className="max-w-[12rem] truncate rounded-full border border-border/70 bg-transparent px-2 py-0.5 text-xs text-muted-foreground focus:outline-none focus:ring-0 disabled:opacity-50"
                  >
                    <option value="">
                      {browserZone ? browserZone : 'Local'}
                    </option>
                    {zoneOptions.map((z) => (
                      <option key={z} value={z}>
                        {z}
                      </option>
                    ))}
                  </select>
                ) : !writable && timeZone ? (
                  <span className={chipClassName}>{timeZone}</span>
                ) : null}
                {showRepeat ? (
                  <span className={chipClassName}>Repeat</span>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>

        {/* Description — textarea when writable; static when read-only */}
        {writable ? (
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            onBlur={commitDescription}
            onKeyDown={(e) => {
              // Enter inserts a newline; Cmd/Ctrl+Enter blurs to commit.
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                (e.target as HTMLTextAreaElement).blur();
              }
            }}
            aria-label="Description"
            placeholder="Description"
            rows={3}
            className="w-full resize-none border-0 bg-transparent p-0 text-sm text-foreground whitespace-pre-wrap placeholder:text-muted-foreground/60 focus:outline-none focus:ring-0 disabled:opacity-50"
          />
        ) : descriptionDisplay ? (
          <p className="whitespace-pre-wrap text-sm text-foreground">
            {descriptionDisplay}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">Description</p>
        )}

        {/* Calendar — swatch + select (writable) or static summary */}
        <div className="flex min-w-0 items-center gap-2">
          <span
            className="h-2.5 w-2.5 shrink-0 rounded-full"
            style={{ backgroundColor: swatch }}
            aria-hidden
          />
          {writable ? (
            <select
              aria-label="Calendar"
              value={event.calendar_id}
              onChange={(e) => {
                const next = e.target.value;
                if (next === event.calendar_id) return;
                void onSaveCalendar(next);
              }}
              className="min-w-0 flex-1 truncate border-0 bg-transparent p-0 text-sm text-foreground focus:outline-none focus:ring-0 disabled:opacity-50"
            >
              {calendarSelectOptions.map((cal) => {
                const label =
                  cal.summary.trim().length > 0
                    ? cal.summary
                    : cal.google_calendar_id;
                const disabledOption = !isWritableCalendar(cal);
                return (
                  <option key={cal.id} value={cal.id} disabled={disabledOption}>
                    {label}
                  </option>
                );
              })}
            </select>
          ) : (
            <span className="truncate text-sm text-foreground">
              {calendarLabel}
            </span>
          )}
        </div>
      </div>
    </>
  );
}

/**
 * Floating inspector for a selected calendar event.
 * Desktop (md+): popover anchored to the event chip.
 * Mobile: bottom drawer over the grid.
 */
export function EventInspector({
  event,
  calendar,
  calendars = [],
  focusTitle = false,
  onClose,
  onSaveTitle,
  onSaveDescription,
  onSaveTimes,
  onSaveAllDay,
  onSaveTimeZone,
  onSaveCalendar,
  onDelete,
  isDeleting = false,
  isDragging = false,
}: EventInspectorProps) {
  const [title, setTitle] = useState(event.title);
  const titleRef = useRef<HTMLInputElement>(null);
  // Track the last-saved title so blur after an unchanged edit is a no-op.
  const savedTitleRef = useRef(event.title);
  const [description, setDescription] = useState(event.description ?? '');
  // Committed (trimmed) description — blank is a valid clear, unlike title.
  const savedDescriptionRef = useRef((event.description ?? '').trim());
  const isDesktop = useIsDesktop();
  const rect = useChipRect(event.id);
  const hidden = isDragging;
  // Mobile drawer: new drafts open expanded (title focus); taps peek.
  const [expanded, setExpanded] = useState(focusTitle);
  const sheetRef = useRef<HTMLDivElement>(null);
  const peekGrab = useGrabber({
    onTap: () => setExpanded(true),
    onUp: () => setExpanded(true),
    onDown: () => onClose(),
  });
  const sheetGrab = useGrabber({
    onTap: () => setExpanded(false),
    onDown: () => setExpanded(false),
  });
  const writable = isCalendarWritable(calendar);

  // Sync local title when the selected event changes — but keep a dirty
  // (unblurred) edit across temp→server id remap so typed text is not lost.
  useEffect(() => {
    const dirty = title !== savedTitleRef.current;
    if (dirty) return;
    setTitle(event.title);
    savedTitleRef.current = event.title;
  }, [event.id, event.title]);

  // Same dirty-across-remap rule for description.
  useEffect(() => {
    const dirty = description !== savedDescriptionRef.current;
    if (dirty) return;
    const next = event.description ?? '';
    setDescription(next);
    savedDescriptionRef.current = next.trim();
  }, [event.id, event.description]);

  useEffect(() => {
    if (focusTitle && writable) {
      titleRef.current?.focus();
      titleRef.current?.select();
    }
  }, [focusTitle, event.id, writable]);

  // Reset on a new selection or a focusTitle flip, but not when a draft's
  // temp id is remapped to its server id (same logical event).
  const prevIdRef = useRef(event.id);
  const prevFocusRef = useRef(focusTitle);
  useEffect(() => {
    const prevId = prevIdRef.current;
    const remap =
      prevId !== event.id &&
      isTempEventId(prevId) &&
      prevFocusRef.current === focusTitle;
    prevIdRef.current = event.id;
    prevFocusRef.current = focusTitle;
    if (!remap) setExpanded(focusTitle);
  }, [event.id, focusTitle]);

  // While peeking, let the grid scroll past the sheet so late events stay reachable.
  useEffect(() => {
    if (isDesktop || expanded) return;
    const scroller = document.querySelector('[data-calendar-scroller]');
    if (!(scroller instanceof HTMLElement)) return;
    const prev = scroller.style.paddingBottom;
    scroller.style.paddingBottom = PEEK_SHEET_HEIGHT;
    return () => {
      scroller.style.paddingBottom = prev;
    };
  }, [isDesktop, expanded]);

  // Keep the selected chip clear of the peek sheet (and its handles).
  useEffect(() => {
    if (isDesktop || expanded) return;
    const raf = requestAnimationFrame(() => {
      const chip = document.querySelector(eventChipSelector(event.id));
      const scroller = document.querySelector('[data-calendar-scroller]');
      const sheet = sheetRef.current;
      if (!chip || !(scroller instanceof HTMLElement) || !sheet) return;
      const c = chip.getBoundingClientRect();
      const s = scroller.getBoundingClientRect();
      const overlap =
        c.bottom + PEEK_HANDLE_CLEARANCE_PX - sheet.getBoundingClientRect().top;
      if (overlap <= 0) return;
      // Never scroll the chip's top out of view.
      const room = Math.max(0, c.top - s.top - 8);
      scroller.scrollTop += Math.min(overlap, room);
    });
    return () => cancelAnimationFrame(raf);
  }, [isDesktop, expanded, event.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (isDragging) return;
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isDragging, onClose]);

  const commitTitle = () => {
    const next = title.trim();
    if (!next || next === savedTitleRef.current) {
      // Restore if blanked out.
      if (!next) setTitle(savedTitleRef.current);
      return;
    }
    savedTitleRef.current = next;
    void onSaveTitle(next);
  };

  const commitDescription = () => {
    const next = description.trim();
    if (next === savedDescriptionRef.current) {
      // Normalize whitespace-only / trailing spaces in the field.
      if (description !== next) setDescription(next);
      return;
    }
    savedDescriptionRef.current = next;
    setDescription(next);
    void onSaveDescription(next);
  };

  // Times are derived from event props (no local dirty draft). Unchanged
  // range is a no-op so change+blur double-fire does not double-PATCH.
  // Compare instants (getTime), not ISO strings — Google/replica often omits
  // milliseconds (`...00Z`) while Date.toISOString() always emits `.000Z`.
  const commitTimes = (next: { start: Date; end: Date } | null) => {
    if (!next) return;
    const currentStart = new Date(event.start_time);
    const currentEnd = new Date(event.end_time);
    if (
      Number.isNaN(currentStart.getTime()) ||
      Number.isNaN(currentEnd.getTime())
    ) {
      return;
    }
    if (
      next.start.getTime() === currentStart.getTime() &&
      next.end.getTime() === currentEnd.getTime()
    ) {
      return;
    }
    void onSaveTimes(next.start.toISOString(), next.end.toISOString());
  };

  const commitAllDayDate = (ymd: string) => {
    if (!ymd.trim()) return;
    const next = applyAllDayStartDate(event.start_time, event.end_time, ymd);
    if (!next) return;
    // Compare civil prefixes so replica `…Z` vs helper `…Z` stay no-ops.
    const curS = civilDateFromAllDayIso(event.start_time);
    const curE = civilDateFromAllDayIso(event.end_time);
    const nextS = civilDateFromAllDayIso(next.startIso);
    const nextE = civilDateFromAllDayIso(next.endIso);
    if (
      curS &&
      curE &&
      nextS &&
      nextE &&
      curS.getTime() === nextS.getTime() &&
      curE.getTime() === nextE.getTime()
    ) {
      return;
    }
    void onSaveAllDay({
      isAllDay: true,
      startIso: next.startIso,
      endIso: next.endIso,
    });
  };

  const onToggleAllDay = () => {
    if (event.is_all_day === true) {
      const timed = allDayRangeToTimed(event.start_time, event.end_time);
      if (!timed) return;
      void onSaveAllDay({
        isAllDay: false,
        startIso: timed.start.toISOString(),
        endIso: timed.end.toISOString(),
      });
      return;
    }
    const start = new Date(event.start_time);
    const end = new Date(event.end_time);
    if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return;
    const allDay = timedRangeToAllDay(start, end);
    void onSaveAllDay({
      isAllDay: true,
      startIso: allDay.startIso,
      endIso: allDay.endIso,
    });
  };

  const onChangeTimeZone = (zone: string) => {
    const current = event.start_time_zone?.trim() || '';
    if (zone === current) return;
    void onSaveTimeZone(zone, event.start_time, event.end_time);
  };

  const form = (
    <InspectorForm
      event={event}
      calendar={calendar}
      calendars={calendars}
      title={title}
      setTitle={setTitle}
      titleRef={titleRef}
      commitTitle={commitTitle}
      description={description}
      setDescription={setDescription}
      commitDescription={commitDescription}
      commitTimes={commitTimes}
      commitAllDayDate={commitAllDayDate}
      onToggleAllDay={onToggleAllDay}
      onChangeTimeZone={onChangeTimeZone}
      onClose={onClose}
      onSaveCalendar={onSaveCalendar}
      onDelete={onDelete}
      isDeleting={isDeleting}
      writable={writable}
    />
  );

  if (isDesktop) {
    return (
      <Popover
        open
        onOpenChange={(open) => {
          if (!open) onClose();
        }}
        modal={false}
      >
        <PopoverAnchor asChild>
          <span
            aria-hidden
            className="fixed pointer-events-none"
            style={{
              top: rect?.top ?? 0,
              left: rect?.left ?? 0,
              width: rect?.width ?? 0,
              height: rect?.height ?? 0,
            }}
          />
        </PopoverAnchor>
        <PopoverContent
          side="right"
          align="start"
          sideOffset={8}
          collisionPadding={12}
          className={cn(
            'z-[60] w-80 p-0 bg-cream flex flex-col max-h-[min(80dvh,560px)]',
            hidden && 'invisible pointer-events-none',
          )}
          onOpenAutoFocus={(e) => {
            if (!focusTitle) e.preventDefault();
          }}
          onInteractOutside={(e) => {
            const target = e.target;
            if (!(target instanceof Element)) return;
            // Chip pointerdown is select / move / resize — keep the popover mounted.
            if (
              target.closest(
                '[data-event-chip], [data-allday-chip], [data-event-id]',
              )
            ) {
              e.preventDefault();
              return;
            }
            // Native time/date pickers often render outside the popover.
            if (target.closest('input[type="time"], input[type="date"]')) {
              e.preventDefault();
            }
          }}
          data-event-inspector
          aria-label="Event details"
        >
          {form}
        </PopoverContent>
      </Popover>
    );
  }

  const isAllDay = event.is_all_day === true;
  const peekWhen = isAllDay
    ? formatEventDateLine(
        civilDateFromAllDayIso(event.start_time) ?? new Date(event.start_time),
        civilDateFromAllDayIso(event.end_time) ?? new Date(event.end_time),
      )
    : formatEventTimeRange(
        new Date(event.start_time),
        new Date(event.end_time),
      );

  // Wrapper ignores pointers so the calendar stays live above the peek; only
  // the sheet (and, expanded, the backdrop) take them.
  return createPortal(
    <div
      className={cn(
        'pointer-events-none fixed inset-0 z-[60]',
        hidden && 'invisible',
      )}
      data-event-inspector
    >
      <button
        type="button"
        tabIndex={expanded ? 0 : -1}
        aria-hidden={!expanded}
        className={cn(
          'absolute inset-0 bg-black/40 transition-opacity motion-reduce:transition-none',
          expanded ? 'pointer-events-auto opacity-100' : 'opacity-0',
        )}
        aria-label="Collapse event details"
        onClick={() => setExpanded(false)}
      />
      <div
        ref={sheetRef}
        role="dialog"
        aria-label="Event details"
        className="pointer-events-auto absolute inset-x-0 bottom-0 flex flex-col overflow-hidden rounded-t-2xl border-t border-border bg-cream shadow-lg transition-[max-height] duration-200 ease-out motion-reduce:transition-none"
        style={{
          maxHeight: expanded ? '85dvh' : PEEK_SHEET_HEIGHT,
        }}
      >
        {!expanded && (
          <div className="relative shrink-0">
            <button
              type="button"
              aria-label="Expand event details"
              aria-expanded={false}
              className="flex w-full touch-none flex-col items-stretch px-4 pb-[calc(0.5rem+env(safe-area-inset-bottom))] pt-2 pr-12 text-left"
              style={{ minHeight: PEEK_SHEET_HEIGHT }}
              {...peekGrab}
            >
              <span
                className="mx-auto mb-2 h-1 w-10 shrink-0 rounded-full bg-muted-foreground/40"
                aria-hidden
              />
              <span className="truncate text-sm font-semibold text-foreground">
                {title.trim() || 'New event'}
              </span>
              <span className="truncate text-xs tabular-nums text-muted-foreground">
                {peekWhen}
              </span>
            </button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              className="absolute right-1 top-2 h-8 w-8"
              onClick={onClose}
              aria-label="Close event details"
            >
              <X className="h-4 w-4" />
            </Button>
          </div>
        )}
        {expanded && (
          <button
            type="button"
            aria-label="Collapse drawer"
            className="flex h-6 w-full shrink-0 touch-none items-center justify-center"
            {...sheetGrab}
          >
            <span
              className="h-1 w-10 rounded-full bg-muted-foreground/40"
              aria-hidden
            />
          </button>
        )}
        {/* Stays mounted while peeking so unsaved edits and focus survive. */}
        <div
          className={cn(
            'min-h-0 flex-1 flex-col',
            expanded ? 'flex' : 'hidden',
          )}
        >
          {form}
        </div>
      </div>
    </div>,
    document.body,
  );
}

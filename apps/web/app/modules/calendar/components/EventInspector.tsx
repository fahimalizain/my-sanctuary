import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
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
import { eventChipSelector } from '../lib/inspector-anchor';
import {
  applyStartDate,
  applyEndTime,
  applyStartTime,
  formatEventDateLine,
  formatEventDuration,
  formatEventTime,
  toDateInputValue,
  toTimeInputValue,
} from '../lib/week-layout';

const DESKTOP_MQ = '(min-width: 768px)';

export interface EventInspectorProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  /** Focus the title input on mount (click-to-create path). */
  focusTitle?: boolean;
  onClose: () => void;
  onSaveTitle: (summary: string) => void | Promise<void>;
  onSaveDescription: (description: string) => void | Promise<void>;
  onSaveTimes: (startIso: string, endIso: string) => void | Promise<void>;
  onDelete: () => void | Promise<void>;
  isSaving?: boolean;
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

interface InspectorFormProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  title: string;
  setTitle: (v: string) => void;
  titleRef: RefObject<HTMLInputElement | null>;
  commitTitle: () => void;
  description: string;
  setDescription: (v: string) => void;
  commitDescription: () => void;
  commitTimes: (next: { start: Date; end: Date } | null) => void;
  onClose: () => void;
  onDelete: () => void | Promise<void>;
  isSaving: boolean;
  isDeleting: boolean;
  writable: boolean;
}

const timeInputClassName =
  'border-0 bg-transparent p-0 text-sm tabular-nums text-foreground focus:outline-none focus:ring-0 disabled:opacity-50';
const dateInputClassName =
  'border-0 bg-transparent p-0 text-sm text-muted-foreground focus:outline-none focus:ring-0 disabled:opacity-50';

function InspectorForm({
  event,
  calendar,
  title,
  setTitle,
  titleRef,
  commitTitle,
  description,
  setDescription,
  commitDescription,
  commitTimes,
  onClose,
  onDelete,
  isSaving,
  isDeleting,
  writable,
}: InspectorFormProps) {
  const [menuOpen, setMenuOpen] = useState(false);

  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const durationLabel = formatEventDuration(start, end);
  const whenRange = `${formatEventTime(start)} → ${formatEventTime(end)} ${durationLabel}`;
  const dateLine = formatEventDateLine(start, end);
  const timesEditable = writable && event.is_all_day !== true;
  const crossDay =
    start.getFullYear() !== end.getFullYear() ||
    start.getMonth() !== end.getMonth() ||
    start.getDate() !== end.getDate();

  const timeZone = event.start_time_zone?.trim() || '';
  const showAllDay = event.is_all_day === true;
  const showRepeat = hasRepeat(event);
  const showChips = showAllDay || Boolean(timeZone) || showRepeat;

  const descriptionDisplay = description.trim();
  const calendarLabel = calendar?.summary || 'Calendar';
  const swatch = eventChipColor(event);
  const inputsDisabled = isSaving || isDeleting;

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
              disabled={isDeleting || isSaving}
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
                    disabled={isDeleting || isSaving}
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
            disabled={isSaving || isDeleting}
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
                    disabled={inputsDisabled}
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
                    disabled={inputsDisabled}
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
                    disabled={inputsDisabled}
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
            ) : (
              <>
                <p className="text-sm tabular-nums text-foreground">
                  {whenRange}
                </p>
                <p className="text-sm text-muted-foreground">{dateLine}</p>
              </>
            )}
            {showChips ? (
              <div className="flex flex-wrap gap-1.5 pt-1">
                {showAllDay ? (
                  <span className="rounded-full border border-border/70 px-2 py-0.5 text-xs text-muted-foreground">
                    All-day
                  </span>
                ) : null}
                {timeZone ? (
                  <span className="rounded-full border border-border/70 px-2 py-0.5 text-xs text-muted-foreground">
                    {timeZone}
                  </span>
                ) : null}
                {showRepeat ? (
                  <span className="rounded-full border border-border/70 px-2 py-0.5 text-xs text-muted-foreground">
                    Repeat
                  </span>
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
            disabled={isSaving || isDeleting}
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

        {/* Calendar — swatch + summary */}
        <div className="flex min-w-0 items-center gap-2">
          <span
            className="h-2.5 w-2.5 shrink-0 rounded-full"
            style={{ backgroundColor: swatch }}
            aria-hidden
          />
          <span className="truncate text-sm text-foreground">
            {calendarLabel}
          </span>
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
  focusTitle = false,
  onClose,
  onSaveTitle,
  onSaveDescription,
  onSaveTimes,
  onDelete,
  isSaving = false,
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
  const commitTimes = (next: { start: Date; end: Date } | null) => {
    if (!next) return;
    const startIso = next.start.toISOString();
    const endIso = next.end.toISOString();
    if (startIso === event.start_time && endIso === event.end_time) return;
    void onSaveTimes(startIso, endIso);
  };

  const form = (
    <InspectorForm
      event={event}
      calendar={calendar}
      title={title}
      setTitle={setTitle}
      titleRef={titleRef}
      commitTitle={commitTitle}
      description={description}
      setDescription={setDescription}
      commitDescription={commitDescription}
      commitTimes={commitTimes}
      onClose={onClose}
      onDelete={onDelete}
      isSaving={isSaving}
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

  return createPortal(
    <div
      className={cn(
        'fixed inset-0 z-[60]',
        hidden && 'invisible pointer-events-none',
      )}
      data-event-inspector
    >
      <button
        type="button"
        className="absolute inset-0 bg-black/40"
        aria-label="Close event details"
        onClick={onClose}
      />
      <div
        role="dialog"
        aria-label="Event details"
        className="absolute inset-x-0 bottom-0 max-h-[85dvh] rounded-t-2xl border-t border-border bg-cream shadow-lg flex flex-col"
      >
        {form}
      </div>
    </div>,
    document.body,
  );
}

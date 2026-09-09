import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from 'react';
import { createPortal } from 'react-dom';
import { Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from '@/components/ui/popover';
import type { CalendarEvent, GoogleCalendar } from '@/app/types';
import { cn } from '@/lib/utils';
import { eventChipSelector } from '../lib/inspector-anchor';
import { formatEventTimeRange } from '../lib/week-layout';

const DESKTOP_MQ = '(min-width: 768px)';

export interface EventInspectorProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  /** Focus the title input on mount (click-to-create path). */
  focusTitle?: boolean;
  onClose: () => void;
  onSaveTitle: (summary: string) => void | Promise<void>;
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

interface InspectorFormProps {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  title: string;
  setTitle: (v: string) => void;
  titleRef: RefObject<HTMLInputElement | null>;
  commitTitle: () => void;
  onClose: () => void;
  onDelete: () => void | Promise<void>;
  isSaving: boolean;
  isDeleting: boolean;
}

function InspectorForm({
  event,
  calendar,
  title,
  setTitle,
  titleRef,
  commitTitle,
  onClose,
  onDelete,
  isSaving,
  isDeleting,
}: InspectorFormProps) {
  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const timeLabel = formatEventTimeRange(start, end);
  const calendarLabel = calendar?.summary || 'Calendar';
  const description = event.description?.trim() ?? '';

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border/60 px-3">
        <h2 className="min-w-0 flex-1 truncate text-sm font-semibold text-foreground">
          Event
        </h2>
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

      <div className="flex-1 min-h-0 space-y-4 overflow-y-auto p-3">
        <div>
          <label
            htmlFor="event-inspector-title"
            className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-muted-foreground"
          >
            Title
          </label>
          <input
            ref={titleRef}
            id="event-inspector-title"
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
            className="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground/60 focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20 disabled:opacity-50"
          />
        </div>

        <div>
          <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Time
          </p>
          <p className="text-sm text-foreground tabular-nums">{timeLabel}</p>
        </div>

        <div>
          <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            Calendar
          </p>
          <p className="truncate text-sm text-foreground">{calendarLabel}</p>
        </div>

        {description ? (
          <div>
            <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
              Description
            </p>
            <p className="whitespace-pre-wrap text-sm text-muted-foreground">
              {description}
            </p>
          </div>
        ) : null}
      </div>

      <div className="shrink-0 border-t border-border/60 p-3">
        <Button
          type="button"
          variant="outline"
          className="w-full text-destructive hover:bg-destructive/10 hover:text-destructive"
          onClick={() => void onDelete()}
          disabled={isDeleting || isSaving}
        >
          <Trash2 className="mr-2 h-4 w-4" />
          {isDeleting ? 'Deleting…' : 'Delete event'}
        </Button>
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
  onDelete,
  isSaving = false,
  isDeleting = false,
  isDragging = false,
}: EventInspectorProps) {
  const [title, setTitle] = useState(event.title);
  const titleRef = useRef<HTMLInputElement>(null);
  // Track the last-saved title so blur after an unchanged edit is a no-op.
  const savedTitleRef = useRef(event.title);
  const isDesktop = useIsDesktop();
  const rect = useChipRect(event.id);
  const hidden = isDragging;

  // Sync local title when the selected event changes — but keep a dirty
  // (unblurred) edit across temp→server id remap so typed text is not lost.
  useEffect(() => {
    const dirty = title !== savedTitleRef.current;
    if (dirty) return;
    setTitle(event.title);
    savedTitleRef.current = event.title;
  }, [event.id, event.title]);

  useEffect(() => {
    if (focusTitle) {
      titleRef.current?.focus();
      titleRef.current?.select();
    }
  }, [focusTitle, event.id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

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

  const form = (
    <InspectorForm
      event={event}
      calendar={calendar}
      title={title}
      setTitle={setTitle}
      titleRef={titleRef}
      commitTitle={commitTitle}
      onClose={onClose}
      onDelete={onDelete}
      isSaving={isSaving}
      isDeleting={isDeleting}
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
            }
          }}
          data-event-inspector
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

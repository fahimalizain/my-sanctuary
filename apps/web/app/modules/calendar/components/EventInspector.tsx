import { useEffect, useRef, useState } from 'react';
import { Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import type { CalendarEvent, GoogleCalendar } from '@/app/types';
import { cn } from '@/lib/utils';
import { formatEventTimeRange } from '../lib/week-layout';

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
}

/**
 * Side panel for inspecting / renaming / deleting a calendar event.
 * Desktop: 320px right column. Mobile: overlay drawer over the grid.
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
}: EventInspectorProps) {
  const [title, setTitle] = useState(event.title);
  const titleRef = useRef<HTMLInputElement>(null);
  // Track the last-saved title so blur after an unchanged edit is a no-op.
  const savedTitleRef = useRef(event.title);

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

  const start = new Date(event.start_time);
  const end = new Date(event.end_time);
  const timeLabel = formatEventTimeRange(start, end);
  const calendarLabel = calendar?.summary || 'Calendar';
  const description = event.description?.trim() ?? '';

  return (
    <aside
      className={cn(
        // Mobile: overlay drawer. Desktop (md+): in-flow right column.
        'absolute inset-y-0 right-0 z-40 flex w-[min(320px,100%)] flex-col border-l border-border bg-cream',
        'md:static md:z-auto md:w-80 md:shrink-0',
      )}
      data-event-inspector
      aria-label="Event details"
    >
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
    </aside>
  );
}

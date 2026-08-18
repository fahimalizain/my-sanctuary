import type { GoogleCalendar } from '@/app/types';
import { cn } from '@/lib/utils';

export interface CalendarPickerProps {
  /** The selected `google_calendar_id`, or '' for none. */
  value: string;
  onChange: (googleCalendarId: string) => void;
  calendars: GoogleCalendar[];
  isLoading?: boolean;
  error?: string | null;
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  /** `sm` for the compact pattern rows. */
  size?: 'default' | 'sm';
  'aria-label'?: string;
}

function calendarLabel(calendar: GoogleCalendar): string {
  let label =
    calendar.summary.length > 0
      ? calendar.summary
      : calendar.google_calendar_id;
  if (calendar.is_primary) label += ' (primary)';
  if (calendar.access_role !== 'owner' && calendar.access_role !== 'writer') {
    label += ' (read-only)';
  }
  return label;
}

/**
 * Presentational Google Calendar picker (native `<select>` — it paints
 * outside `overflow-hidden` containers and needs no custom popover state).
 * Does not fetch: the page owns the calendars list and passes the same
 * arrays/flags into every instance so one request feeds all pickers.
 */
export function CalendarPicker({
  value,
  onChange,
  calendars,
  isLoading = false,
  error = null,
  placeholder = 'None',
  disabled = false,
  id,
  size = 'default',
  'aria-label': ariaLabel,
}: CalendarPickerProps) {
  // Editing an existing category may carry a google_calendar_id that no
  // longer exists in the calendar list (deleted/revoked). Keep it as an
  // option so the value survives the round-trip instead of silently
  // resolving to None.
  const hasStaleValue =
    value.length > 0 &&
    !calendars.some((calendar) => calendar.google_calendar_id === value);

  return (
    <div>
      <select
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={isLoading || disabled}
        aria-label={ariaLabel}
        className={cn(
          'w-full border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all disabled:opacity-50',
          size === 'default'
            ? 'px-4 py-3 rounded-xl'
            : 'px-3 py-2 rounded-lg text-xs',
        )}
      >
        {/* Empty-value option keeps the field optional; while the first
            fetch is in flight it doubles as the loading row. */}
        <option value="">
          {isLoading && calendars.length === 0
            ? 'Loading calendars…'
            : placeholder}
        </option>
        {calendars.map((calendar) => (
          <option
            key={calendar.google_calendar_id}
            value={calendar.google_calendar_id}
          >
            {calendarLabel(calendar)}
          </option>
        ))}
        {hasStaleValue && <option value={value}>{value}</option>}
      </select>
      {error && <p className="mt-1 text-xs text-destructive">{error}</p>}
    </div>
  );
}

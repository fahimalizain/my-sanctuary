// In-memory calendar clipboard: the copied/cut payload plus the pure rules
// that turn it into a create body. No React and no `navigator.clipboard` —
// the page holds one item in state, so Cmd+C never touches the OS clipboard.

import type {
  CalendarEvent,
  GoogleCalendar,
  NewCalendarEventInput,
} from '@/app/types';
import { isCalendarWritable } from '../components/EventInspector';
import { isPersistableDraftTitle } from './event-draft';
import { defaultWritableCalendar } from './week-layout';

/**
 * One copied event, flattened to the fields a paste needs plus the display
 * color. Recurrence is deliberately absent: a clone is a one-shot at this
 * occurrence's times, never a second recurring series.
 */
export interface CalendarClipboardItem {
  /** Trimmed, non-empty (blank titles never make it into an item). */
  summary: string;
  /** Trimmed; may be empty. */
  description: string;
  /** Source `start_time`, unchanged. */
  start: string;
  /** Source `end_time`, unchanged. */
  end: string;
  isAllDay: boolean;
  /** Trimmed `start_time_zone`, else `end_time_zone`, else ''. */
  timeZone: string;
  calendarId: string;
  /** Display only — never sent on create; '' when the source has none. */
  color: string;
  /** Set by the caller for a cut: the persisted id to delete on paste. */
  cutSourceId: string | null;
}

/**
 * Flatten a selected event into a clipboard item, or null when there is
 * nothing copyable (no event, or a draft whose title was never typed).
 * `cutSourceId` starts null — only a cut fills it in.
 */
export function clipboardItemFromEvent(
  event: CalendarEvent | null,
): CalendarClipboardItem | null {
  if (!event) return null;
  const summary = event.title.trim();
  if (!isPersistableDraftTitle(summary)) return null;

  return {
    summary,
    description: (event.description ?? '').trim(),
    start: event.start_time,
    end: event.end_time,
    isAllDay: Boolean(event.is_all_day),
    timeZone:
      (event.start_time_zone ?? '').trim() ||
      (event.end_time_zone ?? '').trim(),
    calendarId: event.calendar_id,
    color: (event.color ?? '').trim(),
    cutSourceId: null,
  };
}

/**
 * Calendar a paste should land in: the source calendar when the user can
 * write to it, otherwise the default writable calendar.
 *
 * A source id that is not in the list is NOT writable — `isCalendarWritable`
 * treats an undefined calendar as writable, which here would paste into a
 * calendar that is missing from the sidebar (filtered or deleted).
 */
export function pasteDestinationCalendarId(
  sourceCalendarId: string,
  calendars: GoogleCalendar[],
): string | null {
  const source = calendars.find((c) => c.id === sourceCalendarId);
  if (source && isCalendarWritable(source)) return source.id;
  return defaultWritableCalendar(calendars)?.id ?? null;
}

/**
 * POST body for a paste. Times are the source's own — a paste is a second
 * one-shot at the same slot, never shifted to "today".
 *
 * Timed rows omit `is_all_day` (the API inserts a timed event when the flag
 * is absent) and carry `start_time_zone` only when the source had one.
 * All-day rows send `is_all_day: true` and never a zone — they are civil
 * dates on both sides.
 *
 * `color_hex` is never sent: the server resolves the title's category color
 * itself (refreshing the calendar's label cache when empty). `item.color` is
 * for the optimistic chip only.
 */
export function pasteCreateInput(
  item: CalendarClipboardItem,
  calendarId: string,
): NewCalendarEventInput {
  const description = item.description.trim();
  const timeZone = item.timeZone.trim();

  return {
    calendar_id: calendarId,
    // A paste keeps its computed destination (source when writable, else the
    // default); the server must not reroute it by title category.
    calendar_id_explicit: true,
    summary: item.summary.trim(),
    start: item.start,
    end: item.end,
    ...(description ? { description } : {}),
    ...(item.isAllDay ? { is_all_day: true } : {}),
    ...(!item.isAllDay && timeZone ? { start_time_zone: timeZone } : {}),
  };
}

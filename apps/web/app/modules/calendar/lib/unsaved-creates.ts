// A create POST that failed leaves the user's event with no server row. The
// temp chip must keep painting every field it holds — including edits made
// while the POST was in flight — and it must survive the discard paths that
// belong to drafts (empty-cell click, Escape, another chip, a new draft):
// clearing it is exactly the data loss this state exists to prevent.
//
// A retry re-POSTs the event as it stands now through the same unified create
// path a draft or paste uses. React-free so the rules are unit-tested; the
// session owns the overlay paint, the selection and the POST itself.

import type { CalendarEvent, NewCalendarEventInput } from '@/app/types';
import { isPersistableDraftTitle } from './event-draft';

/** One local event whose create POST failed, keyed by its temp id. */
export interface UnsavedCreate {
  /** Temp id the chip keeps painting under; never reaches the server. */
  tempId: string;
  /**
   * Body the failed POST carried. Only its shape is reused by a retry: the
   * server already read these flags (all-day, zone, explicit calendar), and
   * the local event alone cannot tell whether its calendar was the user's
   * pick or the server's category default.
   */
  input: NewCalendarEventInput;
}

/**
 * The unsaved-create records. Mutable like the overlay queue: stable callbacks
 * read it synchronously, and React state only mirrors the ids for paint.
 */
export class UnsavedCreates {
  private byId = new Map<string, UnsavedCreate>();

  mark(record: UnsavedCreate): void {
    this.byId.set(record.tempId, record);
  }

  get(tempId: string): UnsavedCreate | undefined {
    return this.byId.get(tempId);
  }

  has(tempId: string): boolean {
    return this.byId.has(tempId);
  }

  /** Forget a settled / dropped create. True when there was a record. */
  clear(tempId: string): boolean {
    return this.byId.delete(tempId);
  }

  /** Ids in insertion (failure) order. */
  ids(): string[] {
    return Array.from(this.byId.keys());
  }

  /** Drop every record (account switch). */
  reset(): void {
    this.byId.clear();
  }
}

export interface RetryCreateInput {
  /** Body the failed POST carried: its flags, not its values. */
  failed: NewCalendarEventInput;
  /** Overlay event: every field the user has on screen right now. */
  local: CalendarEvent;
  /** Inspector calendar pick made since that POST was sent. */
  calendarPicked: boolean;
}

/**
 * POST body for a retry: the event as it stands now, with the failed body's
 * flags carried over.
 *
 * - Title, description, times and calendar come from `local`, so edits made
 *   while the create was unsaved travel with the retry. The success flush
 *   diffs against exactly this body, so nothing is re-patched.
 * - A paste (or a pick) keeps `calendar_id_explicit`: the server must not
 *   reroute a deliberate destination by title category. A draft that was
 *   never picked stays routable, exactly as its first POST was.
 * - All-day sends its flag and civil times, never a zone; a row flipped back
 *   to timed sends `is_all_day: false` so the server cannot keep the posted
 *   all-day shape.
 * - Null when there is nothing to create (blank title), the same gate
 *   `persistDraft` uses: an empty POST would fail again.
 */
export function createRetryInput({
  failed,
  local,
  calendarPicked,
}: RetryCreateInput): NewCalendarEventInput | null {
  const summary = local.title.trim();
  if (!isPersistableDraftTitle(summary)) return null;

  const allDay = local.is_all_day === true;
  const description = local.description ?? '';
  const zone = allDay ? '' : (local.start_time_zone ?? '').trim();
  const explicit = failed.calendar_id_explicit === true || calendarPicked;

  return {
    calendar_id: local.calendar_id,
    ...(explicit ? { calendar_id_explicit: true } : {}),
    summary,
    ...(description ? { description } : {}),
    start: local.start_time,
    end: local.end_time,
    ...(allDay
      ? { is_all_day: true }
      : failed.is_all_day
        ? { is_all_day: false }
        : {}),
    ...(zone ? { start_time_zone: zone } : {}),
  };
}

export interface RetryGate {
  /** Id hidden behind the undo toast, if any: only the toast owns it. */
  hiddenId: string | null;
  /** Ids whose retry POST is already in flight. */
  inFlight: ReadonlySet<string>;
}

/** A retry must not shadow an undo toast, nor double-POST one event. */
export function canRetryUnsavedCreate(id: string, gate: RetryGate): boolean {
  if (gate.hiddenId === id) return false;
  return !gate.inFlight.has(id);
}

/** Ids the banner's Retry may POST now, in failure order. */
export function retryableUnsavedIds(
  ids: readonly string[],
  gate: RetryGate,
): string[] {
  return ids.filter((id) => canRetryUnsavedCreate(id, gate));
}

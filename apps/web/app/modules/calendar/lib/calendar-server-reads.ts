// Read stamps let the local change layer tell a *server read* apart from its
// own write echo.
//
// A GET that starts before a PATCH but resolves after it carries data that
// predates the acknowledgment. Retiring local intent on that read (or painting
// it over the intent) is the gap-8 snap-back. So every events GET takes a stamp
// from a shared monotonic clock when it *starts*, and each row it produced
// remembers that stamp. Write acknowledgments tick the same clock, which makes
// "did this read start after the ack?" a plain integer comparison.
//
// Rows that were never part of a read — our own PATCH / create responses, which
// the provider writes into the cache — carry no stamp (0) and can never retire
// intent.

import type { CalendarEvent } from '@/app/types';

let clock = 0;

/** Next tick of the shared read / write-ack clock. */
export function nextCalendarStamp(): number {
  clock += 1;
  return clock;
}

const readStamps = new WeakMap<CalendarEvent, number>();

/** Remember which read produced each of its rows. */
export function stampCalendarServerRead(
  events: readonly CalendarEvent[],
  stamp: number,
): void {
  for (const event of events) readStamps.set(event, stamp);
}

/**
 * Stamp of the read that produced `event`, or 0 when the row did not come from
 * a read at all (a PATCH / create response cached by the provider).
 */
export function calendarServerReadStamp(event: CalendarEvent): number {
  return readStamps.get(event) ?? 0;
}

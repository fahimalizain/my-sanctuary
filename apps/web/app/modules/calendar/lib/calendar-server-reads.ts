// Read stamps let the local change layer tell a *server read* apart from its
// own write echo.
//
// A GET that starts before a PATCH but resolves after it carries data that
// predates the acknowledgment. Retiring local intent on that read (or painting
// it over the intent) is the gap-8 snap-back. So every events GET takes a stamp
// from a shared monotonic clock when it *starts*, and the window it belongs to
// remembers the stamp of the latest completed GET. Write acknowledgments tick
// the same clock, which makes "did this read start after the ack?" a plain
// integer comparison.
//
// Stamps live per events *window* rather than per row: the cache keeps row
// object identity across identical refetches (structural sharing), so a row
// is the wrong place to hang "which read produced me". The session passes the
// current window's stamp into `apply`, and the provider's own PATCH / create
// cache upserts never touch it.

let clock = 0;

/** Next tick of the shared read / write-ack clock. */
export function nextCalendarStamp(): number {
  clock += 1;
  return clock;
}

/** Stable key for one events window (the same inputs as its query key). */
export function calendarReadWindowKey(
  timeMin: string,
  timeMax: string,
): string {
  return `${timeMin}|${timeMax}`;
}

const windowStamps = new Map<string, number>();

/** Remember the start stamp of a GET that just completed for this window. */
export function stampCalendarServerReadWindow(
  timeMin: string,
  timeMax: string,
  stamp: number,
): void {
  const key = calendarReadWindowKey(timeMin, timeMax);
  // An older read that somehow resolves later must not lower the window: the
  // query layer cancels superseded fetches, but the ordering is cheap to keep.
  if (stamp <= (windowStamps.get(key) ?? 0)) return;
  windowStamps.set(key, stamp);
}

/**
 * Start stamp of the latest completed GET for this window, or 0 when the
 * window has not been read yet (nothing may retire on it).
 */
export function calendarServerReadStamp(
  timeMin: string,
  timeMax: string,
): number {
  return windowStamps.get(calendarReadWindowKey(timeMin, timeMax)) ?? 0;
}

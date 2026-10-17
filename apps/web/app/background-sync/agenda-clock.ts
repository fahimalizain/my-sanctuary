/** Milliseconds until the next midnight in `timeZone` (an IANA name), read
 *  with `hourCycle: 'h23'` so midnight is 00 and never 24. `null` when the
 *  zone is missing or unreadable (`Intl` throws on an unknown zone) — the
 *  caller then skips the timer rather than crashing. `now` is injectable so
 *  tests can pin the clock. */
export function msUntilNextMidnight(
  timeZone: string,
  now: Date = new Date(),
): number | null {
  if (!timeZone) return null;
  try {
    const parts = new Intl.DateTimeFormat('en-US', {
      timeZone,
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hourCycle: 'h23',
    }).formatToParts(now);
    const part = (type: string) =>
      Number(parts.find((entry) => entry.type === type)?.value);
    const hour = part('hour') % 24;
    const minute = part('minute');
    const second = part('second');
    if (![hour, minute, second].every(Number.isFinite)) return null;
    const msIntoDay =
      ((hour * 60 + minute) * 60 + second) * 1000 + now.getMilliseconds();
    return 24 * 60 * 60 * 1000 - msIntoDay;
  } catch {
    return null;
  }
}

/** A fast client clock must not refetch while the server is still on
 *  yesterday, so the midnight timer fires this long after the boundary. */
export const AGENDA_MIDNIGHT_SKEW_MS = 30_000;

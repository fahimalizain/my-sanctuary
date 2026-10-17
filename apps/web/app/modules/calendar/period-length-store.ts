// Calendar period length (1–7) — module-level, not React context and not query
// data: it is a UI preference the day strip and the background events observer
// both read, and the observer has to follow it without the router re-rendering
// or the other session observers being torn down. Listeners fire only on a real
// change, so a repeated set never wakes either reader.
//
// localStorage backs the value but is read on use, never at import: this module
// can be evaluated before a `window` exists (a static import chain from the
// strip, or a test that installs jsdom afterwards). An in-session set keeps the
// new value authoritative even when the write fails (private mode / quota), so
// the UI still changes for this session; the next load reads storage again.

import {
  DAYS_PER_PERIOD,
  PERIOD_LENGTH_STORAGE_KEY,
  clampPeriodLength,
  parseStoredPeriodLength,
} from './lib/week-layout';

/** Value set in this session; null while the stored one is still the truth. */
let override: number | null = null;
const listeners = new Set<() => void>();

function readStoredPeriodLength(): number {
  if (typeof window === 'undefined') return DAYS_PER_PERIOD;
  try {
    return (
      parseStoredPeriodLength(
        window.localStorage.getItem(PERIOD_LENGTH_STORAGE_KEY),
      ) ?? DAYS_PER_PERIOD
    );
  } catch {
    return DAYS_PER_PERIOD;
  }
}

export function getPeriodLength(): number {
  return override ?? readStoredPeriodLength();
}

export function setPeriodLength(n: number): void {
  const next = clampPeriodLength(n);
  // Unchanged: no notification, and no rewrite of the stored value.
  if (next === getPeriodLength()) return;
  override = next;
  try {
    window.localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, String(next));
  } catch {
    // private mode / quota — in-session change still applies
  }
  for (const listener of listeners) listener();
}

export function subscribePeriodLength(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

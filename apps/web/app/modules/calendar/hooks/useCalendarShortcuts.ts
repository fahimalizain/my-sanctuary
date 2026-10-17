import { useEffect, useRef } from 'react';
import {
  isTextEditingTarget,
  resolveCalendarShortcut,
} from '../lib/calendar-shortcuts';

export interface CalendarShortcutsOptions {
  /** Suppress every shortcut, e.g. while a modal dialog owns the keyboard. */
  suspended: boolean;
  onPeriodChange: (n: number) => void;
  onToday: () => void;
  onShiftPeriod: (delta: -1 | 1) => void;
}

interface CalendarShortcutHandlers {
  onPeriodChange: (n: number) => void;
  onToday: () => void;
  onShiftPeriod: (delta: -1 | 1) => void;
}

/**
 * Window-level calendar navigation shortcuts: period length, Today, and the
 * previous/next period. One listener for the whole page — the strip hook no
 * longer listens on its own — so each keydown is resolved exactly once.
 */
export function useCalendarShortcuts({
  suspended,
  onPeriodChange,
  onToday,
  onShiftPeriod,
}: CalendarShortcutsOptions): void {
  // Latest callbacks, so the window listener rebinds only when suspension
  // changes rather than on every drag / selection re-render.
  const handlersRef = useRef<CalendarShortcutHandlers>({
    onPeriodChange,
    onToday,
    onShiftPeriod,
  });

  useEffect(() => {
    handlersRef.current = { onPeriodChange, onToday, onShiftPeriod };
  }, [onPeriodChange, onToday, onShiftPeriod]);

  useEffect(() => {
    // A dialog owns the keyboard while it is open: its focused buttons are not
    // editing targets, so the target check alone would let keys through.
    if (suspended) return;

    const onKeyDown = (e: KeyboardEvent) => {
      // A field keeps its own keystrokes, including the period digits.
      if (isTextEditingTarget(e.target)) return;

      const action = resolveCalendarShortcut({
        key: e.key,
        metaKey: e.metaKey,
        ctrlKey: e.ctrlKey,
        altKey: e.altKey,
        shiftKey: e.shiftKey,
        repeat: e.repeat,
      });
      // Not ours: leave the event completely alone (Cmd+T stays a new tab).
      if (!action) return;

      e.preventDefault();
      const handlers = handlersRef.current;
      if (action.type === 'setPeriod') handlers.onPeriodChange(action.length);
      else if (action.type === 'today') handlers.onToday();
      else handlers.onShiftPeriod(action.delta);
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [suspended]);
}

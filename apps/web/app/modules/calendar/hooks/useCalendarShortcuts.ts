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
  onCreate: () => void;
  /** Null when nothing deletable is selected (no selection or read-only). */
  onRequestDelete: (() => void) | null;
}

interface CalendarShortcutHandlers {
  onPeriodChange: (n: number) => void;
  onToday: () => void;
  onShiftPeriod: (delta: -1 | 1) => void;
  onCreate: () => void;
  onRequestDelete: (() => void) | null;
}

/**
 * Window-level calendar shortcuts: period length, Today, previous/next
 * period, create, and request-delete. One listener for the whole page — the
 * strip hook no longer listens on its own — so each keydown is resolved
 * exactly once.
 */
export function useCalendarShortcuts({
  suspended,
  onPeriodChange,
  onToday,
  onShiftPeriod,
  onCreate,
  onRequestDelete,
}: CalendarShortcutsOptions): void {
  // Latest callbacks, so the window listener rebinds only when suspension
  // changes rather than on every drag / selection re-render.
  const handlersRef = useRef<CalendarShortcutHandlers>({
    onPeriodChange,
    onToday,
    onShiftPeriod,
    onCreate,
    onRequestDelete,
  });

  useEffect(() => {
    handlersRef.current = {
      onPeriodChange,
      onToday,
      onShiftPeriod,
      onCreate,
      onRequestDelete,
    };
  }, [onPeriodChange, onToday, onShiftPeriod, onCreate, onRequestDelete]);

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

      const handlers = handlersRef.current;
      switch (action.type) {
        case 'setPeriod':
          e.preventDefault();
          handlers.onPeriodChange(action.length);
          break;
        case 'today':
          e.preventDefault();
          handlers.onToday();
          break;
        case 'shiftPeriod':
          e.preventDefault();
          handlers.onShiftPeriod(action.delta);
          break;
        case 'create':
          e.preventDefault();
          handlers.onCreate();
          break;
        case 'requestDelete': {
          // Read-only or nothing selected: the key is not ours to consume.
          const requestDelete = handlers.onRequestDelete;
          if (!requestDelete) return;
          e.preventDefault();
          requestDelete();
          break;
        }
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [suspended]);
}

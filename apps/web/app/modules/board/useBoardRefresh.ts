import { useEffect, useRef } from 'react';
import {
  BOARD_REFRESH_INTERVAL_MS,
  shouldRefreshBoard,
} from './board-refresh';

/** Quietly refetches the board (lists → tasks+categories) every
 *  BOARD_REFRESH_INTERVAL_MS while mounted, and immediately when the tab
 *  becomes visible again. Visibility is `document.visibilitychange`, not a
 *  window focus listener — raw focus also fires when clicking from the
 *  address bar or DevTools and would over-fetch.
 *
 *  Never fires on mount — BoardPage's own mount `load()` is the first
 *  refresh (`lastRefreshAtRef` starts at "just now"). Hidden-tab ticks are
 *  cheap no-ops; the interval is not torn down on visibility changes.
 *  Busy boards (live drag, in-flight move/focus/load) skip the refresh via
 *  `isBusy`. */
export function useBoardRefresh(
  load: () => void,
  isBusy: () => boolean,
): void {
  // Latest `load`/`isBusy` without re-subscribing: written during render
  // (the same "latest value" pattern as BoardPage's listsRef/tasksRef) so
  // the single [] effect below always reads fresh callbacks.
  const loadRef = useRef(load);
  loadRef.current = load;
  const isBusyRef = useRef(isBusy);
  isBusyRef.current = isBusy;

  // The mount load fired by BoardPage counts as a just-completed refresh,
  // so a visibility blip right after mount cannot double-fetch.
  const lastRefreshAtRef = useRef(Date.now());

  useEffect(() => {
    const tryRefresh = (): void => {
      const ok = shouldRefreshBoard({
        now: Date.now(),
        lastRefreshAt: lastRefreshAtRef.current,
        visible: document.visibilityState === 'visible',
        busy: isBusyRef.current(),
      });
      if (!ok) return;
      lastRefreshAtRef.current = Date.now();
      loadRef.current();
    };

    const intervalId = setInterval(tryRefresh, BOARD_REFRESH_INTERVAL_MS);
    // Hidden→visible transitions only; a spurious event while already
    // visible is still safe — it passes through the cooldown gate.
    const onVisibility = (): void => {
      if (document.visibilityState === 'visible') tryRefresh();
    };
    document.addEventListener('visibilitychange', onVisibility);

    return () => {
      clearInterval(intervalId);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, []);
}

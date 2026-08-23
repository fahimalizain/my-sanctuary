// Quiet board refresh: a 60s interval tick plus a hidden→visible
// `visibilitychange` trigger, gated through this pure helper. This is polite
// polling, NOT a live subscription (no websocket/SSE; ADR 0002 has no push
// channel). The gate keeps refreshes quiet: skip when the tab is hidden,
// while the board is busy (live drag, in-flight move/focus/load), or within
// the cooldown of the last completed load so mount + interval + visibility
// never double-fetch.

/** Interval between background refresh ticks while the page is mounted. */
export const BOARD_REFRESH_INTERVAL_MS = 60_000;

/** Minimum gap after a refresh before another may fire — a visibility event
 *  right after mount or right after an interval tick must not double-fetch.
 *  The mount load counts as a just-completed refresh. */
export const BOARD_REFRESH_COOLDOWN_MS = 5_000;

export interface BoardRefreshGate {
  /** Current time (`Date.now()` at the call site). */
  now: number;
  /** When the last refresh fired (mount load included). */
  lastRefreshAt: number;
  /** `document.visibilityState === 'visible'`. */
  visible: boolean;
  /** A drag is live, a move/focus/load is in flight. */
  busy: boolean;
}

/** True only when the tab is visible, the board is not mid-drag or mid-
 *  mutation, and at least one cooldown has passed since the last refresh. */
export function shouldRefreshBoard(gate: BoardRefreshGate): boolean {
  if (!gate.visible || gate.busy) return false;
  return gate.now - gate.lastRefreshAt >= BOARD_REFRESH_COOLDOWN_MS;
}

/** Interval between background task refetches while the board is mounted
 *  and idle. Board passes this to `useTasksQuery({ refetchInterval })`
 *  and sets it to `false` while a drag / move / focus is in flight. */
export const BOARD_REFRESH_INTERVAL_MS = 60_000;

import { COL_HEADER_H } from './week-layout';

// Slack (px) around the 11 PM row's top edge. Hiding needs the row to be
// this far inside the viewport; once hidden it only comes back after the row
// has left by the same margin, so a scroll hovering at the edge can't flicker.
export const NAV_LATE_HOURS_HYSTERESIS_PX = 8;

const LAST_HOUR = 23;

/**
 * Whether the floating nav should fade out because the 11 PM hour row is on
 * screen. Row top inside the scroll content is
 * `COL_HEADER_H + allDayHeight + 23 * hourH`; the sticky header + all-day band
 * cover the top of the viewport, so a row scrolled fully under them doesn't
 * count either. Pass `currentlyHidden` to apply hysteresis.
 */
export function shouldHideNavForLateHours(args: {
  scrollTop: number;
  clientHeight: number;
  hourH: number;
  allDayHeight: number;
  currentlyHidden?: boolean;
  hysteresis?: number;
}): boolean {
  const { scrollTop, clientHeight, hourH, allDayHeight } = args;
  if (clientHeight <= 0 || hourH <= 0) return false;
  const slack = args.hysteresis ?? NAV_LATE_HOURS_HYSTERESIS_PX;
  // Hidden state is sticky: widen the "on screen" window by the slack.
  const margin = args.currentlyHidden ? slack : -slack;

  const stickyH = COL_HEADER_H + allDayHeight;
  const rowTop = stickyH + LAST_HOUR * hourH;
  const rowBottom = rowTop + hourH;
  const viewBottom = scrollTop + clientHeight;
  const stickyBottom = scrollTop + stickyH;

  return rowTop < viewBottom + margin && rowBottom > stickyBottom - margin;
}

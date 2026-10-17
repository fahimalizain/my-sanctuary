// Pure hour-density zoom helpers for the week time-grid calendar.
// No React. Deterministic.

import { HOUR_H_BASE, HOUR_H_MAX, HOUR_H_MIN } from './week-layout';

export const HOUR_H_STORAGE_KEY = 'sanctuary.calendar.hourH';

/** Clamp to [HOUR_H_MIN, HOUR_H_MAX]. Non-finite → HOUR_H_BASE. */
export function clampHourHeight(n: number): number {
  if (!Number.isFinite(n)) return HOUR_H_BASE;
  return Math.min(HOUR_H_MAX, Math.max(HOUR_H_MIN, n));
}

/**
 * Multiply current hourH by factor, then clamp.
 * Non-finite or non-positive factor → clampHourHeight(current).
 */
export function hourHAfterZoom(current: number, factor: number): number {
  if (!Number.isFinite(factor) || factor <= 0) {
    return clampHourHeight(current);
  }
  return clampHourHeight(current * factor);
}

/**
 * Time (hours, fractional) under a pointer / pinch midpoint at the scale
 * currently painted in the DOM.
 *   hours = (clientY - scrollerRectTop + scrollTop - headerOffset) / hourH
 * Deliberately unclamped: above the hours area this is a negative time, which
 * keeps the anchor math continuous instead of snapping to hour 0.
 * Non-finite inputs or non-positive hourH → 0.
 */
export function anchorHoursAt(
  clientY: number,
  scrollerRectTop: number,
  scrollTop: number,
  headerOffset: number,
  hourH: number,
): number {
  if (
    !Number.isFinite(clientY) ||
    !Number.isFinite(scrollerRectTop) ||
    !Number.isFinite(scrollTop) ||
    !Number.isFinite(headerOffset) ||
    !Number.isFinite(hourH) ||
    hourH <= 0
  ) {
    return 0;
  }
  return (
    yInHoursArea(clientY, scrollerRectTop, scrollTop, headerOffset) / hourH
  );
}

/**
 * Absolute scroller.scrollTop that puts `anchorHours` back under `viewportY`
 * (px from the scroller's top edge) once the hours area is laid out at `hourH`.
 * Applied after the commit that changes hourH, so the DOM height matches:
 *   scrollTop = headerOffset + anchorHours * hourH - viewportY
 * Clamped at >= 0 (the browser clamps the max). Non-finite inputs → 0.
 */
export function scrollTopForAnchor(
  anchorHours: number,
  hourH: number,
  headerOffset: number,
  viewportY: number,
): number {
  if (
    !Number.isFinite(anchorHours) ||
    !Number.isFinite(hourH) ||
    !Number.isFinite(headerOffset) ||
    !Number.isFinite(viewportY)
  ) {
    return 0;
  }
  return Math.max(0, headerOffset + anchorHours * hourH - viewportY);
}

/**
 * Hour-label stride so 10px labels do not overlap when zoomed out.
 *   hourH >= 28 → 1
 *   hourH >= 20 → 2
 *   else        → 3
 */
export function hourLabelStep(hourH: number): number {
  if (hourH >= 28) return 1;
  if (hourH >= 20) return 2;
  return 3;
}

/**
 * Wheel → multiplicative zoom factor.
 * Convert delta to px: deltaMode 1 → *16, 2 → *400, else as-is.
 * Return Math.exp(-px * 0.003).
 * Negative deltaY zooms in (factor > 1).
 */
export function zoomFactorFromWheel(deltaY: number, deltaMode: number): number {
  let px = deltaY;
  if (deltaMode === 1) px *= 16;
  else if (deltaMode === 2) px *= 400;
  return Math.exp(-px * 0.003);
}

/**
 * Parse localStorage value. null / '' / non-finite → null.
 * Finite → clampHourHeight(n).
 */
export function parseStoredHourH(raw: string | null): number | null {
  if (raw == null || raw === '') return null;
  const n = Number(raw);
  if (!Number.isFinite(n)) return null;
  return clampHourHeight(n);
}

/**
 * Y offset (px) of a pointer inside the hours area.
 *   yInHours = clientY - scrollerRectTop + scrollTop - headerOffset
 */
export function yInHoursArea(
  clientY: number,
  scrollerRectTop: number,
  scrollTop: number,
  headerOffset: number,
): number {
  return clientY - scrollerRectTop + scrollTop - headerOffset;
}

/** Euclidean distance between two points. Non-finite coords → 0. */
export function pinchDistance(
  a: { x: number; y: number },
  b: { x: number; y: number },
): number {
  if (
    !Number.isFinite(a.x) ||
    !Number.isFinite(a.y) ||
    !Number.isFinite(b.x) ||
    !Number.isFinite(b.y)
  ) {
    return 0;
  }
  return Math.hypot(b.x - a.x, b.y - a.y);
}

/** Midpoint Y of two points. Non-finite → 0. */
export function pinchMidpointY(a: { y: number }, b: { y: number }): number {
  if (!Number.isFinite(a.y) || !Number.isFinite(b.y)) return 0;
  return (a.y + b.y) / 2;
}

/**
 * currentDist / originDist.
 * Non-finite or originDist <= 0 or currentDist <= 0 → 1.
 */
export function pinchScale(originDist: number, currentDist: number): number {
  if (
    !Number.isFinite(originDist) ||
    !Number.isFinite(currentDist) ||
    originDist <= 0 ||
    currentDist <= 0
  ) {
    return 1;
  }
  return currentDist / originDist;
}

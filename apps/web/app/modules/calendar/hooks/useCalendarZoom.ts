import { useLayoutEffect, useRef, useState, type RefObject } from 'react';
import {
  HOUR_H_STORAGE_KEY,
  hourHAfterZoom,
  parseStoredHourH,
  pinchDistance,
  pinchMidpointY,
  pinchScale,
  scrollDeltaForHourZoom,
  yInHoursArea,
  zoomFactorFromWheel,
} from '../lib/calendar-zoom';

export function useCalendarZoom(options: {
  scrollerRef: RefObject<HTMLElement | null>;
  autoHourH: number;
  headerOffset: number;
  /** Called when a 2-finger pinch begins. Use to cancel drag + lock strip. */
  onPinchStartRef?: RefObject<(() => void) | null>;
  /** Called when pinch ends (pointers < 2). Unlock strip. */
  onPinchEndRef?: RefObject<(() => void) | null>;
}): { hourH: number } {
  const {
    scrollerRef,
    autoHourH,
    headerOffset,
    onPinchStartRef,
    onPinchEndRef,
  } = options;

  const [userHourH, setUserHourH] = useState<number | null>(() => {
    if (typeof window === 'undefined') return null;
    try {
      return parseStoredHourH(localStorage.getItem(HOUR_H_STORAGE_KEY));
    } catch {
      return null;
    }
  });

  const hourH = userHourH ?? autoHourH;

  // Latest values for stable listeners (registered once per scroller el).
  const hourHRef = useRef(hourH);
  const headerOffsetRef = useRef(headerOffset);
  hourHRef.current = hourH;
  headerOffsetRef.current = headerOffset;

  // Re-bind when the scroller element appears (may be null on first paint).
  // Parent re-renders after measure attach the ref; layout effect re-runs.
  const scrollerEl = scrollerRef.current;

  useLayoutEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;

    const applyZoom = (
      nextHourH: number,
      clientY: number,
      scroller: HTMLElement,
    ) => {
      const current = hourHRef.current;
      if (nextHourH === current) return;

      const rect = scroller.getBoundingClientRect();
      let y = yInHoursArea(
        clientY,
        rect.top,
        scroller.scrollTop,
        headerOffsetRef.current,
      );
      if (y < 0) y = 0;

      const delta = scrollDeltaForHourZoom(current, nextHourH, y);
      scroller.scrollTop += delta;

      // Keep ref in sync before React re-renders so rapid pinch moves
      // compute incremental scroll deltas from the true current hourH.
      hourHRef.current = nextHourH;
      setUserHourH(nextHourH);
      try {
        localStorage.setItem(HOUR_H_STORAGE_KEY, String(nextHourH));
      } catch {
        // private mode / quota — in-session zoom still applies
      }
    };

    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();

      const current = hourHRef.current;
      const factor = zoomFactorFromWheel(e.deltaY, e.deltaMode);
      const next = hourHAfterZoom(current, factor);
      applyZoom(next, e.clientY, el);
    };

    // ── Two-pointer pinch ───────────────────────────────────────────────
    type Ptr = { x: number; y: number };
    const pointers = new Map<number, Ptr>();
    let pinchOrigin: { originHourH: number; originDist: number } | null = null;
    let pinchActive = false;

    const endPinchIfNeeded = () => {
      if (!pinchActive) return;
      pinchActive = false;
      pinchOrigin = null;
      onPinchEndRef?.current?.();
    };

    // Move/up/cancel on window so pinch survives fingers sliding off the
    // scroller (sidebar, header chrome, past the edge). pointerdown stays
    // on the scroller so we only track fingers that land on the calendar.
    // Do not use setPointerCapture — it synthesizes leave/out and fights
    // the existing drag capture.
    let windowBound = false;

    const onPointerMove = (e: PointerEvent) => {
      if (!pointers.has(e.pointerId)) return;
      pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });

      // preventDefault only while pinching — one-finger moves must scroll.
      if (pointers.size !== 2 || !pinchOrigin) return;

      e.preventDefault();

      const pts = [...pointers.values()];
      const factor = pinchScale(
        pinchOrigin.originDist,
        pinchDistance(pts[0], pts[1]),
      );
      const next = hourHAfterZoom(pinchOrigin.originHourH, factor);
      const midY = pinchMidpointY(pts[0], pts[1]);
      applyZoom(next, midY, el);
    };

    const onPointerEnd = (e: PointerEvent) => {
      if (!pointers.has(e.pointerId)) return;
      pointers.delete(e.pointerId);
      if (pointers.size < 2) endPinchIfNeeded();
      if (pointers.size === 0) unbindWindow();
    };

    const bindWindow = () => {
      if (windowBound) return;
      windowBound = true;
      window.addEventListener('pointermove', onPointerMove, {
        capture: true,
        passive: false,
      });
      window.addEventListener('pointerup', onPointerEnd, true);
      window.addEventListener('pointercancel', onPointerEnd, true);
    };

    const unbindWindow = () => {
      if (!windowBound) return;
      windowBound = false;
      window.removeEventListener('pointermove', onPointerMove, true);
      window.removeEventListener('pointerup', onPointerEnd, true);
      window.removeEventListener('pointercancel', onPointerEnd, true);
    };

    const onPointerDown = (e: PointerEvent) => {
      const wasEmpty = pointers.size === 0;
      pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (wasEmpty) bindWindow();
      if (pointers.size === 2 && !pinchActive) {
        const pts = [...pointers.values()];
        const originDist = pinchDistance(pts[0], pts[1]);
        pinchOrigin = {
          originHourH: hourHRef.current,
          originDist,
        };
        pinchActive = true;
        onPinchStartRef?.current?.();
      }
    };

    // Safari page-zoom — block only; Mac trackpad already zooms via Ctrl+wheel.
    const killGesture = (e: Event) => e.preventDefault();

    el.addEventListener('wheel', onWheel, { passive: false });
    el.addEventListener('pointerdown', onPointerDown, true);
    el.addEventListener('gesturestart', killGesture, { passive: false });
    el.addEventListener('gesturechange', killGesture, { passive: false });

    return () => {
      unbindWindow();
      el.removeEventListener('wheel', onWheel);
      el.removeEventListener('pointerdown', onPointerDown, true);
      el.removeEventListener('gesturestart', killGesture);
      el.removeEventListener('gesturechange', killGesture);
    };
  }, [scrollerRef, scrollerEl, onPinchStartRef, onPinchEndRef]);

  return { hourH };
}

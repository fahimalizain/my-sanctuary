import {
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from 'react';
import {
  HOUR_H_STORAGE_KEY,
  hourHAfterZoom,
  parseStoredHourH,
  scrollDeltaForHourZoom,
  yInHoursArea,
  zoomFactorFromWheel,
} from '../lib/calendar-zoom';

export function useCalendarZoom(options: {
  scrollerRef: RefObject<HTMLElement | null>;
  autoHourH: number;
  headerOffset: number;
}): { hourH: number } {
  const { scrollerRef, autoHourH, headerOffset } = options;

  const [userHourH, setUserHourH] = useState<number | null>(() => {
    if (typeof window === 'undefined') return null;
    try {
      return parseStoredHourH(localStorage.getItem(HOUR_H_STORAGE_KEY));
    } catch {
      return null;
    }
  });

  const hourH = userHourH ?? autoHourH;

  // Latest values for a stable wheel listener (registered once per scroller el).
  const hourHRef = useRef(hourH);
  const headerOffsetRef = useRef(headerOffset);
  const autoHourHRef = useRef(autoHourH);
  hourHRef.current = hourH;
  headerOffsetRef.current = headerOffset;
  autoHourHRef.current = autoHourH;

  // Re-bind when the scroller element appears (may be null on first paint).
  // Parent re-renders after measure attach the ref; layout effect re-runs.
  const scrollerEl = scrollerRef.current;

  useLayoutEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;

    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();

      const current = hourHRef.current;
      const factor = zoomFactorFromWheel(e.deltaY, e.deltaMode);
      const next = hourHAfterZoom(current, factor);
      if (next === current) return;

      const rect = el.getBoundingClientRect();
      let y = yInHoursArea(
        e.clientY,
        rect.top,
        el.scrollTop,
        headerOffsetRef.current,
      );
      if (y < 0) y = 0;

      const delta = scrollDeltaForHourZoom(current, next, y);
      el.scrollTop += delta;

      setUserHourH(next);
      try {
        localStorage.setItem(HOUR_H_STORAGE_KEY, String(next));
      } catch {
        // private mode / quota — in-session zoom still applies
      }
    };

    el.addEventListener('wheel', onWheel, { passive: false });
    return () => {
      el.removeEventListener('wheel', onWheel);
    };
  }, [scrollerRef, scrollerEl]);

  return { hourH };
}

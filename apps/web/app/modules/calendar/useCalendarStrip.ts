import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from 'react';
import {
  DAYS_PER_PERIOD,
  STRIP_OVERSCAN,
  addDays,
  clampPeriodLength,
  colWidth as computeColWidth,
  fitPeriodStart,
  formatDayRangeTitle,
  gutterWithRemainder,
  isSameDay,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfDay,
  stripDayCount,
  visibleStartIndex as computeVisibleStartIndex,
} from './week-layout';

export interface CalendarStrip {
  periodLength: number;
  handlePeriodChange: (n: number) => void;
  visibleStart: Date;
  visibleEnd: Date;
  rangeTitle: string;
  today: Date;
  days: Date[];
  dayCount: number;
  range: { timeMin: string; timeMax: string };
  gridColumnRef: RefObject<HTMLDivElement | null>;
  scrollerRef: RefObject<HTMLDivElement | null>;
  scrollerHeight: number;
  colW: number;
  gutterW: number;
  trackWidth: number;
  contentWidth: number;
  setStripLocked: (locked: boolean) => void;
  onScrollerScroll: () => void;
  shiftPeriod: (deltaPeriods: number) => void;
  goToToday: () => void;
  goToDate: (date: Date) => void;
  /** Read/cleared by the page’s now-line vertical scroll effect. */
  shouldScrollToNowRef: RefObject<boolean>;
}

/**
 * Infinite horizontal day strip: period length, measure, scroll/rebase, and
 * navigation. Owns refs + layout effects so CalendarPage can stay focused on
 * events / drag / inspector.
 */
export function useCalendarStrip(): CalendarStrip {
  // Visible day-column count (1–7). Session-only; not persisted.
  const [periodLength, setPeriodLength] = useState(DAYS_PER_PERIOD);

  // Strip window: first *rendered* day. Visible period is windowStart + scroll offset.
  // Initial: overscan before current Mon–Sun so current week is centered in the buffer.
  const [windowStart, setWindowStart] = useState(() =>
    addDays(fitPeriodStart(new Date(), DAYS_PER_PERIOD), -STRIP_OVERSCAN),
  );

  // First visible day index into the rendered strip (0 … dayCount-periodLength).
  // Seeded at overscan so the current period shows before measure/scroll attach.
  const [visibleStartIdx, setVisibleStartIdx] = useState(STRIP_OVERSCAN);

  const dayCount = stripDayCount(periodLength);
  const range = useMemo(
    () => rangeIso(windowStart, dayCount),
    [windowStart, dayCount],
  );

  // Full rendered strip (period + overscan each side).
  const days = useMemo(() => {
    const origin = startOfDay(windowStart);
    return Array.from({ length: dayCount }, (_, i) => addDays(origin, i));
  }, [windowStart, dayCount]);

  // First day of the visible period (drives title + sidebar highlight).
  const visibleStart = useMemo(
    () => addDays(startOfDay(windowStart), visibleStartIdx),
    [windowStart, visibleStartIdx],
  );
  const visibleEnd = useMemo(
    () => addDays(visibleStart, periodLength - 1),
    [visibleStart, periodLength],
  );
  const rangeTitle = useMemo(
    () => formatDayRangeTitle(visibleStart, visibleEnd),
    [visibleStart, visibleEnd],
  );

  const today = useMemo(() => {
    const now = new Date();
    return new Date(now.getFullYear(), now.getMonth(), now.getDate());
  }, []);

  // Measure the grid column (not the scroller) for colWidth / gutter remainder.
  const gridColumnRef = useRef<HTMLDivElement>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const [mainWidth, setMainWidth] = useState(0);
  const [scrollerHeight, setScrollerHeight] = useState(0);

  // Scroll-to-now only once per mount / when jumping to Today.
  const shouldScrollToNowRef = useRef(true);
  // Pending horizontal scroll position after programmatic window moves.
  const pendingScrollLeftRef = useRef<number | null>(null);
  // Seed initial H-scroll once colWidth is known.
  const didInitScrollRef = useRef(false);
  // Rebase direction requested by the scroll handler; applied in layout effect.
  const pendingRebaseRef = useRef<-1 | 0 | 1>(0);
  const [scrollNonce, setScrollNonce] = useState(0);
  // Suppress rebase while applying a programmatic scrollLeft write.
  const suppressRebaseRef = useRef(false);

  useLayoutEffect(() => {
    const el = gridColumnRef.current;
    if (!el) return;

    const measure = () => {
      setMainWidth(el.clientWidth);
      const scroller = scrollerRef.current;
      if (scroller) setScrollerHeight(scroller.clientHeight);
    };
    measure();

    const ro = new ResizeObserver(measure);
    ro.observe(el);
    const scroller = scrollerRef.current;
    if (scroller) ro.observe(scroller);
    return () => ro.disconnect();
  }, []);

  const colW = useMemo(
    () => computeColWidth(mainWidth, periodLength),
    [mainWidth, periodLength],
  );
  const gutterW = useMemo(
    () => gutterWithRemainder(mainWidth, colW, periodLength),
    [mainWidth, colW, periodLength],
  );
  const trackWidth = dayCount * colW;
  const contentWidth = gutterW + trackWidth;

  const setStripLocked = useCallback((locked: boolean) => {
    suppressRebaseRef.current = locked;
  }, []);

  const releaseSuppressRebase = () => {
    // scroll events from programmatic scrollLeft can be sync or rAF-deferred.
    requestAnimationFrame(() => {
      suppressRebaseRef.current = false;
    });
  };

  // Apply pending horizontal scroll (init / Today / mini-month) and rebase.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    // Rebase first: shift window ±periodLength days and compensate scrollLeft
    // so the picture does not jump. Must run before paint.
    const dir = pendingRebaseRef.current;
    if (dir !== 0) {
      pendingRebaseRef.current = 0;
      const compensation = -dir * periodLength * colW;
      suppressRebaseRef.current = true;
      setWindowStart((prev) =>
        shiftWindowStart(startOfDay(prev), dir, periodLength),
      );
      scroller.scrollLeft = scroller.scrollLeft + compensation;
      setVisibleStartIdx(
        computeVisibleStartIndex(
          scroller.scrollLeft,
          colW,
          dayCount,
          periodLength,
        ),
      );
      releaseSuppressRebase();
      return;
    }

    // Initial mount: park scroll so current period is visible.
    if (!didInitScrollRef.current) {
      didInitScrollRef.current = true;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }

    // Programmatic jump (Today / mini-month / period change): park at overscan.
    if (pendingScrollLeftRef.current !== null) {
      pendingScrollLeftRef.current = null;
      suppressRebaseRef.current = true;
      scroller.scrollLeft = scrollLeftForIndex(STRIP_OVERSCAN, colW);
      setVisibleStartIdx(STRIP_OVERSCAN);
      releaseSuppressRebase();
    }
  }, [colW, dayCount, periodLength, windowStart, scrollNonce]);

  // Horizontal scroll: update visible start; request rebase near the edges.
  const onScrollerScroll = useCallback(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    const idx = computeVisibleStartIndex(
      scroller.scrollLeft,
      colW,
      dayCount,
      periodLength,
    );
    setVisibleStartIdx((prev) => (prev === idx ? prev : idx));

    if (suppressRebaseRef.current) return;

    const dir = shouldRebase(idx, dayCount, periodLength);
    if (dir !== 0 && pendingRebaseRef.current === 0) {
      pendingRebaseRef.current = dir;
      setScrollNonce((n) => n + 1);
    }
  }, [colW, dayCount, periodLength]);

  /** Park the strip so `firstVisible` is the left edge of the viewport. */
  const jumpToVisibleStart = useCallback(
    (firstVisible: Date, scrollToNow: boolean) => {
      shouldScrollToNowRef.current = scrollToNow;
      pendingRebaseRef.current = 0;
      const origin = addDays(startOfDay(firstVisible), -STRIP_OVERSCAN);
      setWindowStart(origin);
      setVisibleStartIdx(STRIP_OVERSCAN);
      // Layout effect parks scrollLeft at overscan once colW is known.
      pendingScrollLeftRef.current = 0; // non-null sentinel
      setScrollNonce((n) => n + 1);
    },
    [],
  );

  const shiftPeriod = useCallback(
    (deltaPeriods: number) => {
      shouldScrollToNowRef.current = false;
      const scroller = scrollerRef.current;
      if (!scroller || colW <= 0) return;
      scroller.scrollLeft += deltaPeriods * periodLength * colW;
      // onScroll will update visibleStartIdx and rebase if needed.
      onScrollerScroll();
    },
    [colW, periodLength, onScrollerScroll],
  );

  const goToToday = useCallback(() => {
    jumpToVisibleStart(fitPeriodStart(new Date(), periodLength), true);
  }, [jumpToVisibleStart, periodLength]);

  const goToDate = useCallback(
    (date: Date) => {
      // fitPeriodStart Monday-snaps for 5–7 day views; shorter views land on the day.
      jumpToVisibleStart(
        fitPeriodStart(date, periodLength),
        isSameDay(date, today),
      );
    },
    [jumpToVisibleStart, periodLength, today],
  );

  const handlePeriodChange = useCallback(
    (n: number) => {
      const next = clampPeriodLength(n);
      setPeriodLength(next);
      const nextStart = fitPeriodStart(visibleStart, next);
      const nextDays = Array.from({ length: next }, (_, i) =>
        addDays(nextStart, i),
      );
      const containsToday = nextDays.some((d) => isSameDay(d, today));
      jumpToVisibleStart(nextStart, containsToday);
    },
    [visibleStart, today, jumpToVisibleStart],
  );

  // Notion-style period shortcuts (ignore when typing in a field).
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const t = e.target;
      if (t instanceof HTMLElement) {
        const tag = t.tagName;
        if (
          tag === 'INPUT' ||
          tag === 'TEXTAREA' ||
          tag === 'SELECT' ||
          t.isContentEditable
        ) {
          return;
        }
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      const key = e.key;
      let next: number | null = null;
      if (key === '1' || key === 'd' || key === 'D') next = 1;
      else if (key === 'w' || key === 'W' || key === '0') next = 7;
      else if (key >= '2' && key <= '6') next = Number(key);

      if (next === null) return;
      e.preventDefault();
      handlePeriodChange(next);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [handlePeriodChange]);

  return {
    periodLength,
    handlePeriodChange,
    visibleStart,
    visibleEnd,
    rangeTitle,
    today,
    days,
    dayCount,
    range,
    gridColumnRef,
    scrollerRef,
    scrollerHeight,
    colW,
    gutterW,
    trackWidth,
    contentWidth,
    setStripLocked,
    onScrollerScroll,
    shiftPeriod,
    goToToday,
    goToDate,
    shouldScrollToNowRef,
  };
}

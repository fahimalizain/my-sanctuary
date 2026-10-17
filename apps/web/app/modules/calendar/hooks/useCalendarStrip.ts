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
  EMPTY_WHEEL_BURST,
  PERIOD_LENGTH_STORAGE_KEY,
  STRIP_OVERSCAN,
  addDays,
  clampPeriodLength,
  classifyScrollAxis,
  colWidth as computeColWidth,
  fitPeriodStart,
  formatDayRangeTitle,
  gutterWithRemainder,
  isSameDay,
  isStripWidthMeasured,
  nextWheelBurst,
  parseStoredPeriodLength,
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfDay,
  stripDayCount,
  stripRebaseSuppressed,
  stripSnapDisabled,
  visibleStartIndex as computeVisibleStartIndex,
  wheelDeltaPx,
  type ScrollAxis,
  type StripLockReason,
  type WheelBurst,
} from '../lib/week-layout';

/**
 * Quiet period (ms) with no `scroll` event that counts a finger-up gesture as
 * settled. Fallback for `scrollend`, which only recent Safari ships.
 */
const AXIS_SETTLE_MS = 150;

/**
 * In-flight touch gesture, tracked to decide whether it may page the strip.
 * `left0` is the horizontal position the strip must return to when a vertical
 * gesture settles; null when no gesture holds one.
 */
interface AxisGesture {
  /** First finger position, or null while no gesture is tracked. */
  origin: { x: number; y: number } | null;
  /** Committed axis for this gesture; `pending` until the slop is crossed. */
  intent: ScrollAxis;
  /** Frozen `scrollLeft` for the gesture (see `classifyScrollAxis`). */
  left0: number | null;
}

function readStoredPeriodLength(): number {
  if (typeof window === 'undefined') return DAYS_PER_PERIOD;
  try {
    return (
      parseStoredPeriodLength(
        localStorage.getItem(PERIOD_LENGTH_STORAGE_KEY),
      ) ?? DAYS_PER_PERIOD
    );
  } catch {
    return DAYS_PER_PERIOD;
  }
}

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
  lockStrip: (reason: StripLockReason) => void;
  unlockStrip: (reason: StripLockReason) => void;
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
  // Visible day-column count (1–7). Persisted under PERIOD_LENGTH_STORAGE_KEY.
  const [periodLength, setPeriodLength] = useState(readStoredPeriodLength);

  // Strip window: first *rendered* day. Visible period is windowStart + scroll offset.
  // Initial: overscan before current period so it is centered in the buffer.
  // Uses stored period length so Day/short views do not Monday-snap on first paint.
  const [windowStart, setWindowStart] = useState(() =>
    addDays(
      fitPeriodStart(new Date(), readStoredPeriodLength()),
      -STRIP_OVERSCAN,
    ),
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
  // Suppress rebase while applying a transient programmatic scrollLeft write
  // (init park / jump / rebase compensation). Held lock owners suppress via
  // stripRebaseSuppressed instead, so a rAF release here cannot clobber them.
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

  // ── Strip lock ownership ──────────────────────────────────────────────
  // Lock owners (drag / pinch / axis) that hold the scroller's snap override.
  // A Set, not a counter: repeat holds are idempotent and releasing one owner
  // cannot clear another owner's `scroll-snap-type: none`.
  const stripLockReasonsRef = useRef<Set<StripLockReason>>(new Set());

  /** Apply the scroller's snap override from the currently held owners. */
  const applyStripLock = useCallback(() => {
    // CSS scroll-snap re-snaps on every programmatic scrollLeft write, which
    // would fight drag auto-scroll. Suspend it while any owner holds the lock;
    // clearing the inline override re-engages snap and settles on the nearest
    // column.
    const scroller = scrollerRef.current;
    if (scroller) {
      scroller.style.scrollSnapType = stripSnapDisabled(
        stripLockReasonsRef.current,
      )
        ? 'none'
        : '';
    }
  }, []);

  // ── Touch axis gesture ────────────────────────────────────────────────
  // iOS does not directional-lock a 2D overflow scroller: a vertical swipe
  // through the hours carries a little horizontal velocity, and the mandatory
  // day snap then pages to the neighbouring column. Track the gesture's
  // dominant axis and, for a Y gesture, hold the day until the finger lifts.
  const axisGestureRef = useRef<AxisGesture>({
    origin: null,
    intent: 'pending',
    left0: null,
  });
  const axisSettleTimerRef = useRef<number | null>(null);

  const clearAxisSettle = useCallback(() => {
    if (axisSettleTimerRef.current !== null) {
      clearTimeout(axisSettleTimerRef.current);
      axisSettleTimerRef.current = null;
    }
  }, []);

  /**
   * Drop the tracked gesture without restoring `scrollLeft`. Used when another
   * owner takes the scroller (drag / pinch / a second finger) and when the
   * window is re-parked: their moves invalidate the frozen `left0`.
   */
  const dropAxisGesture = useCallback(() => {
    clearAxisSettle();
    const gesture = axisGestureRef.current;
    gesture.origin = null;
    gesture.intent = 'pending';
    gesture.left0 = null;
    if (stripLockReasonsRef.current.delete('axis')) applyStripLock();
  }, [applyStripLock, clearAxisSettle]);

  const lockStrip = useCallback(
    (reason: StripLockReason) => {
      // Drag / pinch scroll the strip on both axes themselves (drag auto-scroll,
      // zoom `scrollTop`). Their programmatic writes must never start or finish
      // a touch axis gesture, so an in-flight one is dropped here — with no
      // restore, or the restore would fight their writes.
      if (reason !== 'axis') dropAxisGesture();
      stripLockReasonsRef.current.add(reason);
      applyStripLock();
    },
    [applyStripLock, dropAxisGesture],
  );

  const unlockStrip = useCallback(
    (reason: StripLockReason) => {
      stripLockReasonsRef.current.delete(reason);
      applyStripLock();
    },
    [applyStripLock],
  );

  /**
   * A Y gesture is over: put the strip back on the day the finger started on,
   * then release the lock. The restore must happen while snap is still off —
   * re-engaging snap first lets the browser pick the nearest column instead.
   */
  const finishAxisGesture = useCallback(() => {
    clearAxisSettle();
    const scroller = scrollerRef.current;
    const left0 = axisGestureRef.current.left0;
    if (scroller && left0 !== null && scroller.scrollLeft !== left0) {
      // Restore exactly `left0`, not "nearest column": day columns are ~46px
      // wide on a phone, so a ~23px drift would flip the visible day.
      scroller.scrollTo({ left: left0, behavior: 'instant' });
    }
    unlockStrip('axis');
    const gesture = axisGestureRef.current;
    gesture.origin = null;
    gesture.intent = 'pending';
    gesture.left0 = null;
  }, [clearAxisSettle, unlockStrip]);

  /**
   * Wait for the gesture to settle before restoring: iOS keeps applying
   * momentum after the last finger lifts, and a restore mid-momentum would be
   * scrolled away again. `scrollend` where supported, else a quiet period
   * re-armed by every `scroll` event.
   */
  const armAxisSettle = useCallback(() => {
    clearAxisSettle();
    axisSettleTimerRef.current = window.setTimeout(() => {
      axisSettleTimerRef.current = null;
      finishAxisGesture();
    }, AXIS_SETTLE_MS);
  }, [clearAxisSettle, finishAxisGesture]);

  // Touch events, not pointer events: the browser fires `pointercancel` the
  // moment native panning starts, so pointer events cannot follow a finger
  // through a native scroll. All listeners stay passive — never preventDefault,
  // or the gesture stops being native.
  const onTouchStart = useCallback(
    (e: TouchEvent) => {
      const gesture = axisGestureRef.current;
      if (e.touches.length !== 1) {
        // A second finger: pinch owns the scroller now (it writes `scrollTop`),
        // so drop the axis lock with no restore and stop tracking.
        dropAxisGesture();
        return;
      }
      // Drag (long-press on a chip) owns the scroller: never co-track it.
      if (stripRebaseSuppressed(stripLockReasonsRef.current)) return;

      const touch = e.touches[0];
      gesture.origin = { x: touch.clientX, y: touch.clientY };
      gesture.intent = 'pending';
      if (stripLockReasonsRef.current.has('axis')) {
        // A previous flick is still settling: keep its lock and `left0` so this
        // touch re-classifies against the same frozen day, and cancel the
        // settle that would otherwise restore under the new finger.
        clearAxisSettle();
        return;
      }
      gesture.left0 = scrollerRef.current?.scrollLeft ?? 0;
    },
    [clearAxisSettle, dropAxisGesture],
  );

  const onTouchMove = useCallback(
    (e: TouchEvent) => {
      const gesture = axisGestureRef.current;
      // Classify once per gesture: a resolved axis is never revisited.
      if (gesture.origin === null || gesture.intent !== 'pending') return;

      const touch = e.touches[0];
      const axis = classifyScrollAxis(
        touch.clientX - gesture.origin.x,
        touch.clientY - gesture.origin.y,
      );
      if (axis === 'pending') return;
      gesture.intent = axis;

      if (axis === 'y') {
        // Hold the day still. No scrollLeft write here: the finger owns the
        // position, and a write shimmies and kills iOS momentum.
        lockStrip('axis');
        return;
      }
      // Horizontal: native snap pages the days. A lock left over from a
      // still-settling flick has to go now — with no restore — or it would
      // fight the snap.
      if (stripLockReasonsRef.current.has('axis')) dropAxisGesture();
    },
    [dropAxisGesture, lockStrip],
  );

  const onTouchEnd = useCallback(
    (e: TouchEvent) => {
      // Other fingers are still down: wait for the real last lift.
      if (e.touches.length > 0) return;
      if (!stripLockReasonsRef.current.has('axis')) {
        // A tap, or a horizontal swipe handed to native snap: nothing to undo.
        dropAxisGesture();
        return;
      }
      // Arm now too, so a gesture that ends without further scrolling settles
      // on the timer instead of holding the lock forever.
      armAxisSettle();
    },
    [armAxisSettle, dropAxisGesture],
  );

  // A scroll while a settle is armed restarts the quiet period. The timer is
  // only ever armed with every finger up, so this cannot re-arm a live gesture.
  const onAxisSettleScroll = useCallback(() => {
    if (axisSettleTimerRef.current === null) return;
    armAxisSettle();
  }, [armAxisSettle]);

  const onAxisScrollEnd = useCallback(() => {
    if (axisSettleTimerRef.current === null) return;
    finishAxisGesture();
  }, [finishAxisGesture]);

  // ── Wheel axis gesture ────────────────────────────────────────────────
  // Same failure as the touch axis lock, with no finger to lift: a Mac
  // trackpad adds a little horizontal delta to a vertical two-finger scroll,
  // the mandatory day snap walks the strip sideways, and there is no settle to
  // restore on. So the sideways component must never reach the scroller at
  // all: latch the burst's axis, then drop the horizontal part for a vertical
  // burst — re-applying its vertical delta by hand, since preventing the
  // default would otherwise swallow the scroll the user asked for.
  const wheelBurstRef = useRef<WheelBurst>(EMPTY_WHEEL_BURST);

  const onWheel = useCallback((e: WheelEvent) => {
    // Ctrl/meta wheel is pinch-zoom, owned by useCalendarZoom: that handler
    // preventDefaults and rewrites `scrollTop`. Leave it entirely alone.
    if (e.ctrlKey || e.metaKey) return;
    // Drag / pinch write `scrollLeft` themselves (auto-scroll, zoom); a wheel
    // burst must not be classified against, or fight, those writes.
    if (stripRebaseSuppressed(stripLockReasonsRef.current)) return;

    const scroller = scrollerRef.current;
    if (!scroller) return;

    const dxPx = wheelDeltaPx(e.deltaX, e.deltaMode, scroller.clientWidth);
    const dyPx = wheelDeltaPx(e.deltaY, e.deltaMode, scroller.clientHeight);
    const burst = nextWheelBurst(wheelBurstRef.current, {
      dx: dxPx,
      dy: dyPx,
      t: e.timeStamp,
    });
    wheelBurstRef.current = burst;

    // `pending` (under the slop, so at most half a column of drift — mandatory
    // snap settles back on the same day) and `x` (native horizontal scroll and
    // day paging) both stay native.
    if (burst.axis !== 'y') return;
    // deltaX 0 is an ordinary mouse wheel: exactly the pre-fix behaviour.
    if (e.deltaX === 0) return;

    // Vertical burst carrying sideways drift: consume the event and apply the
    // vertical part ourselves, so `scrollLeft` never moves under the day snap.
    e.preventDefault();
    scroller.scrollBy({ top: dyPx, behavior: 'instant' });
  }, []);

  // The scroller is always mounted, so this attaches once.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;

    const opts: AddEventListenerOptions = { passive: true };
    scroller.addEventListener('touchstart', onTouchStart, opts);
    scroller.addEventListener('touchmove', onTouchMove, opts);
    scroller.addEventListener('touchend', onTouchEnd, opts);
    scroller.addEventListener('touchcancel', onTouchEnd, opts);
    scroller.addEventListener('scroll', onAxisSettleScroll, opts);
    scroller.addEventListener('scrollend', onAxisScrollEnd, opts);
    // Non-passive, unlike the touch listeners: a vertical burst has to cancel
    // the sideways component before native snap sees it.
    scroller.addEventListener('wheel', onWheel, { passive: false });

    return () => {
      scroller.removeEventListener('touchstart', onTouchStart);
      scroller.removeEventListener('touchmove', onTouchMove);
      scroller.removeEventListener('touchend', onTouchEnd);
      scroller.removeEventListener('touchcancel', onTouchEnd);
      scroller.removeEventListener('scroll', onAxisSettleScroll);
      scroller.removeEventListener('scrollend', onAxisScrollEnd);
      scroller.removeEventListener('wheel', onWheel);
      clearAxisSettle();
    };
  }, [
    clearAxisSettle,
    onAxisScrollEnd,
    onAxisSettleScroll,
    onTouchEnd,
    onTouchMove,
    onTouchStart,
    onWheel,
  ]);

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
      // An in-flight axis gesture pins a day in `left0`: shift it with the
      // content, or the end-of-gesture restore would land a period away.
      const gesture = axisGestureRef.current;
      if (gesture.left0 !== null) gesture.left0 += compensation;
      // The axis lock holds the visible day, so publish from `left0` and not
      // from the drifting scrollLeft.
      const idxScrollLeft =
        stripLockReasonsRef.current.has('axis') && gesture.left0 !== null
          ? gesture.left0
          : scroller.scrollLeft;
      setVisibleStartIdx(
        computeVisibleStartIndex(idxScrollLeft, colW, dayCount, periodLength),
      );
      releaseSuppressRebase();
      return;
    }

    // Initial mount: park scroll so current period is visible.
    // Gate on measured mainWidth — not colW > 0. colWidth(0) falls back to 16px,
    // so an early park sets scrollLeft = STRIP_OVERSCAN * 16 and marks init done.
    // When the real width arrives, the effect skips re-parking; track grows while
    // scrollLeft stays ~0/112; onScrollerScroll then sets visibleStartIdx to 0 and
    // the title shows the overscan week (e.g. Aug 31–Sep 6 instead of Mon–Sun
    // containing today). Leave didInitScrollRef false until mainWidth > 0;
    // visibleStartIdx stays seeded at STRIP_OVERSCAN (first paint already correct).
    if (!didInitScrollRef.current) {
      if (!isStripWidthMeasured(mainWidth)) return;
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
  }, [colW, dayCount, mainWidth, periodLength, windowStart, scrollNonce]);

  // Horizontal scroll: update visible start; request rebase near the edges.
  const onScrollerScroll = useCallback(() => {
    // Ignore until init park has run with a measured width. Otherwise a mount
    // scrollLeft write (or scrollLeft ≈ 0) sets visibleStartIdx to 0 and the
    // title flashes / sticks on the overscan buffer week.
    if (!didInitScrollRef.current) return;

    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;

    // While the Y lock holds, the visible day is frozen at `left0`: the
    // scrollLeft drifts sideways under the finger, and reading it would flash
    // the next day's title / sidebar before the end-of-gesture restore.
    const axisLeft0 = axisGestureRef.current.left0;
    const idxScrollLeft =
      stripLockReasonsRef.current.has('axis') && axisLeft0 !== null
        ? axisLeft0
        : scroller.scrollLeft;

    const idx = computeVisibleStartIndex(
      idxScrollLeft,
      colW,
      dayCount,
      periodLength,
    );
    setVisibleStartIdx((prev) => (prev === idx ? prev : idx));

    if (suppressRebaseRef.current) return;
    // Drag / pinch own the strip lock: no rebase while they are held. 'axis'
    // is exempt — it disables snap only, so the window may still slide.
    if (stripRebaseSuppressed(stripLockReasonsRef.current)) return;

    const dir = shouldRebase(idx, dayCount, periodLength);
    if (dir !== 0 && pendingRebaseRef.current === 0) {
      pendingRebaseRef.current = dir;
      setScrollNonce((n) => n + 1);
    }
  }, [colW, dayCount, periodLength]);

  /** Park the strip so `firstVisible` is the left edge of the viewport. */
  const jumpToVisibleStart = useCallback(
    (firstVisible: Date, scrollToNow: boolean) => {
      // The window is re-parked from scratch: any tracked axis gesture holds a
      // `left0` from the old window, which must not be restored afterwards.
      dropAxisGesture();
      shouldScrollToNowRef.current = scrollToNow;
      pendingRebaseRef.current = 0;
      const origin = addDays(startOfDay(firstVisible), -STRIP_OVERSCAN);
      setWindowStart(origin);
      setVisibleStartIdx(STRIP_OVERSCAN);
      // Layout effect parks scrollLeft at overscan once colW is known.
      pendingScrollLeftRef.current = 0; // non-null sentinel
      setScrollNonce((n) => n + 1);
    },
    [dropAxisGesture],
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
      try {
        localStorage.setItem(PERIOD_LENGTH_STORAGE_KEY, String(next));
      } catch {
        // private mode / quota — in-session change still applies
      }
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
    lockStrip,
    unlockStrip,
    onScrollerScroll,
    shiftPeriod,
    goToToday,
    goToDate,
    shouldScrollToNowRef,
  };
}

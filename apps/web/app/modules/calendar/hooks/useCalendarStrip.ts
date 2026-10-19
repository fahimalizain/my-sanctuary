import {
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type RefObject,
} from 'react';
import {
  DAYS_PER_PERIOD,
  EMPTY_WHEEL_BURST,
  SCROLL_X_THRESHOLD_PX,
  STRIP_OVERSCAN,
  WHEEL_BURST_IDLE_MS,
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
  rangeIso,
  scrollLeftForIndex,
  shiftWindowStart,
  shouldRebase,
  startOfDay,
  stripDayCount,
  stripRebaseSuppressed,
  swipeTargetIndex,
  swipeVelocity,
  visibleStartIndex as computeVisibleStartIndex,
  wheelDeltaPx,
  type ScrollAxis,
  type StripLockReason,
  type SwipeSample,
  type WheelBurst,
} from '../lib/week-layout';
import { initialStripWindowStart } from '../lib/initial-strip-range';
import {
  getPeriodLength,
  setPeriodLength,
  subscribePeriodLength,
} from '../period-length-store';

/** Release tween duration (ms): the strip's only snap — a short ease-out. */
const SWIPE_SETTLE_MS = 220;

/** Samples kept per gesture (≈ a second of 60Hz moves); velocity reads the tail. */
const SWIPE_SAMPLE_LIMIT = 24;

/**
 * Sub-pixel drift (px). A position within this of a column counts as already
 * there: the by-hand landing skips its write, and the park below skips a
 * re-park.
 */
const COLUMN_EPSILON_PX = 1;

/**
 * In-flight horizontal swipe. The scroller's x axis belongs to this hook alone
 * (the browser may only pan the hours), so a committed x gesture is entirely
 * ours: `left0` is where it started, and every move writes `left0` minus the
 * finger travel past the dead zone.
 */
interface SwipeGesture {
  /** First finger position, or null while no gesture is tracked. */
  origin: { x: number; y: number } | null;
  /** Committed axis; `pending` while the gesture is in the dead zone. */
  intent: ScrollAxis;
  /** `scrollLeft` when the finger landed. */
  left0: number;
  /** Column the finger landed on (flick stepping + clamp centre). */
  startIdx: number;
  /** Sign of dx when the gesture resolved to `x`; 0 until then. */
  lockSign: number;
  /** Last finger x, for the release velocity sample. */
  lastX: number;
  /** Recent finger samples (t, x) for the release velocity. */
  samples: SwipeSample[];
}

export interface CalendarStrip {
  periodLength: number;
  handlePeriodChange: (n: number) => void;
  visibleStart: Date;
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
  // Visible day-column count (1–7), persisted by the store under
  // PERIOD_LENGTH_STORAGE_KEY. Shared with background sync, which retargets its
  // events observer on a change — so it lives outside this hook's state.
  const periodLength = useSyncExternalStore(
    subscribePeriodLength,
    getPeriodLength,
    () => DAYS_PER_PERIOD,
  );

  // Strip window: first *rendered* day. Visible period is windowStart + scroll offset.
  // Initial: overscan before current period so it is centered in the buffer.
  // Uses the stored period length so Day/short views do not Monday-snap on first
  // paint, and the same helper background sync warms its query with.
  const [windowStart, setWindowStart] = useState(() =>
    initialStripWindowStart(new Date(), getPeriodLength()),
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
  // A column-width change (rotation, window resize, sidebar, PWA viewport) that
  // landed while a lock owner held the strip: the same visible index has to be
  // re-parked at the new width, but not by fighting the owner (see the park
  // layout effect and `flushPendingRepark`).
  const pendingReparkRef = useRef(false);

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

  // Latest visible index / column width for callbacks that must not close over
  // a render's values: `visibleStartIdx` is deliberately not a park dependency,
  // and the deferred re-park runs from `unlockStrip` / `dropSwipe`, which a
  // gesture handler may still be holding from an earlier render.
  const visibleStartIdxRef = useRef(visibleStartIdx);
  const colWRef = useRef(colW);
  visibleStartIdxRef.current = visibleStartIdx;
  colWRef.current = colW;

  // ── Strip lock ownership ──────────────────────────────────────────────
  // Owners (drag / pinch / swipe / wheel) that write `scrollLeft` themselves
  // (auto-scroll, zoom, the swipe tween). A Set, not a counter: repeat holds are
  // idempotent and releasing one owner cannot release another's hold. An owner
  // suppresses rebase and defers a re-park; nothing about CSS depends on it.
  const stripLockReasonsRef = useRef<Set<StripLockReason>>(new Set());

  // ── Horizontal scroll publishing ──────────────────────────────────────
  // Update the visible start; request a rebase near the edges. Called from the
  // scroller's own `scroll` event and, for a hand-driven swipe, once the
  // release tween has landed.
  const onScrollerScroll = useCallback(() => {
    // Ignore until init park has run with a measured width. Otherwise a mount
    // scrollLeft write (or scrollLeft ≈ 0) sets visibleStartIdx to 0 and the
    // title flashes / sticks on the overscan buffer week.
    if (!didInitScrollRef.current) return;

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
    // Every lock owner writes `scrollLeft` itself, so no rebase while one is
    // held: the window would shift out from under the write.
    if (stripRebaseSuppressed(stripLockReasonsRef.current)) return;

    const dir = shouldRebase(idx, dayCount, periodLength);
    if (dir !== 0 && pendingRebaseRef.current === 0) {
      pendingRebaseRef.current = dir;
      setScrollNonce((n) => n + 1);
    }
  }, [colW, dayCount, periodLength]);

  // ── Touch swipe gesture ───────────────────────────────────────────────
  // The scroller is `touch-action: pan-y` with no user-scrollable x axis, so
  // the browser pans the hours and never touches `scrollLeft`. A horizontal
  // swipe is therefore ours to drive, which is the only way to have a dead
  // zone — under native 2D panning the strip moves from the first sideways
  // pixel.
  const swipeRef = useRef<SwipeGesture>({
    origin: null,
    intent: 'pending',
    left0: 0,
    startIdx: 0,
    lockSign: 0,
    lastX: 0,
    samples: [],
  });
  // rAF handle for the release tween; null when the strip is idle.
  const settleRafRef = useRef<number | null>(null);

  const resetSwipeGesture = () => {
    const gesture = swipeRef.current;
    gesture.origin = null;
    gesture.intent = 'pending';
    gesture.left0 = 0;
    gesture.startIdx = 0;
    gesture.lockSign = 0;
    gesture.lastX = 0;
    gesture.samples = [];
  };

  const cancelSettle = useCallback(() => {
    if (settleRafRef.current !== null) {
      cancelAnimationFrame(settleRafRef.current);
      settleRafRef.current = null;
    }
  }, []);

  /** Quiet-period timer that lands an x wheel burst on a column. */
  const wheelSettleTimerRef = useRef<number | null>(null);

  const clearWheelSettle = useCallback(() => {
    if (wheelSettleTimerRef.current !== null) {
      clearTimeout(wheelSettleTimerRef.current);
      wheelSettleTimerRef.current = null;
    }
  }, []);

  /**
   * Land a re-park that a lock owner deferred (`pendingReparkRef`). The index is
   * the visible day, not a rounding of the current pixels: after a column-width
   * change those pixels are mid-column at the new width, so `alignStripToColumn`
   * would land on a different day.
   * @returns true when a deferred re-park was applied.
   */
  const flushPendingRepark = useCallback((): boolean => {
    if (!pendingReparkRef.current) return false;
    pendingReparkRef.current = false;
    const scroller = scrollerRef.current;
    const width = colWRef.current;
    if (!scroller || width <= 0) return false;
    suppressRebaseRef.current = true;
    scroller.scrollLeft = scrollLeftForIndex(visibleStartIdxRef.current, width);
    // Declared with the park layout effect below, like the other parks.
    releaseSuppressRebase();
    return true;
  }, []);

  /**
   * Drop the gesture without moving the strip: another owner took the scroller
   * (drag / pinch / a second finger) or the window was re-parked, so `left0` is
   * stale. Also cancels a release tween and gives up the sideways locks it owns.
   */
  const dropSwipe = useCallback(() => {
    cancelSettle();
    clearWheelSettle();
    resetSwipeGesture();
    // Both sideways owners are JS-driven x: dropping one drops the other.
    stripLockReasonsRef.current.delete('swipe');
    stripLockReasonsRef.current.delete('wheel');
    // Any owner still held (a just-claimed drag) keeps a deferred re-park back.
    if (stripLockReasonsRef.current.size === 0) flushPendingRepark();
  }, [cancelSettle, clearWheelSettle, flushPendingRepark]);

  const lockStrip = useCallback(
    (reason: StripLockReason) => {
      // Drag / pinch write `scrollLeft` themselves (auto-scroll, zoom). A touch
      // swipe in flight would fight those writes, and so would its tween, so it
      // is dropped here — no tween.
      if (reason !== 'swipe') dropSwipe();
      stripLockReasonsRef.current.add(reason);
    },
    [dropSwipe],
  );

  /**
   * Park `scrollLeft` on the nearest column, instantly and without a tween.
   * Drag and pinch write `scrollLeft` themselves (auto-scroll, zoom), so they
   * can hand the strip back between two days; the tween only runs for the
   * sideways owners (swipe / wheel), so the landing is finished here.
   */
  const alignStripToColumn = useCallback(() => {
    const scroller = scrollerRef.current;
    if (!scroller || colW <= 0) return;
    const target = scrollLeftForIndex(
      computeVisibleStartIndex(
        scroller.scrollLeft,
        colW,
        dayCount,
        periodLength,
      ),
      colW,
    );
    // Sub-pixel drift: already on the column, and the write would land nowhere.
    if (Math.abs(scroller.scrollLeft - target) < COLUMN_EPSILON_PX) return;
    scroller.scrollLeft = target;
  }, [colW, dayCount, periodLength]);

  const unlockStrip = useCallback(
    (reason: StripLockReason) => {
      stripLockReasonsRef.current.delete(reason);
      // Swipe / wheel releases land through `startSettle`, so only the owners
      // that scroll the strip themselves need the by-hand landing. A re-park a
      // column-width change deferred owns the landing instead: it lands on the
      // visible day at the new width, where `alignStripToColumn` would round the
      // old pixels by that width and pick a different day.
      if (stripLockReasonsRef.current.size === 0) {
        const reparked = flushPendingRepark();
        if (!reparked && (reason === 'drag' || reason === 'pinch')) {
          alignStripToColumn();
        }
      }
    },
    [alignStripToColumn, flushPendingRepark],
  );

  /**
   * Let go of the strip: the gesture is over (or was taken over), so drop the
   * tween and the lock, then publish the day the strip landed on. The tween ends
   * exactly on a column, so there is nothing left to correct; a rebase the lock
   * suppressed runs from this scroll.
   */
  const finishSwipe = useCallback(
    (reason: StripLockReason = 'swipe') => {
      cancelSettle();
      clearWheelSettle();
      resetSwipeGesture();
      unlockStrip(reason);
      onScrollerScroll();
    },
    [cancelSettle, clearWheelSettle, onScrollerScroll, unlockStrip],
  );

  /**
   * Ease `scrollLeft` onto column `target` under the lock the gesture already
   * holds. The tween is the strip's only snap: it has to land exactly on the
   * column, since nothing else will correct a mid-column position.
   */
  const startSettle = useCallback(
    (target: number, reason: StripLockReason = 'swipe') => {
      const scroller = scrollerRef.current;
      cancelSettle();
      // The gesture itself is over; the lock stays held until the tween lands.
      resetSwipeGesture();
      if (!scroller || colW <= 0) {
        finishSwipe(reason);
        return;
      }

      const from = scroller.scrollLeft;
      const to = scrollLeftForIndex(target, colW);
      const delta = to - from;
      if (Math.abs(delta) < 0.5) {
        scroller.scrollLeft = to;
        finishSwipe(reason);
        return;
      }

      // Baseline on the first frame rather than on performance.now(): the two
      // clocks are the same, but not necessarily phase-aligned.
      let startedAt: number | null = null;
      const step = (now: number) => {
        if (startedAt === null) startedAt = now;
        const t = Math.min(1, (now - startedAt) / SWIPE_SETTLE_MS);
        const eased = 1 - (1 - t) ** 3;
        scroller.scrollLeft = from + delta * eased;
        if (t < 1) {
          settleRafRef.current = requestAnimationFrame(step);
          return;
        }
        settleRafRef.current = null;
        // Land exactly on the column, not on a float.
        scroller.scrollLeft = to;
        finishSwipe(reason);
      };
      settleRafRef.current = requestAnimationFrame(step);
    },
    [cancelSettle, colW, finishSwipe],
  );

  /**
   * Land a sideways trackpad burst on the nearest column. The wheel has no
   * finger-up, so a quiet gap (`WHEEL_BURST_IDLE_MS`) stands in for the release;
   * every event of the burst pushes the settle back out.
   */
  const armWheelSettle = useCallback(() => {
    clearWheelSettle();
    wheelSettleTimerRef.current = window.setTimeout(() => {
      wheelSettleTimerRef.current = null;
      const scroller = scrollerRef.current;
      if (!scroller || colW <= 0) {
        dropSwipe();
        return;
      }
      startSettle(
        computeVisibleStartIndex(
          scroller.scrollLeft,
          colW,
          dayCount,
          periodLength,
        ),
        'wheel',
      );
    }, WHEEL_BURST_IDLE_MS);
  }, [clearWheelSettle, colW, dayCount, dropSwipe, periodLength, startSettle]);

  // Touch events, not pointer events: the browser fires `pointercancel` the
  // moment native panning starts, so pointer events cannot follow a finger
  // through a native scroll. touchstart / touchend stay passive; touchmove is
  // not, because a gesture that commits to x has to be able to preventDefault
  // the vertical pan the same touch would otherwise start.
  const onTouchStart = useCallback(
    (e: TouchEvent) => {
      if (e.touches.length !== 1) {
        // A second finger: pinch owns the scroller now (it writes `scrollTop`).
        // Drop the swipe with no tween and unlock, so it never fights the zoom.
        dropSwipe();
        return;
      }
      // Drag (long-press on a chip) owns the scroller: never co-track it.
      const held = stripLockReasonsRef.current;
      if (held.has('drag') || held.has('pinch')) return;

      const scroller = scrollerRef.current;
      if (!scroller || colW <= 0) return;

      // A new touch mid-tween grabs the strip where it is. Cancel the tween but
      // keep the lock: the strip stays between days under the finger.
      cancelSettle();
      const touch = e.touches[0];
      const gesture = swipeRef.current;
      gesture.origin = { x: touch.clientX, y: touch.clientY };
      gesture.intent = 'pending';
      gesture.left0 = scroller.scrollLeft;
      gesture.startIdx = Math.round(scroller.scrollLeft / colW);
      gesture.lockSign = 0;
      gesture.lastX = touch.clientX;
      gesture.samples = [{ t: e.timeStamp, x: touch.clientX }];
    },
    [cancelSettle, colW, dropSwipe],
  );

  const onTouchMove = useCallback(
    (e: TouchEvent) => {
      const gesture = swipeRef.current;
      const scroller = scrollerRef.current;
      const touch = e.touches[0];
      if (gesture.origin === null || !scroller || !touch) return;

      const dx = touch.clientX - gesture.origin.x;
      const dy = touch.clientY - gesture.origin.y;

      if (gesture.intent === 'pending') {
        const axis = classifyScrollAxis(dx, dy);
        // Dead zone: nothing moves sideways until the swipe is deliberate.
        if (axis === 'pending') return;
        gesture.intent = axis;
        if (axis === 'x') {
          // The direction is fixed here and kept for the whole gesture, so
          // travel stays continuous if the finger reverses past its origin.
          gesture.lockSign = dx > 0 ? 1 : -1;
          lockStrip('swipe');
        }
      }
      // A y gesture is the browser's: `pan-y` scrolls the hours natively. It
      // must not fall through to the write below — that would page the day on
      // every vertical move, which is the bug this whole gesture exists to fix.
      if (gesture.intent !== 'x') return;

      // One gesture, one axis: the swipe owns this touch, so the browser gets
      // nothing more from it. Cancelling the move is what freezes the hours for
      // the rest of the gesture — including a diagonal reversal that would
      // otherwise start a vertical pan halfway through. A move the browser has
      // already committed to a scroll is non-cancelable: the call would be a
      // no-op (and Chrome warns about it), so it is skipped.
      if (e.cancelable) e.preventDefault();

      // Subtract the dead zone so the strip starts at `left0` on the threshold
      // and then follows the finger 1:1, with no jump at the origin.
      const travel = dx - gesture.lockSign * SCROLL_X_THRESHOLD_PX;
      const maxLeft = scrollLeftForIndex(dayCount - periodLength, colW);
      scroller.scrollLeft = Math.min(
        maxLeft,
        Math.max(0, gesture.left0 - travel),
      );
      gesture.lastX = touch.clientX;
      gesture.samples.push({ t: e.timeStamp, x: touch.clientX });
      if (gesture.samples.length > SWIPE_SAMPLE_LIMIT) gesture.samples.shift();
    },
    [colW, dayCount, lockStrip, periodLength],
  );

  const onTouchEnd = useCallback(
    (e: TouchEvent) => {
      // Other fingers are still down: wait for the real last lift.
      if (e.touches.length > 0) return;
      const gesture = swipeRef.current;
      if (gesture.origin === null) return;
      const scroller = scrollerRef.current;
      if (!scroller) {
        dropSwipe();
        return;
      }

      const wasSwipe = gesture.intent === 'x';
      const swipeHeld = stripLockReasonsRef.current.has('swipe');
      // Close the velocity window at release with the finger's last position: a
      // finger that stopped before lifting must not read as a flick from its
      // last fast samples.
      gesture.samples.push({ t: e.timeStamp, x: gesture.lastX });

      if (wasSwipe) {
        startSettle(
          swipeTargetIndex({
            startIdx: gesture.startIdx,
            scrollLeft: scroller.scrollLeft,
            velocity: swipeVelocity(gesture.samples),
            colW,
            periodLength,
            maxIdx: dayCount - periodLength,
          }),
        );
        return;
      }
      if (swipeHeld) {
        // This touch interrupted a tween and lifted without committing to an
        // axis: land on the nearest column instead of leaving the strip
        // stranded between two days.
        startSettle(
          computeVisibleStartIndex(
            scroller.scrollLeft,
            colW,
            dayCount,
            periodLength,
          ),
        );
        return;
      }
      // A tap, or a vertical gesture that native pan-y already owns: nothing to
      // land, and nothing the strip has to publish.
      dropSwipe();
    },
    [colW, dayCount, dropSwipe, periodLength, startSettle],
  );

  // ── Wheel axis gesture ────────────────────────────────────────────────
  // The same sideways dead zone as touch, with no finger to lift: a Mac
  // trackpad adds a little horizontal delta to a vertical two-finger scroll,
  // and there is no release to settle on. A touch gets its dead zone from
  // `touch-action: pan-y`; a wheel has no such property, so the handler has to
  // decide for it — the accumulated burst stays `pending` until it is a
  // deliberate, horizontal-dominant swipe. A committed x burst is then driven
  // here (it is the same JS-driven axis the touch swipe uses) and settles on a
  // column once the burst goes quiet.
  const wheelBurstRef = useRef<WheelBurst>(EMPTY_WHEEL_BURST);

  const onWheel = useCallback(
    (e: WheelEvent) => {
      // Ctrl/meta wheel is pinch-zoom, owned by useCalendarZoom: that handler
      // preventDefaults and rewrites `scrollTop`. Leave it entirely alone.
      if (e.ctrlKey || e.metaKey) return;
      // A pointer owner (auto-scroll, zoom, touch swipe) writes `scrollLeft`
      // itself; a wheel burst must not be classified against, or fight, those
      // writes. `wheel` is this handler's own lock, so it is not a blocker.
      const held = stripLockReasonsRef.current;
      if (held.has('drag') || held.has('pinch') || held.has('swipe')) return;

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

      // `x`: a deliberate sideways burst, driven here under the `wheel` lock and
      // landed on a column when it goes quiet. preventDefault keeps a Mac
      // two-finger sideways gesture from becoming a history swipe.
      if (burst.axis === 'x') {
        e.preventDefault();
        cancelSettle();
        if (!held.has('wheel')) lockStrip('wheel');
        scroller.scrollLeft += dxPx;
        armWheelSettle();
        return;
      }
      // Sideways-dominant but still inside the dead zone: the same gesture the
      // browser would turn into a history swipe. Consume it and move nothing —
      // the burst may still commit on a later event, and the pixels dropped here
      // are the price of the dead zone, exactly as they are for touch.
      if (
        burst.axis === 'pending' &&
        Math.abs(burst.sumX) > Math.abs(burst.sumY)
      ) {
        e.preventDefault();
        return;
      }
      // A vertical burst, or a mouse wheel with no deltaX at all, is the
      // browser's: the vertical scroll stays native (momentum and all), the
      // sideways drift of a two-finger scroll cannot move an `overflow-x: hidden`
      // axis, and there is nothing left to re-apply by hand.
    },
    [armWheelSettle, cancelSettle, lockStrip],
  );

  // The scroller is always mounted, so this attaches once per layout change.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;

    scroller.addEventListener('touchstart', onTouchStart, { passive: true });
    // Non-passive: the swipe preventDefaults its own moves to keep one gesture
    // on one axis (see `onTouchMove`).
    scroller.addEventListener('touchmove', onTouchMove, { passive: false });
    scroller.addEventListener('touchend', onTouchEnd, { passive: true });
    scroller.addEventListener('touchcancel', onTouchEnd, { passive: true });
    // Non-passive too: an x burst has to cancel the event before the browser
    // turns it into a history swipe.
    scroller.addEventListener('wheel', onWheel, { passive: false });

    return () => {
      scroller.removeEventListener('touchstart', onTouchStart);
      scroller.removeEventListener('touchmove', onTouchMove);
      scroller.removeEventListener('touchend', onTouchEnd);
      scroller.removeEventListener('touchcancel', onTouchEnd);
      scroller.removeEventListener('wheel', onWheel);
      // Unmount / relayout: never leave a tween or the lock running.
      dropSwipe();
    };
  }, [dropSwipe, onTouchEnd, onTouchMove, onTouchStart, onWheel]);

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
      // An intentional write of ours: nowhere to publish but here, since the
      // compensated position has not been scrolled to yet.
      const compensated = scroller.scrollLeft + compensation;
      scroller.scrollLeft = compensated;
      setVisibleStartIdx(
        computeVisibleStartIndex(compensated, colW, dayCount, periodLength),
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

    // Column-width change (rotation, window resize, sidebar, PWA viewport): the
    // same visible day has to be re-parked at the new width, because the columns
    // have moved out from under the current pixels. The index is the current
    // visible start, never `round(scrollLeft / colW)`: the old pixels divided by
    // the new column width is a different day.
    // Reached only once init has run, with no pending rebase (it returned above)
    // and no pending jump (consumed above); both parks land on the target and
    // fall through to the no-op below.
    const target = scrollLeftForIndex(visibleStartIdxRef.current, colW);
    if (Math.abs(scroller.scrollLeft - target) < COLUMN_EPSILON_PX) {
      // Already on the column: nothing left to re-park (a resize back to the old
      // width lands here too).
      pendingReparkRef.current = false;
      return;
    }
    if (stripLockReasonsRef.current.size > 0) {
      // An owner writes `scrollLeft` itself: the re-park waits for the release
      // that leaves the set empty (`flushPendingRepark`).
      pendingReparkRef.current = true;
      return;
    }
    pendingReparkRef.current = false;
    suppressRebaseRef.current = true;
    scroller.scrollLeft = target;
    releaseSuppressRebase();
  }, [colW, dayCount, mainWidth, periodLength, windowStart, scrollNonce]);

  /** Park the strip so `firstVisible` is the left edge of the viewport. */
  const jumpToVisibleStart = useCallback(
    (firstVisible: Date, scrollToNow: boolean) => {
      // The window is re-parked from scratch: a swipe in flight holds a `left0`
      // from the old window, and its tween targets an index in it.
      dropSwipe();
      shouldScrollToNowRef.current = scrollToNow;
      pendingRebaseRef.current = 0;
      const origin = addDays(startOfDay(firstVisible), -STRIP_OVERSCAN);
      setWindowStart(origin);
      setVisibleStartIdx(STRIP_OVERSCAN);
      // Layout effect parks scrollLeft at overscan once colW is known.
      pendingScrollLeftRef.current = 0; // non-null sentinel
      setScrollNonce((n) => n + 1);
    },
    [dropSwipe],
  );

  const shiftPeriod = useCallback(
    (deltaPeriods: number) => {
      shouldScrollToNowRef.current = false;
      const scroller = scrollerRef.current;
      if (!scroller || colW <= 0) return;
      // Clamped the way the swipe clamps: the browser would clamp the write to
      // the scrollable range anyway, and the published index has to be where the
      // strip can actually land.
      const maxLeft = scrollLeftForIndex(dayCount - periodLength, colW);
      const nextLeft = Math.min(
        maxLeft,
        Math.max(0, scroller.scrollLeft + deltaPeriods * periodLength * colW),
      );
      scroller.scrollLeft = nextLeft;
      // onScroll will update visibleStartIdx and rebase if needed.
      onScrollerScroll();
    },
    [colW, dayCount, periodLength, onScrollerScroll],
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
      // The store persists it and wakes background sync, which retargets its
      // events observer to initialStripRange(now, next) — a different window
      // from the one jumped to below, which anchors on the visible day.
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

  return {
    periodLength,
    handlePeriodChange,
    visibleStart,
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

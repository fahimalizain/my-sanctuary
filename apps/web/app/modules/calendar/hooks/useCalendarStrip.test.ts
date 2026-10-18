// Regression: first visit must show today's fitted period, not the overscan
// buffer week. DOM must exist before the hook module is imported.
//
// Failure mode (pre-fix): mainWidth starts at 0 → colWidth fallback 16px →
// init parks scrollLeft = STRIP_OVERSCAN * 16 and sets didInitScrollRef. When
// measure lands, park is skipped; onScroll then sets visibleStartIdx to 0 so
// the title shows windowStart … windowStart+6 (e.g. Aug 31–Sep 6) instead of
// Mon–Sun containing today.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createElement,
  useLayoutEffect,
  useRef,
  type ReactElement,
} from 'react';
import { installDom } from '../components/install-dom';

installDom();

type CalendarStrip = import('./useCalendarStrip').CalendarStrip;

let useCalendarStrip: typeof import('./useCalendarStrip').useCalendarStrip;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;
let act: typeof import('@testing-library/react').act;
let fitPeriodStart: typeof import('../lib/week-layout').fitPeriodStart;
let addDays: typeof import('../lib/week-layout').addDays;
let STRIP_OVERSCAN: typeof import('../lib/week-layout').STRIP_OVERSCAN;
let DAYS_PER_PERIOD: typeof import('../lib/week-layout').DAYS_PER_PERIOD;
let isSameDay: typeof import('../lib/week-layout').isSameDay;
let colWidth: typeof import('../lib/week-layout').colWidth;
let scrollLeftForIndex: typeof import('../lib/week-layout').scrollLeftForIndex;
let initialStripRange: typeof import('../lib/initial-strip-range').initialStripRange;
let setPeriodLength: typeof import('../period-length-store').setPeriodLength;

const GRID_WIDTH = 800;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  act = rtl.act;
  const layout = await import('../lib/week-layout');
  fitPeriodStart = layout.fitPeriodStart;
  addDays = layout.addDays;
  STRIP_OVERSCAN = layout.STRIP_OVERSCAN;
  DAYS_PER_PERIOD = layout.DAYS_PER_PERIOD;
  isSameDay = layout.isSameDay;
  colWidth = layout.colWidth;
  scrollLeftForIndex = layout.scrollLeftForIndex;
  const stripRange = await import('../lib/initial-strip-range');
  initialStripRange = stripRange.initialStripRange;
  const store = await import('../period-length-store');
  setPeriodLength = store.setPeriodLength;
  const mod = await import('./useCalendarStrip');
  useCalendarStrip = mod.useCalendarStrip;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

function stubClientWidth(el: HTMLElement, width: number): void {
  Object.defineProperty(el, 'clientWidth', {
    configurable: true,
    get: () => width,
  });
  Object.defineProperty(el, 'clientHeight', {
    configurable: true,
    get: () => 600,
  });
}

/**
 * Real jsdom TouchEvent, so the hook's `e.touches.length` / `touches[0]` reads
 * behave as they do in a browser. jsdom has no `Touch` constructor, so the
 * points stay plain objects. `time` pins `event.timeStamp` (release velocity
 * is computed from it), which jsdom would otherwise derive from the clock and
 * make nondeterministic.
 */
function touchEvent(
  type: 'touchstart' | 'touchmove' | 'touchend',
  points: { clientX: number; clientY: number }[],
  time?: number,
): Event {
  const event = new window.TouchEvent(type, {
    touches: points as unknown as Touch[],
  });
  if (time !== undefined) {
    Object.defineProperty(event, 'timeStamp', {
      configurable: true,
      value: time,
    });
  }
  return event;
}

const sleep = (ms: number): Promise<void> =>
  new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });

/** Render the harness, let the init park land, and hand back the strip. */
async function mountParkedStrip(
  onStrip?: (strip: CalendarStrip) => void,
): Promise<CalendarStrip> {
  let latest: CalendarStrip | null = null;
  const getLatest = (): CalendarStrip | null => latest;

  await act(async () => {
    render(
      createElement(StripHarness, {
        gridWidth: GRID_WIDTH,
        onStrip: (s) => {
          latest = s;
          onStrip?.(s);
        },
      }),
    );
  });
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip, 'harness should report strip state');
  return strip;
}

/**
 * Read the harness's newest strip through its reporter. A re-render hands back a
 * new strip object, so "did the day move?" has to re-read it, never compare
 * against the object captured at mount.
 */
function latestStrip(get: () => CalendarStrip | null): CalendarStrip {
  const strip = get();
  assert.ok(strip, 'harness should report strip state');
  return strip;
}

/** Harness attaches strip refs and stubs a measured grid width before paint. */
function StripHarness({
  onStrip,
  gridWidth,
}: {
  onStrip: (strip: CalendarStrip) => void;
  gridWidth: number;
}): ReactElement {
  const strip = useCalendarStrip();
  const gridAttached = useRef(false);
  const scrollerAttached = useRef(false);

  // Report latest strip each layout (after the hook's own layout effects).
  useLayoutEffect(() => {
    onStrip(strip);
  });

  return createElement(
    'div',
    {
      ref: (el: HTMLDivElement | null) => {
        (strip.gridColumnRef as { current: HTMLDivElement | null }).current =
          el;
        if (el && !gridAttached.current) {
          stubClientWidth(el, gridWidth);
          gridAttached.current = true;
        }
      },
    },
    createElement('div', {
      ref: (el: HTMLDivElement | null) => {
        (strip.scrollerRef as { current: HTMLDivElement | null }).current = el;
        if (el && !scrollerAttached.current) {
          stubClientWidth(el, gridWidth);
          // Wide enough that overscan park is within max scroll.
          Object.defineProperty(el, 'scrollWidth', {
            configurable: true,
            get: () => 4000,
          });
          // Tall enough that a scrollTop pin can be observed.
          Object.defineProperty(el, 'scrollHeight', {
            configurable: true,
            get: () => 4000,
          });
          scrollerAttached.current = true;
        }
      },
      onScroll: strip.onScrollerScroll,
      style: { overflow: 'auto', width: gridWidth },
    }),
  );
}

test('useCalendarStrip: after measure, visibleStart is fitPeriodStart(today), not overscan week', async () => {
  let latest: CalendarStrip | null = null;
  // Direct reads of `latest` narrow to `null`: the callback assignment is
  // invisible to control-flow analysis. Read through a function to recover the
  // declared union before snapshotting into a const.
  const getLatest = (): CalendarStrip | null => latest;

  await act(async () => {
    render(
      createElement(StripHarness, {
        gridWidth: GRID_WIDTH,
        onStrip: (s) => {
          latest = s;
        },
      }),
    );
  });

  // Allow rAF suppress release + any follow-up layout from measure → park.
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip, 'harness should report strip state');

  const expectedStart = fitPeriodStart(new Date(), DAYS_PER_PERIOD);
  const overscanStart = addDays(expectedStart, -STRIP_OVERSCAN);

  assert.ok(
    isSameDay(strip.visibleStart, expectedStart),
    `visibleStart should be fitted period start (${expectedStart.toDateString()}), got ${strip.visibleStart.toDateString()}`,
  );
  assert.ok(
    !isSameDay(strip.visibleStart, overscanStart),
    `visibleStart must not be the overscan buffer week (${overscanStart.toDateString()})`,
  );

  // First paint queries exactly the landing window background sync keeps warm:
  // one helper, one key, so opening Calendar after a period away is a cache hit.
  assert.deepEqual(
    strip.range,
    initialStripRange(new Date(), strip.periodLength),
  );

  // Park used measured colW, not the 16px fallback.
  const measuredColW = colWidth(GRID_WIDTH, strip.periodLength);
  assert.ok(measuredColW > 16);
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  assert.equal(
    scroller!.scrollLeft,
    scrollLeftForIndex(STRIP_OVERSCAN, measuredColW),
  );
});

test('useCalendarStrip: onScrollerScroll before measured park does not clobber visibleStartIdx', async () => {
  // gridWidth 0 → measure stays unmeasured; didInitScrollRef stays false.
  let latest: CalendarStrip | null = null;
  // See note above: read through a function to recover the declared union.
  const getLatest = (): CalendarStrip | null => latest;

  await act(async () => {
    render(
      createElement(StripHarness, {
        gridWidth: 0,
        onStrip: (s) => {
          latest = s;
        },
      }),
    );
  });

  const strip = getLatest();
  assert.ok(strip);
  const before = strip.visibleStart.getTime();

  // Simulate a mount scroll with scrollLeft ≈ 0 (would set idx 0 if unguarded).
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  scroller!.scrollLeft = 0;
  await act(async () => {
    strip.onScrollerScroll();
  });

  const after = getLatest();
  assert.ok(after);
  assert.equal(
    after.visibleStart.getTime(),
    before,
    'pre-init onScroll must not move visibleStart off the seeded overscan index',
  );
  assert.ok(
    isSameDay(after.visibleStart, fitPeriodStart(new Date(), DAYS_PER_PERIOD)),
  );
});

// ── Touch swipe ─────────────────────────────────────────────────────────
// `touch-action: pan-y` means native touch never moves the strip sideways, so
// a horizontal swipe is driven by hand and can have a dead zone: the strip
// stays put until the gesture is a deliberate SCROLL_X_THRESHOLD_PX across.
// While a touch has not committed to x it rides the vertical rail: snap is off
// from touchstart (mandatory x-snap is what iOS collapses the strip with) and
// comes back once the native pan is over.

test('useCalendarStrip: a vertical touch never moves the strip sideways', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  // The rail goes up with the finger, before any pan: mandatory snap must not
  // be live during a touch that has not committed to an axis.
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'the rail takes snap off from touchstart',
  );

  // The first move resolves the gesture to y (dy 40 > 10, dx 5 tiny). Every
  // later move must be ignored, whatever sideways drift the thumb picks up:
  // the day must never move on a time scroll.
  const moves = [
    { x: 305, y: 340, t: 16 }, // dx +5
    { x: 296, y: 400, t: 32 }, // dx -4
    { x: 309, y: 480, t: 48 }, // dx +9
    { x: 295, y: 560, t: 64 }, // dx -5
  ];
  for (const move of moves) {
    await act(async () => {
      scroller!.dispatchEvent(
        touchEvent('touchmove', [{ clientX: move.x, clientY: move.y }], move.t),
      );
    });
    assert.equal(
      scroller!.scrollLeft,
      left0,
      `a vertical gesture must never write scrollLeft (move to ${move.x}, ${move.y})`,
    );
    assert.equal(
      scroller!.style.scrollSnapType,
      'none',
      'the rail keeps snap off for the whole pan',
    );
  }

  // The rail outlives the finger: the pan this touch started is still running,
  // and snap must not come back on mid-pan.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 80));
  });
  assert.equal(scroller!.scrollLeft, left0);
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'the rail holds snap off past the finger-up',
  );

  await act(async () => {
    await sleep(250);
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'the rail releases once the pan has gone quiet',
  );
  assert.equal(scroller!.scrollLeft, left0, 'and never moved the strip');
});

test('useCalendarStrip: the rail releases on scrollend without waiting out a timer', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 40));
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'the rail holds snap off past the finger-up',
  );

  // Where the browser fires it, `scrollend` is the exact end of the pan: snap
  // comes back at once instead of after a fixed quiet window.
  await act(async () => {
    scroller!.dispatchEvent(new window.Event('scrollend'));
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'snap is back as soon as the pan is over',
  );
  assert.equal(scroller!.scrollLeft, left0, 'and the strip never moved');
});

test('useCalendarStrip: a horizontal move under the threshold leaves the strip alone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  // 47px sideways: one pixel short of the deliberate swipe.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 253, clientY: 300 }], 16),
    );
  });

  assert.equal(scroller!.scrollLeft, left0, 'the dead zone must not move');
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'the rail is up, but no swipe lock has been taken',
  );

  // The same touch turning vertical must not move it either, and runs on the
  // rail until the pan (here: nothing) goes quiet.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 250, clientY: 420 }], 32),
    );
  });
  assert.equal(scroller!.scrollLeft, left0, 'still no day movement');
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 200));
  });
  await act(async () => {
    await sleep(250);
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'a tap that scrolled nothing still releases the rail',
  );
  assert.equal(scroller!.scrollLeft, left0);
});

test('useCalendarStrip: past the threshold the strip follows minus the dead zone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  // dx 96: the strip follows the 48px past the threshold, not all 96.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 396, clientY: 300 }], 16),
    );
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'snap is off while the finger drives the strip',
  );
  assert.equal(scroller!.scrollLeft, left0 - 48);

  // dx 150 → left0 - 102: continuous with the previous move, no second jump.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 450, clientY: 300 }], 32),
    );
  });
  assert.equal(scroller!.scrollLeft, left0 - 102);
});

test('useCalendarStrip: after the lock the strip follows 1:1 past the origin', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  // dx +60 locks the swipe to the right → left0 − 12.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 360, clientY: 300 }], 16),
    );
  });
  assert.equal(scroller!.scrollLeft, left0 - 12);

  // Back to +10: the finger moved 50px left, so the strip is back at
  // left0 − 12 + 48 = left0 + 38. No second dead zone.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 310, clientY: 300 }], 32),
    );
  });
  assert.equal(scroller!.scrollLeft, left0 + 38);

  // Past the origin to −10: left0 + 58. Keeping the lock-time direction is what
  // avoids the ~96px jump a per-move `sign(dx)` would cause at dx = 0.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 290, clientY: 300 }], 48),
    );
  });
  assert.equal(scroller!.scrollLeft, left0 + 58);
});

test('useCalendarStrip: release tweens onto a column, then re-engages snap', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 450, clientY: 300 }], 16),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 460, clientY: 300 }], 32),
    );
  });
  // Release long after the last move: the finger came to rest, so this is not
  // a flick and must not project past the nearest column.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 500));
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'snap stays off until the tween lands',
  );

  await act(async () => {
    await sleep(400);
  });

  // left0 − 112 = 630; the nearest column is 6 (6 × 106 = 636).
  assert.equal(
    scroller!.scrollLeft,
    scrollLeftForIndex(6, strip.colW),
    'the tween must land exactly on a column',
  );
  assert.equal(
    scroller!.scrollLeft % strip.colW,
    0,
    'no float left between two days',
  );
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'snap re-engages on landing',
  );
});

test('useCalendarStrip: drag taking the strip drops the swipe mid-gesture', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 420, clientY: 300 }], 16),
    );
  });
  const followed = scroller!.scrollLeft;
  assert.equal(scroller!.style.scrollSnapType, 'none');

  // A claimed drag (long-press on a chip) owns scrollLeft now: the swipe is
  // dropped with no tween, and the drag lock keeps snap off.
  await act(async () => {
    strip.lockStrip('drag');
  });
  assert.equal(
    scroller!.scrollLeft,
    followed,
    'dropping the swipe must not move the strip',
  );
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'drag still holds the snap lock',
  );

  // A lift after the takeover must not settle the dropped swipe.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 200));
  });
  await act(async () => {
    await sleep(300);
  });
  assert.equal(
    scroller!.scrollLeft,
    followed,
    'no release tween runs after the drag took the strip',
  );

  await act(async () => {
    strip.unlockStrip('drag');
  });
  assert.equal(scroller!.style.scrollSnapType, '');
});

test('useCalendarStrip: releasing a drag lands the strip on a column before snap returns', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);

  await act(async () => {
    strip.lockStrip('drag');
  });
  // Drag autoscroll leaves the strip between two days.
  scroller!.scrollLeft = scrollLeftForIndex(2, strip.colW) + 40;
  await act(async () => {
    strip.unlockStrip('drag');
  });

  // Snap alone could no longer finish that landing — the guard would put the
  // mid-column position straight back — so the strip is aligned by hand.
  assert.equal(
    scroller!.scrollLeft,
    scrollLeftForIndex(2, strip.colW),
    'the nearest column, not the mid-column position autoscroll left',
  );
  assert.equal(scroller!.style.scrollSnapType, '');
});

// ── Committed scrollLeft guard ──────────────────────────────────────────
// The strip's x axis is programmatically driven: only a write of this hook's
// may move it. `overflow-x: hidden` plus mandatory x-snap broke that on iOS —
// a vertical pan made WebKit collapse `scrollLeft` to the first snap column
// (the oldest overscan day), and the strip republished it and rebased
// backwards through history on every scroll event.

test('useCalendarStrip: a browser scroll to the first column is put back, not published', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  const start0 = latestStrip(() => latest).visibleStart;
  // No touch was dispatched: the guard is unconditional for changes the browser
  // makes on its own, rail or no rail.
  assert.equal(scroller!.style.scrollSnapType, '');

  // The browser throwing the strip at history: values on their way to 0, then 0
  // itself — the first overscan column.
  for (const collapsed of [left0 - 30, left0 - strip.colW, 0]) {
    scroller!.scrollLeft = collapsed;
    await act(async () => {
      strip.onScrollerScroll();
    });

    assert.equal(
      scroller!.scrollLeft,
      left0,
      `scrollLeft must return to the parked column (thrown to ${collapsed})`,
    );
    assert.ok(
      isSameDay(latestStrip(() => latest).visibleStart, start0),
      `the visible day must not follow the browser's scroll (thrown to ${collapsed})`,
    );
  }
  assert.equal(
    latestStrip(() => latest).visibleStart.getTime(),
    start0.getTime(),
    'the fitted period start, not a rebased window',
  );
});

test('useCalendarStrip: a vertical pan cannot rewind the strip through history', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  const start0 = latestStrip(() => latest).visibleStart;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 303, clientY: 360 }], 16),
    );
  });
  // A pan that has resolved to y owns the hours; the strip's x axis is nobody's
  // but this hook's. Mid-pan the browser drops the strip on the first snap
  // column — the oldest overscan day. Publishing that, and rebasing on it, is
  // the rewind through history.
  scroller!.scrollLeft = 0;
  await act(async () => {
    strip.onScrollerScroll();
  });
  assert.equal(scroller!.scrollLeft, left0, 'the strip is put back');
  assert.equal(
    latestStrip(() => latest).visibleStart.getTime(),
    start0.getTime(),
    'the visible period is the fitted one, not an earlier rebased window',
  );

  // The pan ends: the rail releases, snap comes back, and the day never moved.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 100));
  });
  await act(async () => {
    scroller!.dispatchEvent(new window.Event('scrollend'));
  });
  assert.equal(scroller!.style.scrollSnapType, '');
  assert.equal(scroller!.scrollLeft, left0);
  assert.equal(
    latestStrip(() => latest).visibleStart.getTime(),
    start0.getTime(),
    'still the fitted period after the rail releases',
  );
});

test('useCalendarStrip: a corrective write the browser ignores does not move the day', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  const start0 = latestStrip(() => latest).visibleStart;

  // iOS ignores programmatic `scrollLeft` writes while a native pan is in
  // flight: the strip sits at 0 (where the pan collapsed it) and the guard's
  // correction cannot land yet.
  let stored = 0;
  Object.defineProperty(scroller, 'scrollLeft', {
    configurable: true,
    get: () => stored,
    set: (value: number) => {
      if (value === 0) stored = 0;
    },
  });

  try {
    await act(async () => {
      strip.onScrollerScroll();
    });
    assert.equal(scroller!.scrollLeft, 0, 'the corrective write was swallowed');
    assert.equal(
      latestStrip(() => latest).visibleStart.getTime(),
      start0.getTime(),
      'a day the browser threw out must not be published',
    );
    assert.notEqual(
      latestStrip(() => latest).visibleStart.getTime(),
      latestStrip(() => latest).days[0].getTime(),
      'and certainly not the first overscan day',
    );
  } finally {
    // Never leave the stub installed: the hook keeps reading `scrollLeft`.
    Reflect.deleteProperty(scroller!, 'scrollLeft');
  }

  // The pan ends and writes stick again: the next scroll event retries, and the
  // strip is back on the column it was committed to.
  await act(async () => {
    strip.onScrollerScroll();
  });
  assert.equal(scroller!.scrollLeft, left0);
  assert.equal(
    latestStrip(() => latest).visibleStart.getTime(),
    start0.getTime(),
  );
});

test('useCalendarStrip: shiftPeriod is an intentional write and is not reverted', async () => {
  // A three-day period: one period away from the parked overscan column stays
  // clear of the rebase threshold, so the write itself is observable (a 7-day
  // shift rebases and re-parks `scrollLeft`, which would hide the assertion).
  setPeriodLength(3);
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  assert.equal(strip.periodLength, 3);
  const start0 = latestStrip(() => latest).visibleStart;

  try {
    // A `scroll` synchronous with the assignment (jsdom fires none by itself):
    // this is the write the guard must already have committed, or it reads the
    // pre-shift day out of `committedLeftRef` and puts it back.
    let stored = left0;
    Object.defineProperty(scroller, 'scrollLeft', {
      configurable: true,
      get: () => stored,
      set: (value: number) => {
        stored = value;
        scroller!.dispatchEvent(new window.Event('scroll'));
      },
    });

    await act(async () => {
      strip.shiftPeriod(1);
    });

    assert.equal(
      stored,
      left0 + strip.periodLength * strip.colW,
      'the shift is a write of ours: the guard must adopt it, not revert it',
    );
    assert.ok(
      isSameDay(
        latestStrip(() => latest).visibleStart,
        addDays(start0, strip.periodLength),
      ),
      'the visible period moved forward by one',
    );
  } finally {
    // Never leave the stub installed; the element goes with the unmount.
    const landed = scroller!.scrollLeft;
    Reflect.deleteProperty(scroller!, 'scrollLeft');
    scroller!.scrollLeft = landed;
    cleanup();
    // The store is shared with every other test in this file (and localStorage).
    setPeriodLength(DAYS_PER_PERIOD);
  }
});

// ── Column-width changes ────────────────────────────────────────────────
// `committedLeftRef` is pixels: a rotation, window resize, sidebar or PWA
// viewport change moves the columns out from under it. The strip has to re-park
// the same visible index at the new width, or the guard writes the old pixels
// back — mid-column at the new width — and mandatory snap re-snaps that write
// while the read-back is rejected, leaving the day nobody published.

/** A ResizeObserver that hands its callback to the test instead of firing it. */
function captureResizeObserver(): {
  callbacks: ResizeObserverCallback[];
  restore: () => void;
} {
  const previous = globalThis.ResizeObserver;
  const callbacks: ResizeObserverCallback[] = [];
  class RecordingResizeObserver {
    constructor(callback: ResizeObserverCallback) {
      callbacks.push(callback);
    }
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globalThis.ResizeObserver = RecordingResizeObserver;
  return {
    callbacks,
    restore: () => {
      globalThis.ResizeObserver = previous;
    },
  };
}

test('useCalendarStrip: a column-width change re-parks on the same visible day', async () => {
  // The hook measures once in a layout effect and again from its ResizeObserver
  // (install-dom's is a no-op), so the resize is delivered by firing the recorded
  // callback. `measure` reads the element's width, not the entries.
  const observer = captureResizeObserver();

  try {
    let latest: CalendarStrip | null = null;
    const strip = await mountParkedStrip((s) => {
      latest = s;
    });
    const scroller = strip.scrollerRef.current;
    const grid = strip.gridColumnRef.current;
    assert.ok(scroller);
    assert.ok(grid);

    const left0 = scroller!.scrollLeft;
    const colW0 = strip.colW;
    const start0 = latestStrip(() => latest).visibleStart;
    assert.equal(left0, scrollLeftForIndex(STRIP_OVERSCAN, colW0));
    assert.ok(left0 > 0, 'init park should have scrolled the strip');

    // Rotation / window resize / sidebar: the columns get wider.
    stubClientWidth(grid!, 1200);
    const colW1 = colWidth(1200, strip.periodLength);
    assert.notEqual(colW1, colW0, 'the fixture must change the column width');

    const measure = observer.callbacks[0];
    assert.ok(measure, 'the hook should have registered a ResizeObserver');
    await act(async () => {
      measure([], {} as ResizeObserver);
    });
    // The suppress release is rAF-deferred, as in every other park.
    await act(async () => {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    });

    assert.equal(
      scroller!.scrollLeft,
      scrollLeftForIndex(STRIP_OVERSCAN, colW1),
      'the same index, re-parked at the new column width',
    );
    assert.notEqual(
      scroller!.scrollLeft,
      left0,
      'the old pixels would be mid-column at the new width',
    );
    assert.ok(
      isSameDay(latestStrip(() => latest).visibleStart, start0),
      'the visible day must not move',
    );
    assert.equal(
      latestStrip(() => latest).visibleStart.getTime(),
      start0.getTime(),
      'and the window must not rebase',
    );

    // The regression: mandatory snap re-snaps a stale pixel write, the read-back
    // differs, and the guard returned without publishing. The old pixels are
    // thrown back at the strip; the new column is what must be restored.
    scroller!.scrollLeft = left0;
    await act(async () => {
      latestStrip(() => latest).onScrollerScroll();
    });
    assert.equal(
      scroller!.scrollLeft,
      scrollLeftForIndex(STRIP_OVERSCAN, colW1),
      'the guard restores the new column, not the old pixels',
    );
    assert.ok(
      isSameDay(latestStrip(() => latest).visibleStart, start0),
      'still the same visible day',
    );
  } finally {
    observer.restore();
  }
});

test('useCalendarStrip: a resize under a held owner waits for the release', async () => {
  const observer = captureResizeObserver();

  try {
    let latest: CalendarStrip | null = null;
    const strip = await mountParkedStrip((s) => {
      latest = s;
    });
    const scroller = strip.scrollerRef.current;
    const grid = strip.gridColumnRef.current;
    assert.ok(scroller);
    assert.ok(grid);
    const left0 = scroller!.scrollLeft;
    const start0 = latestStrip(() => latest).visibleStart;

    // A drag owns `scrollLeft`: the re-park must not fight its writes.
    await act(async () => {
      strip.lockStrip('drag');
    });

    stubClientWidth(grid!, 1200);
    const colW1 = colWidth(1200, strip.periodLength);
    assert.notEqual(
      colW1,
      strip.colW,
      'the fixture must change the column width',
    );

    const measure = observer.callbacks[0];
    assert.ok(measure, 'the hook should have registered a ResizeObserver');
    await act(async () => {
      measure([], {} as ResizeObserver);
    });
    await act(async () => {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => resolve());
      });
    });
    assert.equal(
      scroller!.scrollLeft,
      left0,
      'a held owner is not fought, whatever the column width',
    );

    // The release lands it: the visible day at the new width, not a rounding of
    // the old pixels by that width.
    await act(async () => {
      latestStrip(() => latest).unlockStrip('drag');
    });
    assert.equal(
      scroller!.scrollLeft,
      scrollLeftForIndex(STRIP_OVERSCAN, colW1),
      'the deferred re-park owns the landing',
    );
    assert.ok(
      isSameDay(latestStrip(() => latest).visibleStart, start0),
      'the visible day must not move',
    );
  } finally {
    observer.restore();
  }
});

// ── Single-axis gestures ────────────────────────────────────────────────
// One finger may only move one axis. `overflow-x: hidden` denies the browser
// the strip's x axis; the vertical pin denies it the y axis while a swipe owns
// the strip, so a native pan that lands anyway cannot move the hours.

test('useCalendarStrip: a horizontal swipe pins the vertical axis until it settles', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const top0 = scroller!.scrollTop;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 400, clientY: 300 }], 16),
    );
  });
  assert.equal(scroller!.style.scrollSnapType, 'none');

  // The browser starts a vertical pan under the swipe: it is undone, not
  // published, so the gesture stays horizontal.
  scroller!.scrollTop = top0 + 140;
  await act(async () => {
    strip.onScrollerScroll();
  });
  assert.equal(scroller!.scrollTop, top0, 'the hours are held for the gesture');

  // The pin outlives the finger: it holds through the release tween.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 200));
  });
  scroller!.scrollTop = top0 + 90;
  await act(async () => {
    strip.onScrollerScroll();
  });
  assert.equal(scroller!.scrollTop, top0);

  await act(async () => {
    await sleep(400);
  });
  assert.equal(scroller!.style.scrollSnapType, '', 'the tween landed');

  // Once settled the vertical axis is the browser's again.
  scroller!.scrollTop = top0 + 90;
  await act(async () => {
    strip.onScrollerScroll();
  });
  assert.equal(scroller!.scrollTop, top0 + 90);
});

// ── Wheel axis latch ────────────────────────────────────────────────────
// A Mac trackpad adds a small horizontal delta to a vertical two-finger
// scroll. Mandatory day snap then walks the strip onto the neighbouring day,
// and — unlike a touch gesture — there is no finger-up to settle on. A wheel
// has no `touch-action`, so the sideways component has to be cancelled by the
// handler: the burst shares touch's dead zone, and a pending or Y burst is
// re-applied vertically by hand.

/** jsdom has no Element.scrollBy; mirror the browser's relative vertical write. */
function stubScrollBy(el: HTMLElement): ScrollToOptions[] {
  const calls: ScrollToOptions[] = [];
  Object.defineProperty(el, 'scrollBy', {
    configurable: true,
    value: (options: ScrollToOptions) => {
      calls.push(options);
      el.scrollTop += options.top ?? 0;
    },
  });
  return calls;
}

/** Cancelable, as a real wheel over a scroll container is. */
function wheelEvent(
  init: {
    deltaX?: number;
    deltaY?: number;
    deltaMode?: number;
    ctrlKey?: boolean;
  } = {},
): WheelEvent {
  return new window.WheelEvent('wheel', {
    deltaX: init.deltaX ?? 0,
    deltaY: init.deltaY ?? 0,
    deltaMode: init.deltaMode ?? 0,
    ctrlKey: init.ctrlKey ?? false,
    bubbles: true,
    cancelable: true,
  });
}

test('useCalendarStrip: a ctrl-wheel pinch-zoom is left to the zoom handler', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);

  // Pinch-zoom carries a deltaX too, and useCalendarZoom owns it.
  const zoom = wheelEvent({ ctrlKey: true, deltaX: 3, deltaY: -50 });
  await act(async () => {
    scroller!.dispatchEvent(zoom);
  });

  assert.equal(zoom.defaultPrevented, false, 'zoom wheels stay untouched here');
  assert.deepEqual(scrollByCalls, []);
});

test('useCalendarStrip: a vertical wheel burst never moves the strip sideways', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);
  const left0 = scroller!.scrollLeft;

  // The first event of the burst is in the dead zone: same rules as touch. It
  // is still consumed, and only its vertical part is re-applied by hand, so no
  // sideways delta ever reaches the scroller.
  const deadZone = wheelEvent({ deltaX: 2, deltaY: 6 });
  await act(async () => {
    scroller!.dispatchEvent(deadZone);
  });
  assert.equal(deadZone.defaultPrevented, true, 'dead-zone drift is swallowed');
  assert.deepEqual(scrollByCalls, [{ top: 6, behavior: 'instant' }]);

  // The accumulated burst now resolves to Y, so every event that carries
  // sideways drift applies its vertical part only.
  const events = [
    wheelEvent({ deltaX: 4, deltaY: 40 }),
    wheelEvent({ deltaX: 4, deltaY: 40 }),
    wheelEvent({ deltaX: -4, deltaY: 40 }),
  ];
  await act(async () => {
    for (const ev of events) scroller!.dispatchEvent(ev);
  });

  assert.deepEqual(
    events.map((ev) => ev.defaultPrevented),
    [true, true, true],
    'a vertical burst cancels the sideways component',
  );
  assert.deepEqual(scrollByCalls, [
    { top: 6, behavior: 'instant' },
    { top: 40, behavior: 'instant' },
    { top: 40, behavior: 'instant' },
    { top: 40, behavior: 'instant' },
  ]);
  assert.equal(scroller!.scrollLeft, left0, 'scrollLeft never moves');
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'a wheel burst holds no snap lock: nothing is left to restore',
  );

  // A pause starts a new burst, so this one decides its own axis. A committed
  // horizontal burst is no longer native: the strip cannot be user-scrolled on
  // x, so the handler takes it and holds snap off until the burst settles.
  await act(async () => {
    await sleep(150);
  });
  const horizontal = wheelEvent({ deltaX: -60, deltaY: 4 });
  await act(async () => {
    scroller!.dispatchEvent(horizontal);
  });
  assert.equal(horizontal.defaultPrevented, true, 'a new burst re-decides');
  assert.equal(
    scroller!.scrollLeft,
    left0 - 60,
    'the x burst drives the strip',
  );
  assert.equal(scroller!.style.scrollSnapType, 'none');
  assert.equal(scrollByCalls.length, 4, 'still no vertical write');
});

test('useCalendarStrip: a committed horizontal wheel burst drives the strip, then settles', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);
  const left0 = scroller!.scrollLeft;

  // Under 48px accumulated: the dead zone drops the sideways delta, and with
  // no vertical part there is nothing to re-apply.
  const deadZone = [
    wheelEvent({ deltaX: -20, deltaY: 0 }),
    wheelEvent({ deltaX: -20, deltaY: 0 }),
  ];
  await act(async () => {
    for (const ev of deadZone) scroller!.dispatchEvent(ev);
  });
  assert.deepEqual(
    deadZone.map((ev) => ev.defaultPrevented),
    [true, true],
    'sideways travel under the threshold must not reach the scroller',
  );
  assert.deepEqual(scrollByCalls, []);
  assert.equal(scroller!.scrollLeft, left0, 'no horizontal movement yet');

  // 48px at 16:1 commits the burst to x. The strip is not user-scrollable on
  // that axis, so the handler writes `scrollLeft` itself, under its own snap
  // lock — no vertical write, no native horizontal scroll.
  const committed = [
    wheelEvent({ deltaX: -8, deltaY: 0 }),
    wheelEvent({ deltaX: -80, deltaY: 2 }),
    wheelEvent({ deltaX: 10, deltaY: 0 }),
  ];
  await act(async () => {
    for (const ev of committed) scroller!.dispatchEvent(ev);
  });
  assert.deepEqual(
    committed.map((ev) => ev.defaultPrevented),
    [true, true, true],
    'a committed horizontal burst is consumed and driven by hand',
  );
  // -8 - 80 + 10 = -78px: the strip followed the burst (deltaX < 0 scrolls
  // the strip left, towards earlier days).
  assert.equal(scroller!.scrollLeft, left0 - 78);
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'snap off while the burst drives the strip',
  );
  assert.deepEqual(scrollByCalls, [], 'the x burst never scrolls vertically');

  // A quiet gap lands the burst on a column and re-engages snap.
  await act(async () => {
    await sleep(400);
  });
  assert.equal(
    scroller!.scrollLeft % strip.colW,
    0,
    'the settle lands exactly on a column',
  );
  assert.notEqual(scroller!.scrollLeft, left0, 'the burst changed the day');
  assert.equal(scroller!.style.scrollSnapType, '');
});

test('useCalendarStrip: a mouse wheel with no deltaX behaves exactly as before', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);

  // px and line deltas: an ordinary mouse reports deltaX 0, so nothing is
  // intercepted and the browser keeps doing the scrolling.
  const events = [
    wheelEvent({ deltaY: 100 }),
    wheelEvent({ deltaY: 100, deltaMode: 1 }),
  ];
  await act(async () => {
    for (const ev of events) scroller!.dispatchEvent(ev);
  });

  assert.deepEqual(
    events.map((ev) => ev.defaultPrevented),
    [false, false],
  );
  assert.deepEqual(scrollByCalls, [], 'no manual write for a pure Y wheel');
});

test('useCalendarStrip: a wheel burst under a drag owner is left alone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);

  await act(async () => {
    strip.lockStrip('drag');
  });
  const during = wheelEvent({ deltaX: 4, deltaY: 40 });
  await act(async () => {
    scroller!.dispatchEvent(during);
  });
  assert.equal(during.defaultPrevented, false, 'drag writes scrollLeft itself');
  assert.deepEqual(scrollByCalls, []);

  // Released again: the next burst classifies and the vertical part applies.
  await act(async () => {
    strip.unlockStrip('drag');
  });
  const after = wheelEvent({ deltaX: 4, deltaY: 40 });
  await act(async () => {
    scroller!.dispatchEvent(after);
  });
  assert.equal(after.defaultPrevented, true);
  assert.deepEqual(scrollByCalls, [{ top: 40, behavior: 'instant' }]);
});

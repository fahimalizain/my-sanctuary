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

/** jsdom has no Element.scrollTo; mirror the browser's write to scrollLeft. */
function stubScrollTo(el: HTMLElement): number[] {
  const calls: number[] = [];
  Object.defineProperty(el, 'scrollTo', {
    configurable: true,
    value: (options: ScrollToOptions) => {
      if (typeof options.left !== 'number') return;
      calls.push(options.left);
      el.scrollLeft = options.left;
    },
  });
  return calls;
}

/**
 * Real jsdom TouchEvent, so the hook's `e.touches.length` / `touches[0]` reads
 * behave as they do in a browser. jsdom has no `Touch` constructor, so the
 * points stay plain objects.
 */
function touchEvent(
  type: 'touchstart' | 'touchmove' | 'touchend',
  points: { clientX: number; clientY: number }[],
): Event {
  return new window.TouchEvent(type, {
    touches: points as unknown as Touch[],
  });
}

const sleep = (ms: number): Promise<void> =>
  new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });

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

// ── Touch axis lock ─────────────────────────────────────────────────────
// A vertical swipe through the hours carries a small horizontal component.
// With no axis lock the mandatory day snap pages to the neighbour column, so
// the day is frozen for the gesture and corrected once it settles.

test('useCalendarStrip: a vertical touch gesture holds the day, then restores it', async () => {
  let latest: CalendarStrip | null = null;
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
  // Let the init park land (measured colW).
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip);
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollToCalls = stubScrollTo(scroller!);

  const left0 = scroller!.scrollLeft;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  assert.equal(scroller!.style.scrollSnapType, '');

  // One finger down, then a mostly-vertical drag: dy 60px, dx -6px. The
  // sideways part is what used to page the day on iOS.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 200, clientY: 300 }]),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 194, clientY: 360 }]),
    );
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    'none',
    'a vertical gesture must suspend the day snap',
  );

  // The thumb drifts a whole column sideways while the finger is down.
  scroller!.scrollLeft = left0 + strip.colW;
  await act(async () => {
    strip.onScrollerScroll();
  });
  const held = getLatest();
  assert.ok(held);
  assert.equal(
    held.visibleStart.getTime(),
    strip.visibleStart.getTime(),
    'the visible day stays frozen at left0 while the axis lock holds',
  );

  // Finger up, then the scroll settles.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', []));
  });
  await act(async () => {
    scroller!.dispatchEvent(new window.Event('scrollend'));
  });

  assert.deepEqual(scrollToCalls, [left0], 'restore exactly left0');
  assert.equal(scroller!.scrollLeft, left0);
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'snap re-engages once the drift is gone',
  );
});

test('useCalendarStrip: settle falls back to the quiet period without scrollend', async () => {
  // iOS Safari only recently shipped `scrollend`; the quiet timer must finish
  // the gesture on its own.
  let latest: CalendarStrip | null = null;
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
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip);
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollToCalls = stubScrollTo(scroller!);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 200, clientY: 300 }]),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 198, clientY: 340 }]),
    );
  });
  scroller!.scrollLeft = left0 + 40;
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', []));
  });

  // No restore while the settle is pending...
  assert.deepEqual(scrollToCalls, []);
  assert.equal(scroller!.style.scrollSnapType, 'none');

  // ...and each scroll event restarts the quiet period (momentum).
  await act(async () => {
    scroller!.dispatchEvent(new window.Event('scroll'));
  });
  await act(async () => {
    await sleep(250);
  });

  assert.deepEqual(scrollToCalls, [left0]);
  assert.equal(scroller!.scrollLeft, left0);
  assert.equal(scroller!.style.scrollSnapType, '');
});

test('useCalendarStrip: a horizontal touch gesture leaves native day paging alone', async () => {
  let latest: CalendarStrip | null = null;
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
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip);
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollToCalls = stubScrollTo(scroller!);
  const left0 = scroller!.scrollLeft;

  // Clearly horizontal: dx 100px, dy 20px (5:1) → the browser pages the days.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 200, clientY: 300 }]),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 300, clientY: 320 }]),
    );
  });
  assert.equal(
    scroller!.style.scrollSnapType,
    '',
    'a horizontal swipe must keep the day snap',
  );

  // The browser snapped onto the next column; no restore on lift.
  scroller!.scrollLeft = left0 + strip.colW;
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', []));
  });
  await act(async () => {
    await sleep(200);
  });

  assert.deepEqual(scrollToCalls, []);
  assert.equal(scroller!.scrollLeft, left0 + strip.colW);
});

test('useCalendarStrip: drag taking the strip drops the axis lock without restoring', async () => {
  let latest: CalendarStrip | null = null;
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
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip);
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollToCalls = stubScrollTo(scroller!);
  const left0 = scroller!.scrollLeft;

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 200, clientY: 300 }]),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 196, clientY: 380 }]),
    );
  });
  assert.equal(scroller!.style.scrollSnapType, 'none');
  scroller!.scrollLeft = left0 + strip.colW;

  // Drag claims the strip (long-press on a chip): the axis gesture is dropped,
  // with no restore, so drag auto-scroll owns scrollLeft.
  await act(async () => {
    strip.lockStrip('drag');
  });
  await act(async () => {
    await sleep(200);
  });
  assert.deepEqual(scrollToCalls, [], 'no restore under the drag owner');
  assert.equal(scroller!.scrollLeft, left0 + strip.colW);

  await act(async () => {
    strip.unlockStrip('drag');
  });
  assert.equal(scroller!.style.scrollSnapType, '');
});

// ── Wheel axis latch ────────────────────────────────────────────────────
// A Mac trackpad adds a small horizontal delta to a vertical two-finger
// scroll. Mandatory day snap then walks the strip onto the neighbouring day,
// and — unlike a touch gesture — there is no finger-up to settle on. The
// sideways component therefore has to be cancelled before the scroller sees
// it, so the axis is latched per burst and a Y burst is re-applied vertically.

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

/** Render the harness, let the init park land, and hand back the strip. */
async function mountParkedStrip(): Promise<CalendarStrip> {
  let latest: CalendarStrip | null = null;
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
  await act(async () => {
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
    });
  });

  const strip = getLatest();
  assert.ok(strip, 'harness should report strip state');
  return strip;
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

  // First event of the burst is under the slop: native, and at most half a
  // column of drift, so mandatory snap settles back on the same day.
  const underSlop = wheelEvent({ deltaX: 2, deltaY: 6 });
  await act(async () => {
    scroller!.dispatchEvent(underSlop);
  });
  assert.equal(
    underSlop.defaultPrevented,
    false,
    'sub-slop drift stays native',
  );
  assert.deepEqual(scrollByCalls, []);

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

  // A pause starts a new burst, so this one decides its own (horizontal) axis
  // and the strip pages natively again.
  await act(async () => {
    await sleep(150);
  });
  const horizontal = wheelEvent({ deltaX: -60, deltaY: 4 });
  await act(async () => {
    scroller!.dispatchEvent(horizontal);
  });
  assert.equal(horizontal.defaultPrevented, false, 'a new burst re-decides');
  assert.equal(scrollByCalls.length, 3);
});

test('useCalendarStrip: a horizontal wheel burst keeps native day paging', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const scrollByCalls = stubScrollBy(scroller!);

  // Clear 15:1 horizontal start, then momentum: both stay native.
  const events = [
    wheelEvent({ deltaX: -60, deltaY: 4 }),
    wheelEvent({ deltaX: -30, deltaY: 2 }),
  ];
  await act(async () => {
    for (const ev of events) scroller!.dispatchEvent(ev);
  });

  assert.deepEqual(
    events.map((ev) => ev.defaultPrevented),
    [false, false],
    'horizontal scrolling and snap paging must stay native',
  );
  assert.deepEqual(scrollByCalls, []);
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

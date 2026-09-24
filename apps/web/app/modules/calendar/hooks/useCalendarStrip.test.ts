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

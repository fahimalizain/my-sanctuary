// Regression: first visit must show today's fitted period, not the overscan
// buffer week. DOM must exist before the hook module is imported.
//
// Failure mode (pre-fix): mainWidth starts at 0 → colWidth fallback 16px →
// init parks the strip at STRIP_OVERSCAN * 16 and sets didInitScrollRef. When
// measure lands, the park is skipped; the publisher then sets visibleStartIdx
// to 0 so the title shows windowStart … windowStart+periodLength (e.g. Aug
// 31–Sep 6) instead of Mon–Sun containing today.
//
// The strip obeys one rule: a gesture picks its axis once (after the dead zone)
// and only that axis moves until the gesture ends. The browser pans the hours
// (`touch-action: pan-y`); the day strip is a transform written by this hook
// alone, has no CSS scroll-snap, and the release tween is its only snap.
//
// The offset is deliberately not `scrollLeft`: on iOS a native vertical pan
// used to overwrite a programmatic `scrollLeft` with 0 on the hidden x axis,
// rewinding the calendar through history. Tests read the offset back out of the
// transform every registered day track carries.

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

/** `translate3d(<x>px, 0, 0)` as written by the hook — one track's offset. */
const TRACK_TRANSFORM_RE = /^translate3d\((-?[\d.e+-]+)px, 0, 0\)$/;

/** Strip offset (px) parsed back out of one track's transform. */
function trackOffset(track: HTMLElement): number {
  const match = TRACK_TRANSFORM_RE.exec(track.style.transform);
  assert.ok(
    match,
    `a strip track must carry its offset, got "${track.style.transform}"`,
  );
  const offset = -Number.parseFloat(match[1]);
  // `-0` is not `0` under strict equality, and a park at 0 lands on `-0`.
  return offset === 0 ? 0 : offset;
}

/**
 * Strip offset (px) every registered day track carries. The scroller never
 * carries a horizontal scroll position, and the tracks are written together —
 * a disagreement is a bug in its own right, so it is asserted on every read.
 */
function readStripX(scroller: HTMLElement): number {
  const tracks = [
    ...scroller.querySelectorAll<HTMLElement>('[data-strip-track]'),
  ];
  assert.equal(tracks.length, 3, 'the harness renders all three day tracks');
  const offsets = tracks.map(trackOffset);
  for (const offset of offsets) {
    assert.equal(offset, offsets[0], 'every track must carry the same offset');
  }
  return offsets[0];
}

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
 * make nondeterministic. Cancelable, like a touchmove the browser has not
 * committed to a scroll: the swipe has to be able to preventDefault it.
 */
function touchEvent(
  type: 'touchstart' | 'touchmove' | 'touchend',
  points: { clientX: number; clientY: number }[],
  time?: number,
): Event {
  const event = new window.TouchEvent(type, {
    touches: points as unknown as Touch[],
    cancelable: true,
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

/** One of the three bands a real grid registers; the hook translates it. */
function StripTrack({
  trackRef,
}: {
  trackRef: CalendarStrip['stripTrackRef'];
}): ReactElement {
  return createElement('div', { 'data-strip-track': '', ref: trackRef });
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
    createElement(
      'div',
      {
        ref: (el: HTMLDivElement | null) => {
          (strip.scrollerRef as { current: HTMLDivElement | null }).current =
            el;
          if (el && !scrollerAttached.current) {
            stubClientWidth(el, gridWidth);
            scrollerAttached.current = true;
          }
        },
        style: { overflow: 'auto', width: gridWidth },
      },
      // The three bands the real grid mounts: day headers, all-day, hours.
      createElement(StripTrack, {
        key: 'headers',
        trackRef: strip.stripTrackRef,
      }),
      createElement(StripTrack, {
        key: 'all-day',
        trackRef: strip.stripTrackRef,
      }),
      createElement(StripTrack, {
        key: 'hours',
        trackRef: strip.stripTrackRef,
      }),
    ),
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
    readStripX(scroller!),
    scrollLeftForIndex(STRIP_OVERSCAN, measuredColW),
  );
});

test('useCalendarStrip: a mount scroll before the measured park does not clobber visibleStartIdx', async () => {
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

  // Simulate a mount scroll with no measurable width: no park has run, so the
  // offset is still 0 — the publisher must not turn that into index 0.
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  await act(async () => {
    scroller!.dispatchEvent(new window.Event('scroll'));
  });

  const after = getLatest();
  assert.ok(after);
  assert.equal(
    after.visibleStart.getTime(),
    before,
    'a pre-init scroll must not move visibleStart off the seeded overscan index',
  );
  assert.equal(readStripX(scroller!), 0, 'nothing may park before the measure');
  assert.ok(
    isSameDay(after.visibleStart, fitPeriodStart(new Date(), DAYS_PER_PERIOD)),
  );
});

// ── Strip tracks ────────────────────────────────────────────────────────
// The offset reaches the DOM as a `transform` written straight to each of the
// three registered tracks. It is deliberately not an inherited custom property
// on the scroller: a variable there invalidated style for the whole calendar
// subtree — every column, chip and hour line — on every touchmove and tween
// frame, which is what made horizontal swipes lag.

test('useCalendarStrip: every day track carries the same offset', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const parked = readStripX(scroller!);
  assert.ok(parked > 0, 'init park should have moved the tracks');

  await act(async () => {
    strip.scrollStripBy(53);
  });

  const tracks = [
    ...scroller!.querySelectorAll<HTMLElement>('[data-strip-track]'),
  ];
  assert.equal(tracks.length, 3, 'headers, all-day and hours are registered');
  assert.deepEqual(
    tracks.map(trackOffset),
    [parked + 53, parked + 53, parked + 53],
    'one write, one offset: the bands move together',
  );
});

test('useCalendarStrip: a track registered after a move lands on the current offset', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const parked = readStripX(scroller!);
  assert.ok(parked > 0, 'init park should have moved the tracks');

  // A track that mounts (or remounts on a period change) must not paint at 0
  // while the other bands sit at the strip's position.
  const late = document.createElement('div');
  const detach = strip.stripTrackRef(late);
  assert.equal(trackOffset(late), parked, 'attach applies the current offset');

  await act(async () => {
    strip.scrollStripBy(40);
  });
  assert.equal(trackOffset(late), parked + 40, 'later writes reach it too');

  // React 19 detach: the cleanup unregisters the element.
  assert.equal(typeof detach, 'function');
  if (typeof detach === 'function') detach();
  await act(async () => {
    strip.scrollStripBy(40);
  });
  assert.equal(
    trackOffset(late),
    parked + 40,
    'a detached track is no longer written',
  );
});

test('useCalendarStrip: the scroller carries no strip custom property', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  assert.ok(readStripX(scroller!) > 0, 'the strip must have moved');

  const inlineProps = (el: HTMLElement): string[] =>
    Array.from({ length: el.style.length }, (_, i) => el.style.item(i));
  assert.deepEqual(
    inlineProps(scroller!).filter((name) => name.startsWith('--')),
    [],
    'the scroller must not carry an inline custom property',
  );
  for (const el of scroller!.querySelectorAll<HTMLElement>('*')) {
    assert.equal(
      el.style.getPropertyValue('--strip-x'),
      '',
      'the offset may not travel through an inherited variable',
    );
  }
});

// ── iOS vertical pan (regression) ───────────────────────────────────────
// On iOS a native vertical pan makes WebKit report its own x for a
// non-user-scrollable axis. With `overflow-x: hidden` that overwrote the
// programmatic `scrollLeft` with 0 mid-gesture; the hook published index 0,
// rebased a period back, and the calendar rewound through history on every
// vertical drag. The strip is a transform now and a native scroll carries no x
// at all, so the browser cannot move it.

test('useCalendarStrip: a native scroll event cannot rewind the strip', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const parked = readStripX(scroller!);
  const start0 = latestStrip(() => latest).visibleStart;
  const days0 = latestStrip(() => latest).days[0];
  assert.ok(parked > 0, 'init park should have parked the strip mid-window');

  // What iOS reports while the finger scrolls vertically: x is 0, whatever the
  // strip's real offset. A scroll event follows each frame.
  Object.defineProperty(scroller!, 'scrollLeft', {
    configurable: true,
    get: () => 0,
  });

  try {
    for (const scrollTop of [40, 120, 260]) {
      scroller!.scrollTop = scrollTop;
      await act(async () => {
        scroller!.dispatchEvent(new window.Event('scroll'));
      });

      assert.equal(
        readStripX(scroller!),
        parked,
        'a native scroll must not move the strip',
      );
      assert.equal(
        latestStrip(() => latest).visibleStart.getTime(),
        start0.getTime(),
        'the visible day must not move',
      );
      assert.ok(
        isSameDay(latestStrip(() => latest).days[0], days0),
        'and the window must not rebase',
      );
    }
  } finally {
    Reflect.deleteProperty(scroller!, 'scrollLeft');
  }
});

// ── Touch swipe ─────────────────────────────────────────────────────────
// The scroller is `touch-action: pan-y` with no user-scrollable x axis, so a
// vertical gesture is native and the strip never moves sideways. A horizontal
// swipe is driven by hand and has a dead zone: the strip stays put until the
// gesture is a deliberate SCROLL_X_THRESHOLD_PX across, and from that moment
// the swipe owns the touch — every later move is preventDefault-ed, so the
// same finger cannot start a vertical pan halfway through.

test('useCalendarStrip: a vertical touch never moves the strip sideways', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);
  const start0 = latestStrip(() => latest).visibleStart;
  assert.ok(left0 > 0, 'init park should have scrolled the strip');

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });

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
    const event = touchEvent(
      'touchmove',
      [{ clientX: move.x, clientY: move.y }],
      move.t,
    );
    await act(async () => {
      scroller!.dispatchEvent(event);
    });
    assert.equal(
      readStripX(scroller!),
      left0,
      `a vertical gesture must never move the strip (move to ${move.x}, ${move.y})`,
    );
    // The vertical pan is the browser's: cancelling it would freeze the hours.
    assert.equal(
      event.defaultPrevented,
      false,
      'a y gesture must reach the native pan untouched',
    );
  }

  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 80));
  });
  assert.equal(readStripX(scroller!), left0);
  assert.ok(
    isSameDay(latestStrip(() => latest).visibleStart, start0),
    'the visible day must not follow a time scroll',
  );
});

test('useCalendarStrip: a horizontal move under the threshold leaves the strip alone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });
  // 47px sideways: one pixel short of the deliberate swipe. The dead zone is
  // not an axis decision, so the event still reaches the browser.
  const deadZone = touchEvent(
    'touchmove',
    [{ clientX: 253, clientY: 300 }],
    16,
  );
  await act(async () => {
    scroller!.dispatchEvent(deadZone);
  });

  assert.equal(readStripX(scroller!), left0, 'the dead zone must not move');
  assert.equal(deadZone.defaultPrevented, false, 'nothing is claimed yet');

  // The same touch turning vertical must not move it either.
  const vertical = touchEvent(
    'touchmove',
    [{ clientX: 250, clientY: 420 }],
    32,
  );
  await act(async () => {
    scroller!.dispatchEvent(vertical);
  });
  assert.equal(readStripX(scroller!), left0, 'still no day movement');
  assert.equal(vertical.defaultPrevented, false);

  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 200));
  });
  assert.equal(readStripX(scroller!), left0);
});

test('useCalendarStrip: past the threshold the strip follows minus the dead zone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

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
  assert.equal(readStripX(scroller!), left0 - 48);

  // dx 150 → left0 - 102: continuous with the previous move, no second jump.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 450, clientY: 300 }], 32),
    );
  });
  assert.equal(readStripX(scroller!), left0 - 102);
});

test('useCalendarStrip: after the lock the strip follows 1:1 past the origin', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

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
  assert.equal(readStripX(scroller!), left0 - 12);

  // Back to +10: the finger moved 50px left, so the strip is back at
  // left0 − 12 + 48 = left0 + 38. No second dead zone.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 310, clientY: 300 }], 32),
    );
  });
  assert.equal(readStripX(scroller!), left0 + 38);

  // Past the origin to −10: left0 + 58. Keeping the lock-time direction is what
  // avoids the ~96px jump a per-move `sign(dx)` would cause at dx = 0.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 290, clientY: 300 }], 48),
    );
  });
  assert.equal(readStripX(scroller!), left0 + 58);
});

test('useCalendarStrip: an x swipe cancels every later move of its gesture', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 300, clientY: 300 }], 0),
    );
  });

  // Still in the dead zone: not cancelled, and nothing moves.
  const pending = touchEvent('touchmove', [{ clientX: 280, clientY: 300 }], 8);
  await act(async () => {
    scroller!.dispatchEvent(pending);
  });
  assert.equal(pending.defaultPrevented, false);
  assert.equal(readStripX(scroller!), left0);

  // dx −60 commits the gesture to x → left0 + 12, and the move is cancelled.
  const committed = touchEvent(
    'touchmove',
    [{ clientX: 240, clientY: 300 }],
    16,
  );
  await act(async () => {
    scroller!.dispatchEvent(committed);
  });
  assert.equal(committed.defaultPrevented, true);
  assert.equal(readStripX(scroller!), left0 + 12);

  // A diagonal move that would classify as y if the axis were re-decided: the
  // gesture already picked x, so it stays horizontal and stays cancelled.
  const diagonal = touchEvent(
    'touchmove',
    [{ clientX: 290, clientY: 340 }],
    32,
  );
  await act(async () => {
    scroller!.dispatchEvent(diagonal);
  });
  assert.equal(
    diagonal.defaultPrevented,
    true,
    'one gesture, one axis: the browser gets nothing once x is claimed',
  );
  assert.equal(readStripX(scroller!), left0 - 38);
});

test('useCalendarStrip: release tweens onto a column', async () => {
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

  await act(async () => {
    await sleep(400);
  });

  // left0 − 112 = 630; the nearest column is 6 (6 × 106 = 636).
  assert.equal(
    readStripX(scroller!),
    scrollLeftForIndex(6, strip.colW),
    'the tween must land exactly on a column',
  );
  assert.equal(
    readStripX(scroller!) % strip.colW,
    0,
    'no float left between two days',
  );
});

test('useCalendarStrip: a flick carries the strip past the nearest column', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 400, clientY: 300 }], 0),
    );
  });
  // Two quick moves to the left, 60px apart in 50ms: the release velocity
  // projects forward, past where the finger actually left the strip.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 340, clientY: 300 }], 200),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 280, clientY: 300 }], 250),
    );
  });
  const releasedLeft = readStripX(scroller!);
  const nearestIdx = Math.round(releasedLeft / strip.colW);

  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 260));
  });
  await act(async () => {
    await sleep(400);
  });

  assert.equal(readStripX(scroller!) % strip.colW, 0, 'lands on a column');
  assert.ok(
    readStripX(scroller!) > scrollLeftForIndex(nearestIdx, strip.colW),
    'a flick carries the strip past the nearest column (later days)',
  );
  assert.notEqual(readStripX(scroller!), releasedLeft);
});

test('useCalendarStrip: a swipe that lands on the last column rebases, keeping that day', async () => {
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);
  const start0 = latestStrip(() => latest).visibleStart;
  const days0 = latestStrip(() => latest).days[0];

  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 400, clientY: 300 }], 0),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 300, clientY: 300 }], 16),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 250, clientY: 300 }], 32),
    );
  });
  // A fast flick: the projection clamps at one period, which is the last
  // column the window can show before it has to slide.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 40));
  });
  await act(async () => {
    await sleep(400);
  });

  // The window slid one period and compensated the pixels, so the strip is back
  // at the park offset — but on the day the swipe actually landed on.
  assert.equal(
    readStripX(scroller!),
    left0,
    'the rebase re-parks at the same offset in the new window',
  );
  assert.ok(
    isSameDay(
      latestStrip(() => latest).days[0],
      addDays(days0, strip.periodLength),
    ),
    'the rendered window slid one period',
  );
  assert.ok(
    isSameDay(
      latestStrip(() => latest).visibleStart,
      addDays(start0, strip.periodLength),
    ),
    'the rebase keeps the day the swipe landed on',
  );
});

test('useCalendarStrip: a touch that interrupts a tween lifts onto a column', async () => {
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
      touchEvent('touchmove', [{ clientX: 240, clientY: 300 }], 16),
    );
  });
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 32));
  });
  await act(async () => {
    await sleep(50);
  });

  // Grab the strip mid-tween: the tween is dropped where it stands, and the
  // lock stays held so the strip stays between days under the finger.
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchstart', [{ clientX: 500, clientY: 300 }], 200),
    );
  });
  const grabbed = readStripX(scroller!);
  await act(async () => {
    scroller!.dispatchEvent(
      touchEvent('touchmove', [{ clientX: 495, clientY: 320 }], 216),
    );
  });
  assert.equal(readStripX(scroller!), grabbed, 'a pending move never moves it');

  // Lifting without an axis lands on the nearest column instead of leaving the
  // strip stranded halfway between two days.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 220));
  });
  await act(async () => {
    await sleep(400);
  });
  assert.equal(
    readStripX(scroller!) % strip.colW,
    0,
    'the interrupted tween still lands on a column',
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
  const followed = readStripX(scroller!);

  // A claimed drag (long-press on a chip) owns the strip now: the swipe is
  // dropped with no tween.
  await act(async () => {
    strip.lockStrip('drag');
  });
  assert.equal(
    readStripX(scroller!),
    followed,
    'dropping the swipe must not move the strip',
  );

  // A lift after the takeover must not settle the dropped swipe.
  await act(async () => {
    scroller!.dispatchEvent(touchEvent('touchend', [], 200));
  });
  await act(async () => {
    await sleep(300);
  });
  assert.equal(
    readStripX(scroller!),
    followed,
    'no release tween runs after the drag took the strip',
  );

  // The drag's own release lands the strip by hand, since no tween ran.
  await act(async () => {
    strip.unlockStrip('drag');
  });
  assert.equal(
    readStripX(scroller!) % strip.colW,
    0,
    'the drag release lands the strip on a column',
  );
});

test('useCalendarStrip: releasing a drag lands the strip on a column', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);

  await act(async () => {
    strip.lockStrip('drag');
  });
  // Drag autoscroll leaves the strip between two days. Mid-window, so the
  // release is a plain landing: an edge would also rebase the window, and that
  // is what the swipe tests cover.
  const targetIdx = STRIP_OVERSCAN + 1;
  await act(async () => {
    strip.scrollStripBy(
      scrollLeftForIndex(targetIdx, strip.colW) + 40 - readStripX(scroller!),
    );
  });
  await act(async () => {
    strip.unlockStrip('drag');
  });

  assert.equal(
    readStripX(scroller!),
    scrollLeftForIndex(targetIdx, strip.colW),
    'the nearest column, not the mid-column position autoscroll left',
  );
});

test('useCalendarStrip: shiftPeriod is a plain write of the strip', async () => {
  // A three-day period: one period away from the parked overscan column stays
  // clear of the rebase threshold, so the write itself is observable (a 7-day
  // shift rebases and re-parks the strip, which would hide the assertion).
  setPeriodLength(3);
  let latest: CalendarStrip | null = null;
  const strip = await mountParkedStrip((s) => {
    latest = s;
  });
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);
  assert.ok(left0 > 0, 'init park should have scrolled the strip');
  assert.equal(strip.periodLength, 3);
  const start0 = latestStrip(() => latest).visibleStart;

  try {
    await act(async () => {
      strip.shiftPeriod(1);
    });

    assert.equal(
      readStripX(scroller!),
      left0 + strip.periodLength * strip.colW,
      'the shift moves the strip one period of pixels',
    );
    assert.ok(
      isSameDay(
        latestStrip(() => latest).visibleStart,
        addDays(start0, strip.periodLength),
      ),
      'the visible period moved forward by one',
    );
  } finally {
    cleanup();
    // The store is shared with every other test in this file (and localStorage).
    setPeriodLength(DAYS_PER_PERIOD);
  }
});

// ── Column-width changes ────────────────────────────────────────────────
// The park is pixels: a rotation, window resize, sidebar or PWA viewport move
// the columns out from under it, so the same visible index has to be re-parked
// at the new width — a re-park of the old pixels rounds to a different day.

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

    const left0 = readStripX(scroller!);
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
      readStripX(scroller!),
      scrollLeftForIndex(STRIP_OVERSCAN, colW1),
      'the same index, re-parked at the new column width',
    );
    assert.notEqual(
      readStripX(scroller!),
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
    const left0 = readStripX(scroller!);
    const start0 = latestStrip(() => latest).visibleStart;

    // A drag moves the strip: the re-park must not fight its moves.
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
      readStripX(scroller!),
      left0,
      'a held owner is not fought, whatever the column width',
    );

    // The release lands it: the visible day at the new width, not a rounding of
    // the old pixels by that width.
    await act(async () => {
      latestStrip(() => latest).unlockStrip('drag');
    });
    assert.equal(
      readStripX(scroller!),
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

// ── Wheel axis latch ────────────────────────────────────────────────────
// A Mac trackpad adds a small horizontal delta to a vertical two-finger scroll,
// and — unlike a touch gesture — there is no finger-up to settle on. A wheel
// has no `touch-action`, so the burst shares touch's dead zone: only a
// deliberate, horizontal-dominant gesture drives the strip, and a vertical one
// is left entirely to the browser.

/** jsdom has no WheelEvent scrolling; the strip's own writes are what we read. */
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
  const left0 = readStripX(scroller!);

  // Pinch-zoom carries a deltaX too, and useCalendarZoom owns it.
  const zoom = wheelEvent({ ctrlKey: true, deltaX: 3, deltaY: -50 });
  await act(async () => {
    scroller!.dispatchEvent(zoom);
  });

  assert.equal(zoom.defaultPrevented, false, 'zoom wheels stay untouched here');
  assert.equal(readStripX(scroller!), left0);
});

test('useCalendarStrip: a vertical wheel burst is left to the browser', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  // The first event of the burst is in the dead zone, with a touch of sideways
  // drift: the burst is still vertical-dominant, so the event is the browser's.
  // Its sideways part cannot move the strip (`overflow-x: hidden`) and its
  // vertical part keeps the native momentum.
  const deadZone = wheelEvent({ deltaX: 2, deltaY: 6 });
  await act(async () => {
    scroller!.dispatchEvent(deadZone);
  });
  assert.equal(deadZone.defaultPrevented, false);

  // The accumulated burst resolves to y, so the rest of it stays native too.
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
    [false, false, false],
    'a vertical burst is never consumed',
  );
  assert.equal(readStripX(scroller!), left0, 'the strip never moves');
});

test('useCalendarStrip: a sideways burst inside the dead zone is consumed and moves nothing', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  // Sideways-dominant but under the threshold: this is the gesture a Mac would
  // otherwise turn into a history swipe, so it is cancelled — and the strip
  // still does not move.
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
    'a sideways gesture must not escape as a history swipe',
  );
  assert.equal(readStripX(scroller!), left0, 'no horizontal movement yet');

  // The next event takes the burst over the threshold: it commits the axis and
  // the strip follows that event's delta only (the dead zone is never caught up).
  const committed = wheelEvent({ deltaX: -8, deltaY: 0 });
  await act(async () => {
    scroller!.dispatchEvent(committed);
  });
  assert.equal(committed.defaultPrevented, true);
  assert.equal(readStripX(scroller!), left0 - 8, 'only the committed delta');
});

test('useCalendarStrip: a committed horizontal wheel burst drives the strip, then settles', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  // 48px at 16:1 commits the burst to x in a single event. The x axis is not
  // user-scrollable, so the handler moves the strip itself under its own
  // lock.
  const committed = [
    wheelEvent({ deltaX: -60, deltaY: 4 }),
    wheelEvent({ deltaX: -18, deltaY: 0 }),
  ];
  await act(async () => {
    for (const ev of committed) scroller!.dispatchEvent(ev);
  });
  assert.deepEqual(
    committed.map((ev) => ev.defaultPrevented),
    [true, true],
    'a committed horizontal burst is consumed and driven by hand',
  );
  // -60 - 18 = -78px: deltaX < 0 scrolls the strip left, towards earlier days.
  assert.equal(readStripX(scroller!), left0 - 78);

  // A quiet gap lands the burst on a column: the wheel has no finger-up.
  await act(async () => {
    await sleep(400);
  });
  assert.equal(
    readStripX(scroller!) % strip.colW,
    0,
    'the settle lands exactly on a column',
  );
  assert.notEqual(readStripX(scroller!), left0, 'the burst changed the day');
});

test('useCalendarStrip: a mouse wheel with no deltaX behaves exactly as before', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

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
  assert.equal(readStripX(scroller!), left0);
});

test('useCalendarStrip: a wheel burst under a drag owner is left alone', async () => {
  const strip = await mountParkedStrip();
  const scroller = strip.scrollerRef.current;
  assert.ok(scroller);
  const left0 = readStripX(scroller!);

  await act(async () => {
    strip.lockStrip('drag');
  });
  const during = wheelEvent({ deltaX: -60, deltaY: 4 });
  await act(async () => {
    scroller!.dispatchEvent(during);
  });
  assert.equal(during.defaultPrevented, false, 'drag moves the strip itself');
  assert.equal(readStripX(scroller!), left0);

  // Released again: the next burst is classified and driven by the handler.
  await act(async () => {
    strip.unlockStrip('drag');
  });
  const after = wheelEvent({ deltaX: -60, deltaY: 4 });
  await act(async () => {
    scroller!.dispatchEvent(after);
  });
  assert.equal(after.defaultPrevented, true);
  assert.equal(readStripX(scroller!), left0 - 60);
});

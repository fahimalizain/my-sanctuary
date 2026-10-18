// A gutter precision handle (the start/end pill) must not move the hours.
// Autoscroll, a trackpad wheel, and a touch pan all used to: a pill stuck to
// the viewport edge already sits inside the autoscroll zone, so grabbing it
// flung the day while the 1:10 edge barely moved. The freeze is scoped to the
// hours — the sidebar and the inspector keep scrolling — and it cancels the
// ctrl/meta zoom wheel before the scroller's own listener sees it.
//
// DOM must exist before the hook module is imported — installDom() then a
// dynamic import (static ESM imports hoist and can race the DOM setup).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type PointerEvent as ReactPointerEvent } from 'react';
import type { CalendarEvent } from '@/app/types';
import { installDom } from '../components/install-dom';

installDom();

type CalendarDragApi = import('./useCalendarDrag').CalendarDragApi;

let useCalendarDrag: typeof import('./useCalendarDrag').useCalendarDrag;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;
let act: typeof import('@testing-library/react').act;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  act = rtl.act;
  useCalendarDrag = (await import('./useCalendarDrag')).useCalendarDrag;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

function Harness(props: { api: { current: CalendarDragApi | null } }) {
  const drag = useCalendarDrag({
    hourH: 48,
    lockStrip: () => {},
    unlockStrip: () => {},
    onClickCreate: () => {},
    onDragCreate: () => {},
    onMove: () => {},
    onResize: () => {},
    onAllDayCreate: () => {},
    onChipTap: () => {},
  });
  props.api.current = drag;
  return null;
}

async function mountDrag(): Promise<CalendarDragApi> {
  const api: { current: CalendarDragApi | null } = { current: null };
  await act(async () => {
    render(createElement(Harness, { api }));
  });
  assert.ok(api.current);
  return api.current;
}

function pointerDown(
  target: HTMLElement,
  clientX: number,
  clientY: number,
): ReactPointerEvent<HTMLElement> {
  return {
    button: 0,
    clientX,
    clientY,
    pointerId: 1,
    pointerType: 'mouse',
    metaKey: false,
    ctrlKey: false,
    timeStamp: 0,
    currentTarget: target,
    stopPropagation() {},
  } as unknown as ReactPointerEvent<HTMLElement>;
}

function stubRect(
  el: HTMLElement,
  box: { top: number; left: number; width: number; height: number },
): void {
  el.getBoundingClientRect = () =>
    ({
      x: box.left,
      y: box.top,
      top: box.top,
      left: box.left,
      right: box.left + box.width,
      bottom: box.top + box.height,
      width: box.width,
      height: box.height,
      toJSON() {
        return {};
      },
    }) as DOMRect;
}

/** jsdom clamps `scrollTop` to 0 on an unscrolled box, so the test owns it. */
function stubScrollTop(
  el: HTMLElement,
  initial: number,
): { read: () => number } {
  let top = initial;
  Object.defineProperty(el, 'scrollTop', {
    configurable: true,
    get: () => top,
    set: (value: number) => {
      top = value;
    },
  });
  return { read: () => top };
}

function mountGrid(scrollTop: number): {
  scroller: HTMLDivElement;
  col: HTMLDivElement;
  readTop: () => number;
  remove: () => void;
} {
  const scroller = document.createElement('div');
  scroller.setAttribute('data-calendar-scroller', '');
  const readTop = stubScrollTop(scroller, scrollTop).read;
  stubRect(scroller, { top: 0, left: 0, width: 400, height: 640 });
  const col = document.createElement('div');
  col.setAttribute('data-day-col', '2026-10-18');
  stubRect(col, { top: 120, left: 52, width: 100, height: 1152 });
  document.body.append(scroller, col);
  // jsdom has no hit-testing. A miss still lets autoscroll run; the pointer
  // path must not throw before the scroll assertion.
  const prevHit = document.elementFromPoint;
  const hadHitTest = typeof prevHit === 'function';
  document.elementFromPoint = () => null;
  return {
    scroller,
    col,
    readTop,
    remove: () => {
      scroller.remove();
      col.remove();
      if (hadHitTest) document.elementFromPoint = prevHit;
      else Reflect.deleteProperty(document, 'elementFromPoint');
    },
  };
}

function timedEvent(): CalendarEvent {
  return {
    id: 'evt-1',
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Focus',
    description: '',
    start_time: new Date(2026, 9, 18, 9, 0, 0, 0).toISOString(),
    end_time: new Date(2026, 9, 18, 10, 0, 0, 0).toISOString(),
    last_synced_at: '2026-10-18T00:00:00.000Z',
  };
}

/**
 * Queue animation frames instead of running them. Flushing a few is enough to
 * catch an autoscroll loop (it re-queues itself) without spinning forever.
 */
function stubAnimationFrame(): {
  flush: (frames: number) => void;
  restore: () => void;
} {
  const queued = new Map<number, FrameRequestCallback>();
  let nextId = 1;
  const prev = {
    raf: globalThis.requestAnimationFrame,
    cancel: globalThis.cancelAnimationFrame,
    windowRaf: window.requestAnimationFrame,
    windowCancel: window.cancelAnimationFrame,
  };
  const raf: typeof globalThis.requestAnimationFrame = (cb) => {
    const id = nextId++;
    queued.set(id, cb);
    return id;
  };
  const cancel: typeof globalThis.cancelAnimationFrame = (id) => {
    queued.delete(id);
  };
  globalThis.requestAnimationFrame = raf;
  globalThis.cancelAnimationFrame = cancel;
  window.requestAnimationFrame = raf;
  window.cancelAnimationFrame = cancel;
  return {
    flush(frames: number) {
      for (let i = 0; i < frames; i++) {
        const batch = [...queued.values()];
        queued.clear();
        for (const cb of batch) cb(performance.now());
      }
    },
    restore() {
      globalThis.requestAnimationFrame = prev.raf;
      globalThis.cancelAnimationFrame = prev.cancel;
      window.requestAnimationFrame = prev.windowRaf;
      window.cancelAnimationFrame = prev.windowCancel;
    },
  };
}

function wheel(deltaY: number): WheelEvent {
  return new window.WheelEvent('wheel', {
    deltaY,
    bubbles: true,
    cancelable: true,
  });
}

/** The zoom gesture: `useCalendarZoom` acts on ctrl/meta wheel only. */
function ctrlWheel(deltaY: number): WheelEvent {
  return new window.WheelEvent('wheel', {
    deltaY,
    ctrlKey: true,
    bubbles: true,
    cancelable: true,
  });
}

function touchMove(): Event {
  return new window.Event('touchmove', { bubbles: true, cancelable: true });
}

/** jsdom's document is shared by every test in this file. */
function stubVisibilityState(state: DocumentVisibilityState): () => void {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => state,
  });
  return () => Reflect.deleteProperty(document, 'visibilityState');
}

/** The real pill lives in the gutter, i.e. inside the scroller's subtree. */
function mountPill(scroller: HTMLElement): HTMLDivElement {
  const pill = document.createElement('div');
  scroller.append(pill);
  return pill;
}

test('gutter scrub freezes vertical scroll and releases it on cancel', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const pill = document.createElement('div');
  document.body.append(pill);
  try {
    const drag = await mountDrag();
    const top = grid.readTop();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(pill, 20, 300),
        'end',
        timedEvent(),
      );
    });

    const duringWheel = wheel(120);
    grid.scroller.dispatchEvent(duringWheel);
    assert.equal(duringWheel.defaultPrevented, true, 'wheel must not pan');

    const touch = new window.Event('touchmove', {
      bubbles: true,
      cancelable: true,
    });
    grid.scroller.dispatchEvent(touch);
    assert.equal(touch.defaultPrevented, true, 'touch pan must not scroll');

    // Browser drag-autoscroll writes scrollTop with no cancelable event.
    grid.scroller.scrollTop = top + 90;
    grid.scroller.dispatchEvent(new window.Event('scroll'));
    assert.equal(grid.readTop(), top, 'a stray scroll is pinned back');

    await act(async () => {
      window.dispatchEvent(
        new window.PointerEvent('pointermove', {
          pointerId: 1,
          clientX: 20,
          clientY: 640,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    // Pointer is on the bottom edge, where a grid drag autoscrolls.
    frames.flush(3);
    assert.equal(grid.readTop(), top, 'precision scrub does not autoscroll');

    await act(async () => {
      drag.cancel();
    });

    const afterWheel = wheel(120);
    grid.scroller.dispatchEvent(afterWheel);
    assert.equal(afterWheel.defaultPrevented, false);
    grid.scroller.scrollTop = top + 90;
    grid.scroller.dispatchEvent(new window.Event('scroll'));
    assert.equal(grid.readTop(), top + 90, 'scroll works again after release');
  } finally {
    pill.remove();
    grid.remove();
    frames.restore();
  }
});

test('a scroll write that cannot land does not disarm the pin', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const pill = mountPill(grid.scroller);
  // Take the scroll position over from `mountGrid` so a write can be made to
  // fail, which is what a document too short to scroll that far does to the
  // pin. A failed write is not a loop: it fires no scroll event of its own.
  let value = 80;
  let writable = true;
  Object.defineProperty(grid.scroller, 'scrollTop', {
    configurable: true,
    get: () => value,
    set: (next: number) => {
      if (writable) value = next;
    },
  });
  try {
    const drag = await mountDrag();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(pill, 20, 300),
        'end',
        timedEvent(),
      );
    });

    writable = false;
    value = 170;
    grid.scroller.dispatchEvent(new window.Event('scroll'));
    assert.equal(value, 170, 'the write was clamped');

    writable = true;
    value = 250;
    grid.scroller.dispatchEvent(new window.Event('scroll'));
    assert.equal(value, 80, 'the pin survived the clamped write');

    await act(async () => {
      drag.cancel();
    });
  } finally {
    grid.remove();
    frames.restore();
  }
});

test('a grid drag still autoscrolls and does not swallow the wheel', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  try {
    const drag = await mountDrag();
    const top = grid.readTop();
    await act(async () => {
      drag.onColumnPointerDown(
        pointerDown(grid.col, 80, 300),
        new Date(2026, 9, 18),
      );
    });

    const duringWheel = wheel(40);
    grid.scroller.dispatchEvent(duringWheel);
    assert.equal(duringWheel.defaultPrevented, false);

    await act(async () => {
      window.dispatchEvent(
        new window.PointerEvent('pointermove', {
          pointerId: 1,
          clientX: 80,
          clientY: 640,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
    frames.flush(1);
    assert.ok(
      grid.readTop() > top,
      'holding the pointer on the bottom edge scrolls the hours',
    );

    await act(async () => {
      drag.cancel();
    });
  } finally {
    grid.remove();
    frames.restore();
  }
});

test('a scrub cancels the ctrl/meta zoom wheel at the scroller', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const pill = mountPill(grid.scroller);
  // Stands in for useCalendarZoom's wheel listener, which is bound on the
  // scroller itself and rescales the hours.
  const seen: WheelEvent[] = [];
  const onScrollerWheel = (e: Event) => seen.push(e as WheelEvent);
  grid.scroller.addEventListener('wheel', onScrollerWheel, { passive: false });
  try {
    const drag = await mountDrag();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(pill, 20, 300),
        'end',
        timedEvent(),
      );
    });

    const zoom = ctrlWheel(-120);
    pill.dispatchEvent(zoom);
    assert.equal(zoom.defaultPrevented, true, 'ctrl+wheel must not rescale');
    assert.deepEqual(seen, [], 'the zoom handler must never run');

    await act(async () => {
      drag.cancel();
    });

    const after = ctrlWheel(-120);
    pill.dispatchEvent(after);
    assert.deepEqual(seen, [after], 'zoom works again after release');
  } finally {
    grid.scroller.removeEventListener('wheel', onScrollerWheel);
    grid.remove();
    frames.restore();
  }
});

test('a scrub leaves scrolling outside the hours alone', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const inspector = document.createElement('aside');
  document.body.append(inspector);
  try {
    const drag = await mountDrag();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(inspector, 700, 300),
        'end',
        timedEvent(),
      );
    });

    const outsideWheel = wheel(120);
    inspector.dispatchEvent(outsideWheel);
    assert.equal(
      outsideWheel.defaultPrevented,
      false,
      'the inspector keeps its own scroll',
    );
    const outsideTouch = touchMove();
    inspector.dispatchEvent(outsideTouch);
    assert.equal(outsideTouch.defaultPrevented, false);

    // The hold is still a hold: the hours themselves stay frozen.
    const insideWheel = wheel(120);
    grid.scroller.dispatchEvent(insideWheel);
    assert.equal(insideWheel.defaultPrevented, true);

    await act(async () => {
      drag.cancel();
    });
  } finally {
    inspector.remove();
    grid.remove();
    frames.restore();
  }
});

test('losing focus mid-scrub releases the freeze', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const pill = mountPill(grid.scroller);
  try {
    const drag = await mountDrag();
    const top = grid.readTop();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(pill, 20, 300),
        'end',
        timedEvent(),
      );
    });
    const during = wheel(120);
    grid.scroller.dispatchEvent(during);
    assert.equal(during.defaultPrevented, true);

    await act(async () => {
      window.dispatchEvent(new window.Event('blur'));
    });

    const afterBlur = wheel(120);
    grid.scroller.dispatchEvent(afterBlur);
    assert.equal(afterBlur.defaultPrevented, false, 'the hours scroll again');
    grid.scroller.scrollTop = top + 60;
    grid.scroller.dispatchEvent(new window.Event('scroll'));
    assert.equal(grid.readTop(), top + 60, 'a late scroll is not pinned back');

    await act(async () => {
      drag.cancel();
    });
  } finally {
    grid.remove();
    frames.restore();
  }
});

test('a hidden document releases the freeze', async () => {
  const frames = stubAnimationFrame();
  const grid = mountGrid(80);
  const pill = mountPill(grid.scroller);
  const restoreVisibility = stubVisibilityState('hidden');
  try {
    const drag = await mountDrag();
    await act(async () => {
      drag.onGutterEdgePointerDown(
        pointerDown(pill, 20, 300),
        'end',
        timedEvent(),
      );
    });
    const during = wheel(120);
    grid.scroller.dispatchEvent(during);
    assert.equal(during.defaultPrevented, true);

    await act(async () => {
      document.dispatchEvent(new window.Event('visibilitychange'));
    });

    const after = wheel(120);
    grid.scroller.dispatchEvent(after);
    assert.equal(after.defaultPrevented, false, 'the hours scroll again');

    await act(async () => {
      drag.cancel();
    });
  } finally {
    restoreVisibility();
    grid.remove();
    frames.restore();
  }
});

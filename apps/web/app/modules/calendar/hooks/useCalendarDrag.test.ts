// A gutter precision handle (the start/end pill) must not move the hours.
// Autoscroll, a trackpad wheel, and a touch pan all used to: a pill stuck to
// the viewport edge already sits inside the autoscroll zone, so grabbing it
// flung the day while the 1:10 edge barely moved.
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
function stubScrollTop(el: HTMLElement, initial: number): { read: () => number } {
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

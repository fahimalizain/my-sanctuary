// The calendar scroller must have no horizontal scroll range: its content is
// exactly the scroller's own width, and each horizontal band clips a wider day
// track that is translated by the strip offset (`--strip-x`). That is the fix
// for the iOS rewind — a hidden x axis let WebKit overwrite `scrollLeft` mid
// vertical pan.
//
// jsdom computes no layout, so `scrollWidth` cannot meaningfully be compared to
// `clientWidth` here. This pins the structure that guarantees the invariant
// instead: no fixed-width content child, a clipping viewport per band, and all
// three tracks sharing the same transform.
//
// DOM must exist before the component module is imported.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, createRef } from 'react';
import { addDays, startOfDay, STRIP_TRACK_TRANSFORM } from '../lib/week-layout';
import { installDom } from './install-dom';

installDom();

let CalendarWeekGrid: typeof import('./CalendarWeekGrid').CalendarWeekGrid;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  CalendarWeekGrid = (await import('./CalendarWeekGrid')).CalendarWeekGrid;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

const DAY_COUNT = 21;
const COL_W = 106;
const GUTTER_W = 52;

function mountGrid(): {
  scroller: HTMLDivElement;
  days: Date[];
} {
  const days = Array.from({ length: DAY_COUNT }, (_, i) =>
    addDays(startOfDay(new Date(2026, 9, 12)), i),
  );
  const scrollerRef = createRef<HTMLDivElement>();
  const gridColumnRef = createRef<HTMLDivElement>();

  render(
    createElement(CalendarWeekGrid, {
      gridColumnRef,
      scrollerRef,
      colW: COL_W,
      gutterW: GUTTER_W,
      trackWidth: DAY_COUNT * COL_W,
      days,
      today: days[7],
      now: new Date(2026, 9, 19, 12),
      hourH: 48,
      totalHoursH: 1152,
      hourGridBg: '',
      hourLabels: [0, 6, 12, 18],
      gutterEdges: null,
      allDayHeight: 25,
      allDayChips: [],
      todayIndex: 7,
      eventsByDay: new Map(),
      selectedEventId: null,
      draggingEventId: null,
      onColumnPointerDown: () => {},
      onChipPointerDown: () => {},
      onSelectChip: () => {},
      onAllDayPointerDown: () => {},
      onAllDayChipPointerDown: () => {},
      onGutterEdgePointerDown: () => {},
      onChipSelect: () => {},
      isDragging: false,
      precisionSnap: false,
      timedPreviewByDay: new Map(),
      previewInfo: null,
      previewColor: '#2a5c8a',
      previewTitle: '',
      allDayPreview: null,
      isLoading: false,
      eventsEmpty: true,
      error: null,
      onRetry: () => {},
    }),
  );

  const scroller = scrollerRef.current;
  assert.ok(scroller, 'the scroller ref must be attached');
  return { scroller: scroller!, days };
}

test('CalendarWeekGrid: the scroller content is never wider than the scroller', () => {
  const { scroller } = mountGrid();

  assert.ok(
    scroller.className.includes('overflow-x-hidden'),
    'the scroller must never grow a horizontal pan axis',
  );
  assert.ok(scroller.className.includes('overflow-y-auto'));

  // One content wrapper, exactly the scroller's width by default: no
  // `contentWidth`-wide child, so there is zero x scroll range.
  assert.equal(
    scroller.children.length,
    1,
    'the scroller must hold a single content wrapper',
  );
  const content = scroller.firstElementChild as HTMLElement;
  assert.equal(
    content.style.width,
    '',
    'the content wrapper must not carry a fixed width',
  );
  assert.ok(content.className.includes('min-h-full'));
});

test('CalendarWeekGrid: all three day tracks are clipped and share one offset', () => {
  const { scroller, days } = mountGrid();

  const tracks = [...scroller.querySelectorAll<HTMLElement>('div')].filter(
    (el) => el.style.transform === STRIP_TRACK_TRANSFORM,
  );
  assert.equal(
    tracks.length,
    3,
    'day headers, all-day band and hours track all translate by the strip',
  );
  assert.equal(
    new Set(tracks.map((track) => track.style.transform)).size,
    1,
    'the bands read the same CSS variable, so they move together',
  );

  for (const track of tracks) {
    const viewport = track.parentElement;
    assert.ok(viewport, 'each track sits in its own viewport');
    assert.ok(
      viewport!.className.includes('overflow-hidden'),
      'the track viewport must clip the wider track',
    );
    assert.ok(
      viewport!.className.includes('flex-1'),
      'the viewport is the scroller width minus the gutter',
    );
    assert.equal(track.style.width, `${days.length * COL_W}px`);
  }

  // The translated hours track still holds every column and the drag overlay,
  // so ghosts and hit-testing stay aligned with their day.
  const hoursTrack = tracks[tracks.length - 1];
  assert.equal(
    hoursTrack.querySelectorAll('[data-day-col]').length,
    days.length,
  );
  assert.equal(
    scroller.querySelectorAll('[data-allday-day]').length,
    days.length,
  );
});

// Unit tests for calendar page pure helpers (chip color, click-create,
// day labels, all-day / timed packing). No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import type { PositionedEvent } from '../components/EventChip';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  dayNameShort,
  eventChipColor,
  keyboardCreateRange,
} from './calendar-model';
import { colorForCalendar } from './week-layout';

function makeEvent(
  overrides: Partial<CalendarEvent> &
    Pick<CalendarEvent, 'id' | 'start_time' | 'end_time'>,
): CalendarEvent {
  return {
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Event',
    description: '',
    last_synced_at: '2026-09-08T00:00:00.000Z',
    ...overrides,
  };
}

// ── eventChipColor ──────────────────────────────────────────────────────

test('eventChipColor: uses event.color when non-blank', () => {
  const event = makeEvent({
    id: 'e1',
    start_time: '2026-09-08T09:00:00.000Z',
    end_time: '2026-09-08T10:00:00.000Z',
    color: '#2a5c8a',
  });
  assert.equal(eventChipColor(event), '#2a5c8a');
});

test('eventChipColor: falls back to colorForCalendar when color missing', () => {
  const event = makeEvent({
    id: 'e1',
    calendar_id: 'cal-abc',
    start_time: '2026-09-08T09:00:00.000Z',
    end_time: '2026-09-08T10:00:00.000Z',
  });
  assert.equal(eventChipColor(event), colorForCalendar('cal-abc'));
});

test('eventChipColor: falls back when color is blank/whitespace', () => {
  const event = makeEvent({
    id: 'e1',
    calendar_id: 'cal-xyz',
    start_time: '2026-09-08T09:00:00.000Z',
    end_time: '2026-09-08T10:00:00.000Z',
    color: '   ',
  });
  assert.equal(eventChipColor(event), colorForCalendar('cal-xyz'));
});

// ── dayNameShort ────────────────────────────────────────────────────────

test('dayNameShort: known Monday → Mon (WEEK_DAYS is Mon-first)', () => {
  // 2026-09-07 is a Monday.
  assert.equal(dayNameShort(new Date(2026, 8, 7)), 'Mon');
});

// ── clickCreateTimesFromSlot ────────────────────────────────────────────

test('clickCreateTimesFromSlot: 9:00 slot → 30 min range on that day', () => {
  const day = new Date(2026, 8, 8); // Tue Sep 8 local midnight
  const range = clickCreateTimesFromSlot({ day, minutes: 9 * 60 });
  assert.equal(range.start.getFullYear(), 2026);
  assert.equal(range.start.getMonth(), 8);
  assert.equal(range.start.getDate(), 8);
  assert.equal(range.start.getHours(), 9);
  assert.equal(range.start.getMinutes(), 0);
  assert.equal(range.end.getHours(), 9);
  assert.equal(range.end.getMinutes(), 30);
  assert.equal(range.end.getTime() - range.start.getTime(), 30 * 60 * 1000);
});

// ── keyboardCreateRange ────────────────────────────────────────────────

test('keyboardCreateRange: today visible → next half-hour on today, not visibleStart', () => {
  const visibleStart = new Date(2026, 8, 7); // Mon Sep 7
  const now = new Date(2026, 8, 8, 10, 7, 0); // Tue Sep 8 10:07
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 3 });
  assert.equal(range.start.getDate(), 8);
  assert.equal(range.start.getHours(), 10);
  assert.equal(range.start.getMinutes(), 30);
  assert.equal(range.end.getHours(), 11);
  assert.equal(range.end.getMinutes(), 0);
  assert.equal(range.end.getTime() - range.start.getTime(), 30 * 60_000);
});

test('keyboardCreateRange: an exact 10:30:00 boundary stays at 10:30', () => {
  const visibleStart = new Date(2026, 8, 8);
  const now = new Date(2026, 8, 8, 10, 30, 0);
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 1 });
  assert.equal(range.start.getHours(), 10);
  assert.equal(range.start.getMinutes(), 30);
  assert.equal(range.end.getHours(), 11);
  assert.equal(range.end.getMinutes(), 0);
});

test('keyboardCreateRange: a second past the boundary rolls to the next half-hour', () => {
  const visibleStart = new Date(2026, 8, 8);
  const now = new Date(2026, 8, 8, 10, 30, 1);
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 1 });
  assert.equal(range.start.getHours(), 11);
  assert.equal(range.start.getMinutes(), 0);
  assert.equal(range.end.getHours(), 11);
  assert.equal(range.end.getMinutes(), 30);
});

test('keyboardCreateRange: a 23:50 ceil is pulled back to 23:30–24:00', () => {
  const visibleStart = new Date(2026, 8, 8);
  const now = new Date(2026, 8, 8, 23, 50, 0);
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 1 });
  assert.equal(range.start.getDate(), 8);
  assert.equal(range.start.getHours(), 23);
  assert.equal(range.start.getMinutes(), 30);
  // End is midnight of the next civil day, not 24:00 on this one.
  assert.equal(range.end.getDate(), 9);
  assert.equal(range.end.getHours(), 0);
  assert.equal(range.end.getMinutes(), 0);
  assert.equal(range.end.getTime() - range.start.getTime(), 30 * 60_000);
});

test('keyboardCreateRange: today off-screen → 09:00 on visibleStart', () => {
  const visibleStart = new Date(2026, 8, 1);
  const now = new Date(2026, 8, 8, 10, 7, 0);
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 3 });
  assert.equal(range.start.getDate(), 1);
  assert.equal(range.start.getMonth(), 8);
  assert.equal(range.start.getHours(), 9);
  assert.equal(range.start.getMinutes(), 0);
  assert.equal(range.end.getHours(), 9);
  assert.equal(range.end.getMinutes(), 30);
});

test('keyboardCreateRange: a one-day period uses the clock when that day is today', () => {
  const visibleStart = new Date(2026, 8, 8);
  const now = new Date(2026, 8, 8, 14, 7, 0);
  const range = keyboardCreateRange({ now, visibleStart, periodLength: 1 });
  assert.equal(range.start.getHours(), 14);
  assert.equal(range.start.getMinutes(), 30);
  assert.equal(range.end.getHours(), 15);
  assert.equal(range.end.getMinutes(), 0);
});

test('keyboardCreateRange: invalid inputs fall back to a finite 09:00 range', () => {
  const epoch = new Date(0);
  const doubledInvalid = keyboardCreateRange({
    now: new Date(Number.NaN),
    visibleStart: new Date(Number.NaN),
    periodLength: 7,
  });
  assert.equal(doubledInvalid.start.toDateString(), epoch.toDateString());
  assert.equal(doubledInvalid.start.getHours(), 9);
  assert.equal(doubledInvalid.start.getMinutes(), 0);
  assert.equal(doubledInvalid.end.getHours(), 9);
  assert.equal(doubledInvalid.end.getMinutes(), 30);

  const visibleStart = new Date(2026, 8, 8);
  const onlyNowInvalid = keyboardCreateRange({
    now: new Date(Number.NaN),
    visibleStart,
    periodLength: 7,
  });
  assert.equal(onlyNowInvalid.start.getDate(), 8);
  assert.equal(onlyNowInvalid.start.getHours(), 9);
  assert.equal(onlyNowInvalid.start.getMinutes(), 0);
  assert.equal(onlyNowInvalid.end.getMinutes(), 30);
});

// ── buildEventsByDay ────────────────────────────────────────────────────

test('buildEventsByDay: 10:00–11:00 event under day key with height > 0', () => {
  const day = new Date(2026, 8, 8); // Tue Sep 8 local
  const start = new Date(2026, 8, 8, 10, 0, 0, 0);
  const end = new Date(2026, 8, 8, 11, 0, 0, 0);
  const event = makeEvent({
    id: 'timed-1',
    title: 'Standup',
    start_time: start.toISOString(),
    end_time: end.toISOString(),
  });
  const hourH = 48;
  const map = buildEventsByDay([day], [event], hourH);
  const key = day.toDateString();
  assert.ok(map.has(key));
  const list = map.get(key)!;
  assert.equal(list.length, 1);
  assert.equal(list[0].event.id, 'timed-1');
  assert.equal(list[0].startMin, 10 * 60);
  assert.equal(list[0].endMin, 11 * 60);
  assert.ok(list[0].height > 0);
});

/** Packing fields only: `top` / `height` are allowed to depend on the zoom. */
function packGeometry(list: PositionedEvent[]) {
  return list.map((p) => ({
    id: p.event.id,
    leftPercent: p.leftPercent,
    widthPercent: p.widthPercent,
    layerIndex: p.layerIndex,
    leftPixels: p.leftPixels,
  }));
}

// Oct 18: a 5-min chip paints well past its own end (CHIP_MIN_H is 67.5 min at
// hourH 16 and 5.2 min at hourH 208), and a 10-min event sits 2 min clear of its
// neighbour — the zoom-sensitive case the old widen got wrong.
test('buildEventsByDay: pack geometry is identical at hourH 16, 48, 208', () => {
  const day = new Date(2026, 9, 18); // Sun Oct 18 local
  const at = (h: number, m: number) =>
    new Date(2026, 9, 18, h, m, 0, 0).toISOString();
  const events = [
    makeEvent({
      id: 'tafseer',
      title: 'Tafseer Planning',
      start_time: at(16, 37),
      end_time: at(16, 42),
    }),
    makeEvent({
      id: 'salat',
      title: 'Salat & Istighfar',
      start_time: at(16, 42),
      end_time: at(16, 56),
    }),
    // True overlap → some widthPercent is not 1.
    makeEvent({
      id: 'long',
      title: 'Deep work',
      start_time: at(9, 0),
      end_time: at(11, 0),
    }),
    makeEvent({
      id: 'overlap',
      title: 'Standup',
      start_time: at(10, 0),
      end_time: at(12, 0),
    }),
    // 10 min, 2 min gap: true durations never overlap.
    makeEvent({
      id: 'short',
      title: 'Short',
      start_time: at(6, 0),
      end_time: at(6, 10),
    }),
    makeEvent({
      id: 'neighbour',
      title: 'Neighbour',
      start_time: at(6, 12),
      end_time: at(6, 22),
    }),
  ];

  const key = day.toDateString();
  const at16 = buildEventsByDay([day], events, 16).get(key)!;
  const at48 = buildEventsByDay([day], events, 48).get(key)!;
  const at208 = buildEventsByDay([day], events, 208).get(key)!;

  assert.deepEqual(packGeometry(at48), packGeometry(at16));
  assert.deepEqual(packGeometry(at208), packGeometry(at16));

  // Chronological paint order at every zoom.
  const chronological = [
    'short',
    'neighbour',
    'long',
    'overlap',
    'tafseer',
    'salat',
  ];
  assert.deepEqual(
    at16.map((p) => p.event.id),
    chronological,
  );
  assert.deepEqual(
    at48.map((p) => p.event.id),
    chronological,
  );

  // Guard: the overlap must really split the column, or the deepEqual above
  // would also pass on all-1 geometry.
  assert.ok(packGeometry(at16).some((p) => p.widthPercent < 1));

  // Zoom still moves pixels — only packing is invariant. 60 min must not be a
  // min-height chip at either zoom.
  const tall = at48.find((p) => p.event.id === 'long')!;
  const taller = at208.find((p) => p.event.id === 'long')!;
  assert.notEqual(tall.height, taller.height);
});

test('buildEventsByDay: a later-starting chip is emitted after its neighbour', () => {
  const day = new Date(2026, 9, 18); // Sun Oct 18 local
  const at = (h: number, m: number) =>
    new Date(2026, 9, 18, h, m, 0, 0).toISOString();
  const tafseer = makeEvent({
    id: 'tafseer',
    title: 'Tafseer Planning',
    start_time: at(16, 37),
    end_time: at(16, 42),
  });
  const salat = makeEvent({
    id: 'salat',
    title: 'Salat & Istighfar',
    start_time: at(16, 42),
    end_time: at(16, 56),
  });

  // Reverse input order: an overlay append or an untied server ORDER BY can
  // hand the pair over in either order. The output must still be chronological
  // so Salat paints over Tafseer's min-height tail in DayColumn.
  const list = buildEventsByDay([day], [salat, tafseer], 48).get(
    day.toDateString(),
  )!;
  assert.deepEqual(
    list.map((p) => p.event.id),
    ['tafseer', 'salat'],
  );
  for (const p of list) {
    assert.equal(p.leftPercent, 0);
    assert.equal(p.widthPercent, 1);
    assert.equal(p.layerIndex, 1);
  }
});

// ── buildAllDayChips ────────────────────────────────────────────────────

test('buildAllDayChips: midnight→next-midnight one civil day → one chip', () => {
  // All-day style: Tue Sep 8 00:00 → Wed Sep 9 00:00 local (timed multi-day path).
  const day = new Date(2026, 8, 8);
  const start = new Date(2026, 8, 8, 0, 0, 0, 0);
  const end = new Date(2026, 8, 9, 0, 0, 0, 0);
  const event = makeEvent({
    id: 'allday-1',
    title: 'Holiday',
    start_time: start.toISOString(),
    end_time: end.toISOString(),
  });
  const { allDayChips, allDayHeight } = buildAllDayChips([day], [event], 1);
  assert.equal(allDayChips.length, 1);
  assert.equal(allDayChips[0].id, 'allday-1');
  assert.equal(allDayChips[0].title, 'Holiday');
  assert.equal(allDayChips[0].startDay, 0);
  assert.equal(allDayChips[0].endDay, 0);
  assert.equal(allDayChips[0].lane, 0);
  assert.ok(allDayHeight > 0);
});

test('buildAllDayChips: is_all_day uses ISO prefix (UTC midnight stays civil day)', () => {
  // Replica stores all-day as YYYY-MM-DDT00:00:00Z. In UTC-4 that would be
  // the previous evening via new Date(iso) — chips must still occupy the 13th.
  const day = new Date(2026, 8, 13); // local Sep 13
  const event = makeEvent({
    id: 'allday-civil',
    title: 'Holiday',
    is_all_day: true,
    start_time: '2026-09-13T00:00:00Z',
    end_time: '2026-09-14T00:00:00Z',
  });
  const { allDayChips } = buildAllDayChips([day], [event], 1);
  assert.equal(allDayChips.length, 1);
  assert.equal(allDayChips[0].startDay, 0);
  assert.equal(allDayChips[0].endDay, 0);
});

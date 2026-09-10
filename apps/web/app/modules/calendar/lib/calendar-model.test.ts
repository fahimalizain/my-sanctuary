// Unit tests for calendar page pure helpers (chip color, click-create,
// day labels, all-day / timed packing). No React / DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  dayNameShort,
  eventChipColor,
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

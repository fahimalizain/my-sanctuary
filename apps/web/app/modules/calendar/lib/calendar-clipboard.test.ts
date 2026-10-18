// Unit tests for the in-memory calendar clipboard: the payload a copy/cut
// keeps, where a paste lands, and the create body it POSTs. Pure — no DOM.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent, GoogleCalendar } from '@/app/types';
import {
  clipboardItemFromEvent,
  pasteCreateInput,
  pasteDestinationCalendarId,
  type CalendarClipboardItem,
} from './calendar-clipboard';

function makeEvent(
  overrides: Partial<CalendarEvent> &
    Pick<CalendarEvent, 'id' | 'start_time' | 'end_time'>,
): CalendarEvent {
  return {
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Event',
    description: '',
    last_synced_at: '2026-10-01T00:00:00.000Z',
    ...overrides,
  };
}

/** Writer on `cal-1` by default, so most cases only override what they test. */
function makeCalendar(
  overrides: Partial<GoogleCalendar> & Pick<GoogleCalendar, 'id'>,
): GoogleCalendar {
  return {
    google_calendar_id: `google-${overrides.id}`,
    summary: overrides.id,
    time_zone: 'UTC',
    is_primary: false,
    access_role: 'writer',
    sync_enabled: true,
    ...overrides,
  };
}

function makeItem(
  overrides: Partial<CalendarClipboardItem> = {},
): CalendarClipboardItem {
  return {
    summary: 'Standup',
    description: '',
    start: '2026-10-05T09:00:00.000Z',
    end: '2026-10-05T09:30:00.000Z',
    isAllDay: false,
    timeZone: '',
    calendarId: 'cal-1',
    color: '',
    cutSourceId: null,
    ...overrides,
  };
}

// ── clipboardItemFromEvent ──────────────────────────────────────────────

test('clipboardItemFromEvent: null event and blank titles are not copyable', () => {
  assert.equal(clipboardItemFromEvent(null), null);
  assert.equal(
    clipboardItemFromEvent(
      makeEvent({
        id: 'e-empty',
        title: '',
        start_time: '2026-10-05T09:00:00.000Z',
        end_time: '2026-10-05T09:30:00.000Z',
      }),
    ),
    null,
  );
  assert.equal(
    clipboardItemFromEvent(
      makeEvent({
        id: 'e-blank',
        title: '   ',
        start_time: '2026-10-05T09:00:00.000Z',
        end_time: '2026-10-05T09:30:00.000Z',
      }),
    ),
    null,
  );
});

test('clipboardItemFromEvent: trims text and keeps the source times', () => {
  const item = clipboardItemFromEvent(
    makeEvent({
      id: 'e1',
      title: '  Standup  ',
      description: '  daily notes  ',
      start_time: '2026-10-05T09:00:00.000Z',
      end_time: '2026-10-05T09:30:00.000Z',
    }),
  );

  assert.deepEqual(item, {
    summary: 'Standup',
    description: 'daily notes',
    start: '2026-10-05T09:00:00.000Z',
    end: '2026-10-05T09:30:00.000Z',
    isAllDay: false,
    timeZone: '',
    calendarId: 'cal-1',
    color: '',
    cutSourceId: null,
  });
});

test('clipboardItemFromEvent: an all-day event keeps its ISO strings and color', () => {
  const item = clipboardItemFromEvent(
    makeEvent({
      id: 'e2',
      title: 'Offsite',
      is_all_day: true,
      start_time: '2026-10-05T00:00:00Z',
      end_time: '2026-10-06T00:00:00Z',
      color: '#336699',
    }),
  );

  assert.ok(item);
  assert.equal(item.isAllDay, true);
  assert.equal(item.start, '2026-10-05T00:00:00Z');
  assert.equal(item.end, '2026-10-06T00:00:00Z');
  assert.equal(item.color, '#336699');
  assert.equal(item.timeZone, '');
});

test('clipboardItemFromEvent: zone prefers start_time_zone, else end_time_zone', () => {
  const start = clipboardItemFromEvent(
    makeEvent({
      id: 'e3',
      start_time: '2026-10-05T09:00:00.000Z',
      end_time: '2026-10-05T09:30:00.000Z',
      start_time_zone: ' Asia/Riyadh ',
      end_time_zone: 'Asia/Dubai',
    }),
  );
  assert.equal(start?.timeZone, 'Asia/Riyadh');

  const fallback = clipboardItemFromEvent(
    makeEvent({
      id: 'e4',
      start_time: '2026-10-05T09:00:00.000Z',
      end_time: '2026-10-05T09:30:00.000Z',
      start_time_zone: '   ',
      end_time_zone: ' Asia/Dubai ',
    }),
  );
  assert.equal(fallback?.timeZone, 'Asia/Dubai');

  const none = clipboardItemFromEvent(
    makeEvent({
      id: 'e5',
      start_time: '2026-10-05T09:00:00.000Z',
      end_time: '2026-10-05T09:30:00.000Z',
      start_time_zone: '  ',
      end_time_zone: '',
    }),
  );
  assert.equal(none?.timeZone, '');
});

test('clipboardItemFromEvent: a recurring source is flattened to one one-shot', () => {
  const item = clipboardItemFromEvent(
    makeEvent({
      id: 'e6',
      recurrence: '["RRULE:FREQ=DAILY"]',
      recurring_event_id: 'series-1',
      start_time: '2026-10-05T09:00:00.000Z',
      end_time: '2026-10-05T09:30:00.000Z',
    }),
  );

  assert.ok(item);
  // The item carries exactly these fields — no recurrence, no google ids.
  assert.deepEqual(Object.keys(item).sort(), [
    'calendarId',
    'color',
    'cutSourceId',
    'description',
    'end',
    'isAllDay',
    'start',
    'summary',
    'timeZone',
  ]);
  // …and the create body adds nothing beyond the allowed keys.
  assert.deepEqual(Object.keys(pasteCreateInput(item, 'cal-1')).sort(), [
    'calendar_id',
    'calendar_id_explicit',
    'end',
    'start',
    'summary',
  ]);
});

// ── pasteDestinationCalendarId ──────────────────────────────────────────

test('pasteDestinationCalendarId: the source calendar wins when it is writable', () => {
  const source = makeCalendar({ id: 'src', access_role: 'writer' });
  const primary = makeCalendar({
    id: 'p1',
    is_primary: true,
    access_role: 'owner',
  });

  assert.equal(pasteDestinationCalendarId('src', [source, primary]), 'src');
});

test('pasteDestinationCalendarId: a read-only source falls through to the primary writer', () => {
  const reader = makeCalendar({ id: 'src', access_role: 'reader' });
  const primary = makeCalendar({
    id: 'p1',
    is_primary: true,
    access_role: 'owner',
  });

  assert.equal(pasteDestinationCalendarId('src', [reader, primary]), 'p1');
});

test('pasteDestinationCalendarId: an unknown source id falls through', () => {
  const primary = makeCalendar({
    id: 'p1',
    is_primary: true,
    access_role: 'owner',
  });

  // isCalendarWritable(undefined) is true; a calendar missing from the list
  // must not count as writable.
  assert.equal(pasteDestinationCalendarId('gone', [primary]), 'p1');
});

test('pasteDestinationCalendarId: null when nothing is writable', () => {
  const reader = makeCalendar({
    id: 'r1',
    is_primary: true,
    access_role: 'reader',
  });

  assert.equal(pasteDestinationCalendarId('r1', [reader]), null);
  assert.equal(pasteDestinationCalendarId('gone', []), null);
});

// ── pasteCreateInput ────────────────────────────────────────────────────

test('pasteCreateInput: an all-day item sends is_all_day and never a zone', () => {
  const item = makeItem({
    summary: 'Offsite',
    start: '2026-10-05T00:00:00Z',
    end: '2026-10-06T00:00:00Z',
    isAllDay: true,
    timeZone: 'Asia/Riyadh',
    // Display-only: the insert path must never see it.
    color: '#336699',
  });

  const input = pasteCreateInput(item, 'dest');

  assert.deepEqual(input, {
    calendar_id: 'dest',
    calendar_id_explicit: true,
    summary: 'Offsite',
    start: '2026-10-05T00:00:00Z',
    end: '2026-10-06T00:00:00Z',
    is_all_day: true,
  });
  assert.deepEqual(Object.keys(input).sort(), [
    'calendar_id',
    'calendar_id_explicit',
    'end',
    'is_all_day',
    'start',
    'summary',
  ]);
});

test('pasteCreateInput: a timed item with a zone omits is_all_day', () => {
  const input = pasteCreateInput(
    makeItem({ timeZone: ' Asia/Dubai ' }),
    'dest',
  );

  assert.deepEqual(input, {
    calendar_id: 'dest',
    calendar_id_explicit: true,
    summary: 'Standup',
    start: '2026-10-05T09:00:00.000Z',
    end: '2026-10-05T09:30:00.000Z',
    start_time_zone: 'Asia/Dubai',
  });
  assert.equal('is_all_day' in input, false);
  assert.deepEqual(Object.keys(input).sort(), [
    'calendar_id',
    'calendar_id_explicit',
    'end',
    'start',
    'start_time_zone',
    'summary',
  ]);
});

test('pasteCreateInput: a timed item without a zone sends no zone key', () => {
  const input = pasteCreateInput(makeItem({ timeZone: '   ' }), 'dest');

  assert.equal('start_time_zone' in input, false);
  assert.equal('is_all_day' in input, false);
});

test('pasteCreateInput: an empty description is omitted', () => {
  const input = pasteCreateInput(makeItem({ description: '   ' }), 'dest');

  assert.equal('description' in input, false);
});

test('pasteCreateInput: a non-empty description is trimmed and sent', () => {
  const input = pasteCreateInput(
    makeItem({ description: '  daily notes  ' }),
    'dest',
  );

  assert.equal(input.description, 'daily notes');
});

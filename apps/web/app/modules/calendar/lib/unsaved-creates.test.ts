// Unit tests for the unsaved-create rules: a failed create stays a local
// event, and a retry POSTs its current state without losing the flags the
// failed body carried (all-day, zone, explicit calendar). No React, no network.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import {
  UnsavedCreates,
  canRetryUnsavedCreate,
  createRetryInput,
  retryableUnsavedIds,
} from './unsaved-creates';

function makeEvent(overrides: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id: 'tmp_1',
    calendar_id: 'cal-1',
    google_event_id: '',
    title: 'Dentist',
    description: '',
    start_time: '2026-10-18T09:00:00.000Z',
    end_time: '2026-10-18T10:00:00.000Z',
    last_synced_at: '2026-10-18T00:00:00.000Z',
    ...overrides,
  };
}

/** The body a draft's title blur posts, i.e. the failed baseline. */
const DRAFT_POST = {
  calendar_id: 'cal-1',
  summary: 'Dentist',
  start: '2026-10-18T09:00:00.000Z',
  end: '2026-10-18T10:00:00.000Z',
};

// ── Records ─────────────────────────────────────────────────────────────

test('records are keyed by temp id and keep failure order', () => {
  const records = new UnsavedCreates();
  records.mark({ tempId: 'tmp_1', input: DRAFT_POST });
  records.mark({ tempId: 'tmp_2', input: { ...DRAFT_POST, summary: 'Lunch' } });

  assert.deepEqual(records.ids(), ['tmp_1', 'tmp_2']);
  assert.equal(records.get('tmp_2')?.input.summary, 'Lunch');
  assert.equal(records.has('tmp_1'), true);

  assert.equal(records.clear('tmp_1'), true, 'the record was there');
  assert.equal(records.clear('tmp_1'), false, 'and is now gone');
  assert.deepEqual(records.ids(), ['tmp_2']);

  records.reset();
  assert.deepEqual(records.ids(), []);
});

// ── Retry body ──────────────────────────────────────────────────────────

test('a draft retry posts the local state, not the failed body', () => {
  const input = createRetryInput({
    failed: DRAFT_POST,
    local: makeEvent({
      title: 'Dentist 2',
      description: 'Bring the form',
      start_time: '2026-10-18T11:00:00.000Z',
      end_time: '2026-10-18T12:00:00.000Z',
    }),
    calendarPicked: false,
  });

  assert.equal(
    input?.calendar_id_explicit,
    undefined,
    'an untouched draft stays routable by category',
  );
  assert.deepEqual(input, {
    calendar_id: 'cal-1',
    summary: 'Dentist 2',
    description: 'Bring the form',
    start: '2026-10-18T11:00:00.000Z',
    end: '2026-10-18T12:00:00.000Z',
  });
});

test('a pick made while the create was unsaved becomes the explicit destination', () => {
  const input = createRetryInput({
    failed: DRAFT_POST,
    local: makeEvent({ calendar_id: 'cal-2' }),
    calendarPicked: true,
  });

  assert.equal(input?.calendar_id, 'cal-2');
  assert.equal(input?.calendar_id_explicit, true);
});

test('a paste retry keeps its explicit destination', () => {
  const input = createRetryInput({
    failed: { ...DRAFT_POST, calendar_id_explicit: true },
    local: makeEvent({ calendar_id: 'cal-2' }),
    calendarPicked: false,
  });

  assert.equal(input?.calendar_id, 'cal-2');
  assert.equal(
    input?.calendar_id_explicit,
    true,
    'the server must not reroute a paste by title',
  );
});

test('an all-day retry posts civil dates and no zone', () => {
  const input = createRetryInput({
    failed: { ...DRAFT_POST, is_all_day: true },
    local: makeEvent({
      is_all_day: true,
      start_time: '2026-10-19T00:00:00.000Z',
      end_time: '2026-10-20T00:00:00.000Z',
      start_time_zone: 'Asia/Tokyo',
      end_time_zone: 'Asia/Tokyo',
    }),
    calendarPicked: false,
  });

  assert.equal(input?.is_all_day, true);
  assert.equal(input?.start, '2026-10-19T00:00:00.000Z');
  assert.equal(input?.start_time_zone, undefined, 'all-day carries no zone');
});

test('a retry flipped back to timed clears the posted all-day flag', () => {
  const input = createRetryInput({
    failed: { ...DRAFT_POST, is_all_day: true },
    local: makeEvent({ is_all_day: false, start_time_zone: 'Europe/Paris' }),
    calendarPicked: false,
  });

  assert.equal(input?.is_all_day, false, 'the flip back is explicit');
  assert.equal(input?.start_time_zone, 'Europe/Paris');
});

test('a timed retry omits a zone the user cleared', () => {
  const input = createRetryInput({
    failed: { ...DRAFT_POST, start_time_zone: 'Europe/Paris' },
    local: makeEvent({ start_time_zone: '  ' }),
    calendarPicked: false,
  });

  assert.equal(input?.start_time_zone, undefined);
});

test('a blank title is not retryable', () => {
  const input = createRetryInput({
    failed: DRAFT_POST,
    local: makeEvent({ title: '   ' }),
    calendarPicked: false,
  });
  assert.equal(input, null);
});

test('an empty description is omitted from the retry body', () => {
  const input = createRetryInput({
    failed: DRAFT_POST,
    local: makeEvent({ description: '' }),
    calendarPicked: false,
  });
  assert.equal(input?.description, undefined);
});

// ── Retry gates ─────────────────────────────────────────────────────────

const noGate = { hiddenId: null, inFlight: new Set<string>() };

test('a hidden or in-flight create is not retried again', () => {
  assert.equal(canRetryUnsavedCreate('tmp_1', noGate), true);
  assert.equal(
    canRetryUnsavedCreate('tmp_1', { hiddenId: 'tmp_1', inFlight: new Set() }),
    false,
    'the undo toast owns it',
  );
  assert.equal(
    canRetryUnsavedCreate('tmp_1', {
      hiddenId: null,
      inFlight: new Set(['tmp_1']),
    }),
    false,
    'one retry POST per event at a time',
  );
  assert.equal(
    canRetryUnsavedCreate('tmp_1', { hiddenId: 'tmp_2', inFlight: new Set() }),
    true,
    'another event being hidden does not block this one',
  );
});

test('retryableUnsavedIds filters in failure order', () => {
  assert.deepEqual(
    retryableUnsavedIds(['tmp_1', 'tmp_2', 'tmp_3'], {
      hiddenId: 'tmp_2',
      inFlight: new Set(['tmp_3']),
    }),
    ['tmp_1'],
  );
  assert.deepEqual(retryableUnsavedIds([], noGate), []);
});

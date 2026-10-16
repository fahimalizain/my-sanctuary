// Unit tests for the per-event PATCH chain (ordering, coalescing, revert).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import {
  buildDraftFlushPatch,
  EventPatchQueue,
  mergeEventPatch,
  revertFailedPatch,
  toWirePatch,
  type EventPatch,
} from './event-patch-queue';

function makeEvent(overrides: Partial<CalendarEvent> = {}): CalendarEvent {
  return {
    id: 'e1',
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Old',
    description: '',
    start_time: '2026-09-13T05:00:00.000Z',
    end_time: '2026-09-13T06:00:00.000Z',
    last_synced_at: '2026-09-08T00:00:00.000Z',
    ...overrides,
  };
}

interface Call {
  id: string;
  input: EventPatch;
  resolve: (ev?: CalendarEvent) => void;
  reject: (err: unknown) => void;
}

function harness() {
  const calls: Call[] = [];
  const log: string[] = [];
  const queue = new EventPatchQueue({
    send: (id, input) =>
      new Promise<CalendarEvent>((resolve, reject) => {
        calls.push({
          id,
          input,
          resolve: (ev) => resolve(ev ?? makeEvent({ id })),
          reject,
        });
      }),
    onSuccess: (id, sent, _ev, idle) => {
      log.push(`ok:${id}:${Object.keys(sent).join(',')}:${idle}`);
    },
    onError: (id, sent, _err, idle) => {
      log.push(`err:${id}:${Object.keys(sent).join(',')}:${idle}`);
    },
  });
  return { queue, calls, log };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

test('mergeEventPatch: last write wins per field; all-day drops zone', () => {
  assert.deepEqual(
    mergeEventPatch({ summary: 'a', start: 's1' }, { summary: 'b' }),
    { summary: 'b', start: 's1' },
  );
  assert.deepEqual(
    mergeEventPatch({ start_time_zone: 'UTC' }, { is_all_day: true }),
    { is_all_day: true },
  );
});

test('toWirePatch: cleared zone is omitted', () => {
  assert.deepEqual(toWirePatch({ start: 's', start_time_zone: '' }), {
    start: 's',
  });
});

test('one request in flight per event; edits coalesce into the next', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e1', { summary: 'B' });
  queue.enqueue('e1', { description: 'D' });
  queue.enqueue('e1', { summary: 'C' });
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].input, { summary: 'A' });

  calls[0].resolve();
  await tick();
  // B and C merged; only the latest title goes out, with the description.
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].input, { summary: 'C', description: 'D' });
  calls[1].resolve();
  await tick();
  assert.deepEqual(log, [
    'ok:e1:summary:false',
    'ok:e1:summary,description:true',
  ]);
  assert.equal(queue.isBusy('e1'), false);
});

test('out-of-order: a later edit is sent only after the earlier one settles', async () => {
  const { queue, calls } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e1', { summary: 'B' });
  // B has not been sent, so it cannot respond before A.
  assert.equal(calls.length, 1);
  calls[0].resolve(makeEvent({ title: 'A' }));
  await tick();
  assert.deepEqual(calls[1].input, { summary: 'B' });
});

test('different events run in parallel', () => {
  const { queue, calls } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e2', { summary: 'B' });
  assert.equal(calls.length, 2);
});

test('drag-style times patch merges with a pending inspector edit', async () => {
  const { queue, calls } = harness();
  queue.enqueue('e1', { description: 'x' });
  queue.enqueue('e1', { start: 's2', end: 'e2' });
  calls[0].resolve();
  await tick();
  assert.deepEqual(calls[1].input, { start: 's2', end: 'e2' });
});

test('calendar move goes in its own request after the field request', async () => {
  const { queue, calls } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e1', { calendar_id: 'cal-2', description: 'D' });
  calls[0].resolve();
  await tick();
  assert.deepEqual(calls[1].input, { description: 'D' });
  calls[1].resolve();
  await tick();
  assert.deepEqual(calls[2].input, { calendar_id: 'cal-2' });
});

test('failure does not stop later queued edits; idle flag is accurate', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e1', { summary: 'B' });
  calls[0].reject(new Error('boom'));
  await tick();
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].input, { summary: 'B' });
  calls[1].resolve();
  await tick();
  assert.deepEqual(log, ['err:e1:summary:false', 'ok:e1:summary:true']);
});

test('revertFailedPatch: reverts failed fields not superseded by a newer edit', () => {
  const base = makeEvent();
  // Title still holds the failed value; description was edited afterwards.
  const overlay = makeEvent({ title: 'A', description: 'newer' });
  const reverted = revertFailedPatch(overlay, base, {
    summary: 'A',
    description: 'older',
  });
  assert.equal(reverted.title, 'Old');
  assert.equal(reverted.description, 'newer');
});

test('revertFailedPatch: failed move restores calendar and color', () => {
  const base = makeEvent({ color: '#111' });
  const overlay = makeEvent({ calendar_id: 'cal-2', color: '#222' });
  const reverted = revertFailedPatch(overlay, base, { calendar_id: 'cal-2' });
  assert.equal(reverted.calendar_id, 'cal-1');
  assert.equal(reverted.color, '#111');
});

test('revertFailedPatch: superseded failure leaves the overlay alone', () => {
  const base = makeEvent();
  const overlay = makeEvent({ title: 'B' });
  const reverted = revertFailedPatch(overlay, base, { summary: 'A' });
  assert.equal(reverted.title, 'B');
});

test('buildDraftFlushPatch: null when nothing changed since create', () => {
  const local = makeEvent({ title: 'T', description: '' });
  assert.equal(
    buildDraftFlushPatch({
      posted: {
        summary: 'T',
        description: '',
        start: local.start_time,
        end: local.end_time,
      },
      local,
      serverCalendarId: 'cal-1',
    }),
    null,
  );
});

test('buildDraftFlushPatch: collects title, times, zone and move', () => {
  const local = makeEvent({
    title: 'Renamed',
    end_time: '2026-09-13T07:00:00.000Z',
    start_time_zone: 'UTC',
    calendar_id: 'cal-2',
  });
  const patch = buildDraftFlushPatch({
    posted: {
      summary: 'T',
      description: '',
      start: local.start_time,
      end: '2026-09-13T06:00:00.000Z',
    },
    local,
    serverCalendarId: 'cal-1',
  });
  assert.deepEqual(patch, {
    summary: 'Renamed',
    start: local.start_time,
    end: '2026-09-13T07:00:00.000Z',
    start_time_zone: 'UTC',
    calendar_id: 'cal-2',
  });
});

test('temp flush then a later user edit stay ordered on the server id', async () => {
  const { queue, calls } = harness();
  const local = makeEvent({
    id: 'srv-1',
    title: 'Renamed',
    calendar_id: 'cal-2',
  });
  const flush = buildDraftFlushPatch({
    posted: {
      summary: 'T',
      description: '',
      start: local.start_time,
      end: local.end_time,
    },
    local,
    serverCalendarId: 'cal-1',
  });
  assert.ok(flush);
  queue.enqueue('srv-1', flush);
  // User keeps editing right after the remap.
  queue.enqueue('srv-1', { summary: 'Final' });
  assert.deepEqual(calls[0].input, { summary: 'Renamed' });
  calls[0].resolve();
  await tick();
  // Field edit (merged) precedes the exclusive calendar move.
  assert.deepEqual(calls[1].input, { summary: 'Final' });
  calls[1].resolve();
  await tick();
  assert.deepEqual(calls[2].input, { calendar_id: 'cal-2' });
});

test('cancel with a request in flight ignores its success', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.cancel('e1');
  calls[0].resolve();
  await tick();
  assert.deepEqual(log, []);
  assert.equal(queue.isBusy('e1'), false);
});

test('cancel with a request in flight ignores its failure', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.cancel('e1');
  calls[0].reject(new Error('404'));
  await tick();
  assert.deepEqual(log, []);
});

test('cancel drops pending edits', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.enqueue('e1', { summary: 'B' });
  queue.cancel('e1');
  calls[0].resolve();
  await tick();
  assert.equal(calls.length, 1);
  assert.deepEqual(log, []);
});

test('enqueue after cancel works normally (in flight and idle)', async () => {
  const { queue, calls, log } = harness();
  queue.enqueue('e1', { summary: 'A' });
  queue.cancel('e1');
  queue.enqueue('e1', { summary: 'B' });
  calls[0].resolve();
  await tick();
  // A's result is ignored; B goes out next and reports normally.
  assert.deepEqual(calls[1].input, { summary: 'B' });
  calls[1].resolve();
  await tick();
  assert.deepEqual(log, ['ok:e1:summary:true']);

  queue.cancel('e1');
  queue.enqueue('e1', { summary: 'C' });
  calls[2].resolve();
  await tick();
  assert.deepEqual(log, ['ok:e1:summary:true', 'ok:e1:summary:true']);
});

// Unit tests for the settled-patch policy (cache write, overlay clear/revert).
// No DOM and no API client: every collaborator is a plain callback.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import type { EventOverlay } from './event-overlays';
import { reconcilePatchError, reconcilePatchSuccess } from './event-patch-sync';
import type { EventPatch } from './event-patch-queue';

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

function upsertOverlay(event: CalendarEvent): EventOverlay {
  return { op: 'upsert', event };
}

const deleteOverlay: EventOverlay = { op: 'delete', id: 'e1' };

/** Records every callback the helper is allowed to make. */
function recorder() {
  const cleared: string[] = [];
  const upserted: CalendarEvent[] = [];
  const cached: CalendarEvent[] = [];
  return { cleared, upserted, cached };
}

test('success: idle overlay matching the server row is cleared', () => {
  const rec = recorder();
  const server = makeEvent({ title: 'Renamed' });

  reconcilePatchSuccess({
    id: 'e1',
    event: server,
    idle: true,
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Renamed' })),
    clear: (id) => rec.cleared.push(id),
    upsertCache: (event) => rec.cached.push(event),
  });

  assert.deepEqual(rec.cached, [server], 'authoritative row is cached');
  assert.deepEqual(rec.cleared, ['e1']);
});

test('success: not idle keeps the overlay for the queued edits', () => {
  const rec = recorder();
  const server = makeEvent({ title: 'Renamed' });

  reconcilePatchSuccess({
    id: 'e1',
    event: server,
    idle: false,
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Renamed' })),
    clear: (id) => rec.cleared.push(id),
    upsertCache: (event) => rec.cached.push(event),
  });

  assert.deepEqual(rec.cached, [server]);
  assert.deepEqual(rec.cleared, [], 'a later patch still owns this id');
});

test('success: idle overlay the server row does not match is left alone', () => {
  const rec = recorder();

  reconcilePatchSuccess({
    id: 'e1',
    event: makeEvent({ title: 'Server title' }),
    idle: true,
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Newer local title' })),
    clear: (id) => rec.cleared.push(id),
    upsertCache: (event) => rec.cached.push(event),
  });

  assert.equal(rec.cached.length, 1);
  assert.deepEqual(rec.cleared, []);
});

test('success: a delete overlay is never cleared by a field response', () => {
  const rec = recorder();

  reconcilePatchSuccess({
    id: 'e1',
    event: makeEvent(),
    idle: true,
    getOverlay: () => deleteOverlay,
    clear: (id) => rec.cleared.push(id),
    upsertCache: (event) => rec.cached.push(event),
  });

  assert.equal(rec.cached.length, 1);
  assert.deepEqual(rec.cleared, []);
});

test('success: no overlay still clears the stale id', () => {
  const rec = recorder();

  reconcilePatchSuccess({
    id: 'e1',
    event: makeEvent(),
    idle: true,
    getOverlay: () => undefined,
    clear: (id) => rec.cleared.push(id),
    upsertCache: (event) => rec.cached.push(event),
  });

  assert.deepEqual(rec.cleared, ['e1']);
});

test('error: upsert with no server row clears only when idle', () => {
  const sent: EventPatch = { summary: 'Renamed' };

  for (const idle of [true, false]) {
    const rec = recorder();
    reconcilePatchError({
      id: 'e1',
      sent,
      idle,
      getOverlay: () => upsertOverlay(makeEvent({ title: 'Renamed' })),
      clear: (id) => rec.cleared.push(id),
      upsert: (event) => rec.upserted.push(event),
      findServerEvent: () => undefined,
    });
    assert.deepEqual(rec.cleared, idle ? ['e1'] : []);
    assert.deepEqual(rec.upserted, []);
  }
});

test('error: a reverted patch matching the server row is cleared', () => {
  const rec = recorder();
  const base = makeEvent({ title: 'Old' });

  reconcilePatchError({
    id: 'e1',
    sent: { summary: 'Renamed' },
    idle: true,
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Renamed' })),
    clear: (id) => rec.cleared.push(id),
    upsert: (event) => rec.upserted.push(event),
    findServerEvent: (id) => (id === 'e1' ? base : undefined),
  });

  assert.deepEqual(rec.cleared, ['e1']);
  assert.deepEqual(rec.upserted, []);
});

test('error: superseded field leaves the overlay alone', () => {
  const rec = recorder();
  const base = makeEvent({ title: 'Old' });

  reconcilePatchError({
    id: 'e1',
    sent: { summary: 'Failed title' },
    idle: true,
    // Title was edited again while the failed request was in flight.
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Newest title' })),
    clear: (id) => rec.cleared.push(id),
    upsert: (event) => rec.upserted.push(event),
    findServerEvent: () => base,
  });

  assert.deepEqual(rec.cleared, []);
  assert.deepEqual(rec.upserted, [], 'revert is a no-op for that field');
});

test('error: a partial revert repaints the overlay with the reverted row', () => {
  const rec = recorder();
  const base = makeEvent({ title: 'Old', description: '' });

  reconcilePatchError({
    id: 'e1',
    // The description edit came later, so only the title reverts.
    sent: { summary: 'Renamed', description: 'failed note' },
    idle: true,
    getOverlay: () =>
      upsertOverlay(
        makeEvent({ title: 'Renamed', description: 'a newer note' }),
      ),
    clear: (id) => rec.cleared.push(id),
    upsert: (event) => rec.upserted.push(event),
    findServerEvent: () => base,
  });

  assert.deepEqual(rec.cleared, [], 'reverted row still differs from base');
  assert.equal(rec.upserted.length, 1);
  assert.equal(rec.upserted[0]?.title, 'Old');
  assert.equal(rec.upserted[0]?.description, 'a newer note');
});

test('error: not idle keeps a reverted row instead of clearing it', () => {
  const rec = recorder();
  const base = makeEvent({ title: 'Old' });

  reconcilePatchError({
    id: 'e1',
    sent: { summary: 'Renamed' },
    idle: false,
    getOverlay: () => upsertOverlay(makeEvent({ title: 'Renamed' })),
    clear: (id) => rec.cleared.push(id),
    upsert: (event) => rec.upserted.push(event),
    findServerEvent: () => base,
  });

  assert.deepEqual(rec.cleared, [], 'nothing is idle, so keep painting');
  assert.equal(rec.upserted.length, 1);
  assert.equal(rec.upserted[0]?.title, 'Old');
});

test('error: a delete overlay is never reverted', () => {
  const rec = recorder();
  let lookups = 0;

  reconcilePatchError({
    id: 'e1',
    sent: { summary: 'Renamed' },
    idle: true,
    getOverlay: () => deleteOverlay,
    clear: (id) => rec.cleared.push(id),
    upsert: (event) => rec.upserted.push(event),
    findServerEvent: () => {
      lookups += 1;
      return makeEvent();
    },
  });

  assert.deepEqual(rec.cleared, []);
  assert.deepEqual(rec.upserted, []);
  assert.equal(lookups, 0, 'the delete owns the id; no base lookup needed');
});

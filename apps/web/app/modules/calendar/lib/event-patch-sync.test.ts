// Unit tests for the settled-patch policy: success acknowledges (and keeps)
// intent, failure keeps it retryable, and only a later server read retires a
// field.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import { EventChangeLayer } from './event-change-layer';
import { settlePatchFailure, settlePatchSuccess } from './event-patch-sync';

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

function serverLookup(
  ...rows: CalendarEvent[]
): (id: string) => CalendarEvent | undefined {
  const byId = new Map(rows.map((row) => [row.id, row]));
  return (id) => byId.get(id);
}

function recorder() {
  const cached: CalendarEvent[] = [];
  return {
    cached,
    upsertCache: (event: CalendarEvent) => cached.push(event),
  };
}

test('success: acknowledges the sent values, caches the row, keeps the paint', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New' });
  const rec = recorder();
  const server = makeEvent({ title: 'New' });

  settlePatchSuccess({
    layer,
    id: 'e1',
    sent: { summary: 'New' },
    event: server,
    stamp: 2,
    upsertCache: rec.upsertCache,
  });

  assert.deepEqual(rec.cached, [server], 'the authoritative row is cached');
  assert.equal(
    layer.getOverlay('e1')?.op,
    'upsert',
    'a write response is not a server read: it never retires intent',
  );
  assert.equal(layer.unsentPatch('e1'), null, 'the value is acknowledged');

  // Only a window read that started after the ack settles the field.
  const agreed = makeEvent({ title: 'New' });
  assert.equal(layer.paint([agreed], 3)[0].title, 'New');
  assert.equal(layer.size, 0);
});

test('success: an ack of an older value does not cover a newer edit', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'A' });
  layer.edit('e1', { summary: 'B' });
  const rec = recorder();

  // The request that landed carried A; B was typed while it flew.
  settlePatchSuccess({
    layer,
    id: 'e1',
    sent: { summary: 'A' },
    event: makeEvent({ title: 'A' }),
    stamp: 2,
    upsertCache: rec.upsertCache,
  });

  assert.deepEqual(
    layer.unsentPatch('e1'),
    { summary: 'B' },
    'B still needs the server',
  );
});

test('failure: the intent stays painted and retryable, cache untouched', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New' });
  const rec = recorder();

  settlePatchFailure({ layer, id: 'e1' });

  assert.deepEqual(rec.cached, [], 'a failure has no row to cache');
  assert.equal(layer.paint([base])[0].title, 'New');
  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'New' });
  assert.equal(layer.hasFailedWrites(), true);
});

test('failure: a read cannot retire a value the server never accepted', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New' });
  settlePatchFailure({ layer, id: 'e1' });

  const remote = makeEvent({ title: 'Old' });
  assert.equal(layer.paint([remote], 9)[0].title, 'New');
  assert.equal(layer.hasFailedWrites(), true);
});

test('success/failure leave a delete intent alone', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.remove('e1');
  const rec = recorder();

  settlePatchSuccess({
    layer,
    id: 'e1',
    sent: { summary: 'New' },
    event: makeEvent({ title: 'New' }),
    stamp: 2,
    upsertCache: rec.upsertCache,
  });
  settlePatchFailure({ layer, id: 'e1' });

  assert.deepEqual(layer.paint([makeEvent()]), [], 'the delete still owns it');
  assert.deepEqual(layer.getOverlay('e1'), { op: 'delete', id: 'e1' });
  assert.equal(rec.cached.length, 1, 'the row is cached under the delete');
});

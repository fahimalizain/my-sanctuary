// Unit tests for the per-event local change layer: display intent, the
// no-op-skip send rules, and the ack / server-read retirement policy.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import { applyEventIntent, EventChangeLayer } from './event-change-layer';

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

/** The layer's cache lookup over a fixed set of server rows. */
function serverLookup(
  ...rows: CalendarEvent[]
): (id: string) => CalendarEvent | undefined {
  const byId = new Map(rows.map((row) => [row.id, row]));
  return (id) => byId.get(id);
}

/**
 * The row painted for a window read that STARTED at `readStamp` (0: no read
 * behind these rows, e.g. our own PATCH response).
 */
function paintedTitle(
  layer: EventChangeLayer,
  server: CalendarEvent[],
  readStamp = 0,
): string {
  return layer.paint(server, readStamp)[0]?.title ?? '';
}

// ── Display ────────────────────────────────────────────────────────────

test('paint: intent covers the server row and leaves it untouched', () => {
  const layer = new EventChangeLayer();
  layer.edit('e1', { summary: 'New' });

  const server = makeEvent();
  const painted = layer.paint([server]);

  assert.equal(painted.length, 1);
  assert.equal(painted[0].title, 'New');
  assert.equal(server.title, 'Old', 'the cache row is never mutated');
});

test('paint: a local create row is appended to the server list', () => {
  const layer = new EventChangeLayer();
  const local = makeEvent({ id: 'tmp_1', title: 'Draft' });
  layer.setLocalEvent('tmp_1', local);

  const painted = layer.paint([makeEvent()]);
  assert.deepEqual(
    painted.map((event) => event.id),
    ['e1', 'tmp_1'],
  );
  assert.equal(painted[1].title, 'Draft');
});

test('paint: delete intent hides the row; clear drops it', () => {
  const layer = new EventChangeLayer();
  layer.remove('e1');
  assert.deepEqual(layer.paint([makeEvent()]), []);
  assert.deepEqual(layer.getOverlay('e1'), { op: 'delete', id: 'e1' });

  layer.clear('e1');
  assert.equal(layer.size, 0);
  assert.equal(layer.paint([makeEvent()])[0].title, 'Old');
});

test('applyEventIntent: all-day clears zones; a move follows the calendar color', () => {
  const allDay = applyEventIntent(makeEvent({ start_time_zone: 'UTC' }), {
    is_all_day: true,
  });
  assert.equal(allDay.is_all_day, true);
  assert.equal(allDay.start_time_zone, '');
  assert.equal(allDay.end_time_zone, '');

  const moved = applyEventIntent(makeEvent(), { calendar_id: 'cal-2' });
  assert.equal(moved.calendar_id, 'cal-2');
  assert.notEqual(moved.color, undefined);
});

// ── Recording ──────────────────────────────────────────────────────────

test('recordRow: only the fields that changed become intent', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));

  // Exactly what the session paints: the current row with one field changed.
  layer.recordRow('e1', { ...base, title: 'New' });

  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'New' });
  assert.equal(paintedTitle(layer, [base]), 'New');
});

test('recordRow: a no-op paint records nothing', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));

  const changed = layer.recordRow('e1', { ...base });
  assert.equal(changed, false);
  assert.equal(layer.size, 0);
  assert.equal(layer.getOverlay('e1'), undefined);
});

test('recordRow: no known base keeps the whole row as intent', () => {
  const layer = new EventChangeLayer(() => undefined);
  layer.recordRow('e1', makeEvent({ title: 'New' }));

  const send = layer.unsentPatch('e1');
  assert.ok(send);
  assert.equal(send.summary, 'New');
  assert.equal(send.start, '2026-09-13T05:00:00.000Z');
});

// ── Sending: squash + no-op skip ───────────────────────────────────────

test('prepareSend: fields the server row already holds are skipped', () => {
  const base = makeEvent({ title: 'New' });
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New', description: 'note' });

  const prepared = layer.prepareSend('e1', { summary: 'New' }, base);
  assert.deepEqual(prepared.patch, {});
  assert.deepEqual(prepared.skipped, { summary: 'New' });

  layer.retireSkipped('e1', prepared.skipped);
  // The description is still unsent; only the no-op title retired.
  assert.deepEqual(layer.unsentPatch('e1'), { description: 'note' });
});

test('prepareSend: a real change keeps its body', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New' });

  const prepared = layer.prepareSend('e1', { summary: 'New' }, base);
  assert.deepEqual(prepared.patch, { summary: 'New' });
  assert.deepEqual(prepared.skipped, {});
});

test('prepareSend: an all-day write carries both time edges', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', {
    is_all_day: true,
    start: '2026-09-13',
    end: '2026-09-14',
  });

  const prepared = layer.prepareSend('e1', { is_all_day: true }, base);
  assert.deepEqual(prepared.patch, {
    is_all_day: true,
    start: '2026-09-13',
    end: '2026-09-14',
  });
});

test('prepareSend: a zone clear still carries the times', () => {
  const base = makeEvent({ start_time_zone: 'Europe/Berlin' });
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { start: '2026-09-13T09:00:00.000Z', start_time_zone: '' });

  const prepared = layer.prepareSend('e1', { start_time_zone: '' }, base);
  assert.deepEqual(prepared.patch, {
    start_time_zone: '',
    start: '2026-09-13T09:00:00.000Z',
    end: base.end_time,
  });
});

test('unsentPatch: an acknowledged value is not sent again', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'New' });
  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'New' });

  layer.acknowledge('e1', { summary: 'New' }, 2);
  assert.equal(layer.unsentPatch('e1'), null);
  // Still painted: only a server read may retire it.
  assert.equal(layer.getOverlay('e1')?.op, 'upsert');

  // A newer local value is unsent again.
  layer.edit('e1', { summary: 'Newest' });
  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'Newest' });
});

// ── Retirement: ack + a read that started after it ─────────────────────

test('retire: a read that started after the ack and agrees drops the field', () => {
  const layer = new EventChangeLayer();
  layer.edit('e1', { summary: 'New' });
  layer.acknowledge('e1', { summary: 'New' }, 2);

  const agreed = [makeEvent({ title: 'New' })];
  assert.equal(paintedTitle(layer, agreed, 3), 'New');
  assert.equal(layer.size, 0, 'agreement retires; the display is unchanged');
  assert.equal(layer.getOverlay('e1'), undefined);
});

test('retire: a stale read (started before the ack) neither retires nor repaints', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'New' });
  layer.acknowledge('e1', { summary: 'New' }, 2);

  // The GET started at 1, before the PATCH was acknowledged; it resolves
  // later and still carries the pre-write row (the gap-8 race).
  const stale = [makeEvent({ title: 'Old' })];

  assert.equal(paintedTitle(layer, stale, 1), 'New');
  assert.equal(layer.size, 1);
  assert.ok(layer.getOverlay('e1'), 'the intent is kept for the next read');
});

test('retire: a read that started after the ack and disagrees lets the server win', () => {
  const layer = new EventChangeLayer();
  layer.edit('e1', { summary: 'New' });
  layer.acknowledge('e1', { summary: 'New' }, 2);

  const remote = [makeEvent({ title: 'Remote' })];
  assert.equal(paintedTitle(layer, remote, 3), 'Remote');
  assert.equal(layer.size, 0, 'a post-ack disagreement must not mask forever');
});

test('retire: an unacknowledged field survives every read', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'New' });
  // No acknowledge(): the write never landed.

  const remote = [makeEvent({ title: 'Old' })];
  assert.equal(paintedTitle(layer, remote, 9), 'New');
  assert.ok(layer.getOverlay('e1'));
});

test('retire: a newer local value is not retired by an ack of the older one', () => {
  const layer = new EventChangeLayer();
  layer.edit('e1', { summary: 'A' });
  layer.acknowledge('e1', { summary: 'A' }, 2);
  layer.edit('e1', { summary: 'B' });

  // The read agrees with the acknowledged value, not the current intent.
  const agreed = [makeEvent({ title: 'A' })];
  assert.equal(paintedTitle(layer, agreed, 3), 'B');
  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'B' });
});

test('retire: no window read (our own write echo) never retires', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'New' });
  layer.acknowledge('e1', { summary: 'New' }, 2);

  // A PATCH response cached by the provider advances no window stamp.
  const echo = makeEvent({ title: 'New' });
  assert.equal(paintedTitle(layer, [echo], 0), 'New');
  assert.ok(layer.getOverlay('e1'), 'our own write is not a server read');
});

// ── Failure ────────────────────────────────────────────────────────────

test('failure: the intent stays painted and retryable', () => {
  const base = makeEvent();
  const layer = new EventChangeLayer(serverLookup(base));
  layer.edit('e1', { summary: 'New' });
  layer.noteFailure('e1');

  assert.equal(paintedTitle(layer, [base]), 'New');
  assert.deepEqual(layer.unsentPatch('e1'), { summary: 'New' });
  assert.equal(layer.hasFailedWrites(), true);

  layer.acknowledge('e1', { summary: 'New' }, 2);
  assert.equal(layer.hasFailedWrites(), false);
});

test('failure: a partial retry keeps the flag while a field is still unsent', () => {
  const layer = new EventChangeLayer(serverLookup(makeEvent()));
  layer.edit('e1', { summary: 'New', description: 'note' });
  layer.noteFailure('e1');

  layer.acknowledge('e1', { summary: 'New' }, 3);
  assert.equal(
    layer.hasFailedWrites(),
    true,
    'the note still needs the server',
  );

  layer.acknowledge('e1', { description: 'note' }, 4);
  assert.equal(layer.hasFailedWrites(), false);
});

// ── Local create lifecycle ─────────────────────────────────────────────

test('local create: draft marker, pick and settlement', () => {
  const layer = new EventChangeLayer();
  const local = makeEvent({ id: 'tmp_1', title: '' });
  layer.setLocalEvent('tmp_1', local);
  layer.markDraft('tmp_1');
  layer.markCalendarPicked('tmp_1');

  assert.equal(layer.getDraftId(), 'tmp_1');
  assert.equal(layer.isCalendarPicked('tmp_1'), true);

  // The POST is sent: no draft any more, the row still paints.
  layer.releaseDraft('tmp_1');
  assert.equal(layer.getDraftId(), null);
  assert.equal(layer.paint([]).length, 1);

  layer.settleCreate('tmp_1');
  assert.equal(layer.paint([]).length, 0);
  assert.equal(layer.size, 0, 'settle drops the local row and its metadata');
});

test('reset: drops every record', () => {
  const layer = new EventChangeLayer();
  layer.setLocalEvent('tmp_1', makeEvent({ id: 'tmp_1' }));
  layer.edit('e1', { summary: 'New' });
  layer.remove('e2');

  layer.reset();
  assert.equal(layer.size, 0);
  assert.deepEqual(
    layer.paint([makeEvent()]).map((e) => e.id),
    ['e1'],
  );
});

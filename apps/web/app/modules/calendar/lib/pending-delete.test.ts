// Unit tests for the deferred-delete state machine: the toast owns a grace
// window, and Undo must never have sent a request. The transitions also pin
// down the create-POST race (a server id parked mid-grace vs. after commit).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CalendarEvent } from '@/app/types';
import {
  commitPendingDelete,
  noteCreateSucceeded,
  pendingDeleteTitle,
  requestPendingDelete,
  undoPendingDelete,
  type PendingDelete,
  type PendingDeleteState,
} from './pending-delete';

function makeEvent(id: string, title = 'Deep work'): CalendarEvent {
  const start = new Date(2026, 8, 13, 5, 0);
  const end = new Date(2026, 8, 13, 6, 0);
  return {
    id,
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title,
    description: '',
    start_time: start.toISOString(),
    end_time: end.toISOString(),
    last_synced_at: '2026-09-08T00:00:00.000Z',
  };
}

function request(
  current: PendingDeleteState,
  id: string,
  draftId: string | null = null,
) {
  return requestPendingDelete(current, {
    id,
    event: makeEvent(id),
    draftId,
  });
}

/** Request a persisted delete and assert it hid via the overlay. */
function persisted(id: string): PendingDelete {
  const outcome = request(null, id);
  assert.equal(outcome.hide, 'overlay-delete');
  assert.ok(outcome.next && outcome.next.phase === 'pending');
  return outcome.next;
}

/** Request a temp delete and assert it hid by suppressing paint. */
function temp(id: string, draftId: string | null = null): PendingDelete {
  const outcome = request(null, id, draftId);
  assert.equal(outcome.hide, 'suppress-paint');
  assert.ok(outcome.next && outcome.next.phase === 'pending');
  return outcome.next;
}

// ── Persisted rows ──────────────────────────────────────────────────────

test('persisted request: no delete yet; commit deletes that id', () => {
  const pending = persisted('e1');

  // Requesting only hides: the machine carries no delete instruction.
  assert.equal(pending.serverId, 'e1');
  assert.equal(pending.tempId, null);

  const commit = commitPendingDelete(pending);
  assert.deepEqual(commit.effect, { kind: 'delete-server', id: 'e1' });
  assert.equal(commit.next, null, 'the record is gone after commit');
});

test('persisted undo before commit restores the overlay and does not delete', () => {
  const undo = undoPendingDelete(persisted('e1'));

  assert.deepEqual(undo.restore, { kind: 'clear-delete-overlay', id: 'e1' });
  assert.equal(undo.next, null);
  assert.equal(undo.selectId, 'e1', 'selection comes back with the chip');
  assert.equal(undo.removeCutSource, null);
});

test('undo after commit is a no-op: the record is already cleared', () => {
  const commit = commitPendingDelete(persisted('e1'));
  const undo = undoPendingDelete(commit.next);

  assert.deepEqual(undo.restore, { kind: 'none' });
  assert.equal(undo.next, null);
  assert.equal(undo.selectId, null);
});

test('a second delete commits the first persisted id, then tracks the new one', () => {
  const first = persisted('e1');
  const second = request(first, 'e2');

  assert.deepEqual(second.commitPrevious?.effect, {
    kind: 'delete-server',
    id: 'e1',
  });
  assert.ok(second.next && second.next.phase === 'pending');
  assert.equal(second.next.serverId, 'e2');
  assert.equal(second.hide, 'overlay-delete');
});

test('the same id twice during grace does not double-commit', () => {
  const first = persisted('e1');
  const again = request(first, 'e1');

  assert.equal(again.commitPrevious, null);
  assert.equal(again.hide, 'none', 'the toast already owns this delete');
  assert.equal(again.next, first);
});

// ── Temp ids (draft or in-flight create) ────────────────────────────────

test('temp request suppresses paint instead of placing a delete overlay', () => {
  const pending = temp('tmp_1');

  assert.equal(pending.tempId, 'tmp_1');
  assert.equal(pending.serverId, null);
  assert.equal(pending.hiddenId, 'tmp_1');
});

test('temp commit before a server id clears the temp overlay; no DELETE', () => {
  const commit = commitPendingDelete(temp('tmp_1'));

  assert.deepEqual(commit.effect, { kind: 'clear-temp-overlay', id: 'tmp_1' });
  // The committed marker is the late-create guard, not an undoable record.
  assert.deepEqual(commit.next, { phase: 'committed', tempId: 'tmp_1' });
});

test('temp undo unhides the chip and restores the draft id it had', () => {
  const undo = undoPendingDelete(temp('tmp_1', 'tmp_1'));

  assert.deepEqual(undo.restore, { kind: 'unhide-temp', id: 'tmp_1' });
  assert.equal(undo.draftId, 'tmp_1', 'the open draft becomes the draft again');
  assert.equal(undo.selectId, 'tmp_1');
  assert.equal(undo.removeCutSource, null);
});

test('a pasted temp id is not restored as a draft', () => {
  const undo = undoPendingDelete(temp('tmp_2', null));
  assert.equal(undo.draftId, null);
});

// ── Create POST landing mid-grace or after commit ───────────────────────

test('create success during grace parks the server id, hidden and not deleted', () => {
  const parked = noteCreateSucceeded(temp('tmp_1'), {
    tempId: 'tmp_1',
    serverId: 'srv_1',
  });

  assert.equal(parked.kind, 'park');
  assert.ok(parked.next && parked.next.phase === 'pending');
  assert.equal(parked.next.serverId, 'srv_1');
  assert.equal(parked.next.hiddenId, 'srv_1');
  assert.equal(parked.next.tempId, 'tmp_1');

  // Committing the parked record is the only DELETE, and it targets the row.
  const commit = commitPendingDelete(parked.next);
  assert.deepEqual(commit.effect, { kind: 'delete-server', id: 'srv_1' });
  assert.equal(commit.next, null);
});

test('undo after the server id is parked reveals it and does not delete', () => {
  const parked = noteCreateSucceeded(temp('tmp_1'), {
    tempId: 'tmp_1',
    serverId: 'srv_1',
  });
  assert.equal(parked.kind, 'park');
  assert.ok(parked.next);

  const undo = undoPendingDelete(parked.next);
  assert.deepEqual(undo.restore, { kind: 'reveal-server', id: 'srv_1' });
  assert.equal(undo.selectId, 'srv_1');
  assert.equal(undo.draftId, null, 'it is persisted now, not the open draft');
  assert.equal(undo.next, null);
});

test('create success after commit says ghost-delete (POST outlived the toast)', () => {
  const commit = commitPendingDelete(temp('tmp_1'));
  const late = noteCreateSucceeded(commit.next, {
    tempId: 'tmp_1',
    serverId: 'srv_1',
  });

  assert.equal(late.kind, 'ghost-delete');
  assert.equal(late.next, null, 'the tombstone is consumed by the delete');
});

test('create success for an unrelated temp id is not ours to delete', () => {
  const other = temp('tmp_9');
  const result = noteCreateSucceeded(other, {
    tempId: 'tmp_1',
    serverId: 'srv_1',
  });

  assert.equal(result.kind, 'none');
  assert.equal(result.next, other, 'the other pending delete is untouched');
});

// ── Paste cut source ────────────────────────────────────────────────────

test('grace success keeps the cut source; undo after park owes its removal', () => {
  const parked = noteCreateSucceeded(temp('tmp_1'), {
    tempId: 'tmp_1',
    serverId: 'srv_1',
    cutSourceId: 'src_1',
  });
  assert.equal(parked.kind, 'park');
  assert.ok(parked.next);

  // Parking itself has no removal instruction — the cut waits with the toast.
  assert.equal(parked.next.cutSourceId, 'src_1');

  const undo = undoPendingDelete(parked.next);
  assert.equal(undo.removeCutSource, 'src_1');
  assert.deepEqual(undo.restore, { kind: 'reveal-server', id: 'srv_1' });
});

test('commit after park deletes the pasted row and leaves the cut source', () => {
  const parked = noteCreateSucceeded(temp('tmp_1'), {
    tempId: 'tmp_1',
    serverId: 'srv_1',
    cutSourceId: 'src_1',
  });
  assert.equal(parked.kind, 'park');
  assert.ok(parked.next);

  const commit = commitPendingDelete(parked.next);
  assert.deepEqual(commit.effect, { kind: 'delete-server', id: 'srv_1' });
  assert.equal(commit.next, null, 'no cut-source step is reported');
});

test('a temp cut source is never parked for removal', () => {
  const parked = noteCreateSucceeded(temp('tmp_1'), {
    tempId: 'tmp_1',
    serverId: 'srv_1',
    cutSourceId: 'tmp_source',
  });
  assert.equal(parked.kind, 'park');
  assert.ok(parked.next);
  assert.equal(parked.next.cutSourceId, null);
});

// ── Toast title ─────────────────────────────────────────────────────────

test('pendingDeleteTitle is the overlaid title, and null with no toast', () => {
  assert.equal(pendingDeleteTitle(persisted('e1')), 'Deep work');
  assert.equal(pendingDeleteTitle(null), null);
  assert.equal(
    pendingDeleteTitle(commitPendingDelete(persisted('e1')).next),
    null,
    'no toast after the timer fires',
  );
});

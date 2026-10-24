// Deferred calendar delete: the event leaves the grid immediately, but the
// DELETE waits until the undo toast expires. React-free, so the race between
// the timer, Undo, and a create POST that lands mid-grace is unit-tested
// instead of living only inside the session hook.
//
// The session turns these instructions into overlay-queue calls, cache
// writes, its hide filter, the draft id, selection and delete mutations.

import type { CalendarEvent } from '@/app/types';
import { isTempEventId } from './event-overlays';

/** Grace window before the DELETE is sent. */
export const DELETE_UNDO_MS = 5000;

/** A delete that owns the undo toast right now. */
export interface PendingDelete {
  phase: 'pending';
  /** Id hidden from the grid: the temp id until a create parks, then server. */
  hiddenId: string;
  /** Client-only id when the event had no server row at request time. */
  tempId: string | null;
  /** Persisted id, or the server id a create returned during grace. */
  serverId: string | null;
  /** Unpersisted draft id to put back on undo; null when it was not one. */
  draftId: string | null;
  /** Cut source a successful paste would have deleted; null otherwise. */
  cutSourceId: string | null;
  /** Event as overlaid at request time (toast title / undo identity). */
  event: CalendarEvent;
}

/**
 * A committed temp delete keeps its temp id: the create POST may still be in
 * flight, and its success is what must remove the row it just made. There is
 * no undo window left, only that late-arrival guard.
 */
export interface CommittedTempDelete {
  phase: 'committed';
  tempId: string;
}

export type PendingDeleteState = PendingDelete | CommittedTempDelete | null;

/** How the session must hide the event for a new request. */
export type HideMode = 'overlay-delete' | 'suppress-paint' | 'none';

export type CommitEffect =
  | { kind: 'delete-server'; id: string }
  | { kind: 'clear-temp-overlay'; id: string }
  | { kind: 'none' };

export interface CommitOutcome {
  next: PendingDeleteState;
  effect: CommitEffect;
}

export interface RequestPendingDeleteInput {
  /** Selected event id at request time. */
  id: string;
  /** Overlaid event, so the toast title matches what the user saw. */
  event: CalendarEvent;
  /**
   * The open unpersisted draft id when this delete is that draft. A draft
   * whose create POST is already sent is no longer one (the session releases
   * the id at send), so Undo must not turn it back into a draft.
   */
  draftId: string | null;
}

export interface RequestOutcome {
  next: PendingDeleteState;
  hide: HideMode;
  /**
   * The previous grace-period delete, which must be committed before this
   * one starts: only one toast (and one timer) exists at a time.
   */
  commitPrevious: CommitOutcome | null;
}

/**
 * Start a deferred delete. Persisted ids get a delete overlay; temp ids are
 * only filtered out of the paint because removing their overlay would make an
 * in-flight create's onSuccess DELETE the server row immediately — and a
 * failed create must keep its overlay so Undo can restore it and a retry can
 * read its fields.
 */
export function requestPendingDelete(
  current: PendingDeleteState,
  input: RequestPendingDeleteInput,
): RequestOutcome {
  if (current?.phase === 'pending' && current.hiddenId === input.id) {
    // Second delete of the same event while its toast runs: already asked
    // for, and committing it here would send the DELETE behind the user.
    return { next: current, hide: 'none', commitPrevious: null };
  }

  const temp = isTempEventId(input.id);
  const commitPrevious =
    current?.phase === 'pending' ? commitPendingDelete(current) : null;

  const next: PendingDelete = {
    phase: 'pending',
    hiddenId: input.id,
    tempId: temp ? input.id : null,
    // A persisted request already knows its server row; a temp id must wait
    // for the create POST (if any) to return one.
    serverId: temp ? null : input.id,
    // Only the open local draft needs its id back on undo. A pasted temp id
    // is not a draft and must not be treated as one — same for a draft whose
    // create POST is in flight (it already released its draft id).
    draftId: temp ? input.draftId : null,
    cutSourceId: null,
    event: input.event,
  };

  return {
    next,
    hide: temp ? 'suppress-paint' : 'overlay-delete',
    commitPrevious,
  };
}

/**
 * Commit the pending delete. A persisted row (or a parked create result) is
 * DELETEd; a temp id with no server row only drops its overlay — if its POST
 * is still in flight, that onSuccess becomes the delete, and a failed create's
 * retry record is dropped by the session at the same moment.
 */
export function commitPendingDelete(
  current: PendingDeleteState,
): CommitOutcome {
  if (!current) return { next: null, effect: { kind: 'none' } };
  // Already committed: the toast is gone and there is nothing left to send.
  if (current.phase === 'committed') {
    return { next: current, effect: { kind: 'none' } };
  }

  if (current.serverId) {
    return {
      next: null,
      effect: { kind: 'delete-server', id: current.serverId },
    };
  }

  if (current.tempId) {
    return {
      next: { phase: 'committed', tempId: current.tempId },
      effect: { kind: 'clear-temp-overlay', id: current.hiddenId },
    };
  }

  // No server id and no temp id: nothing was hidden or sent.
  return { next: null, effect: { kind: 'none' } };
}

/** How Undo puts the event back on screen. */
export type Restore =
  | { kind: 'none' }
  | { kind: 'clear-delete-overlay'; id: string }
  | { kind: 'unhide-temp'; id: string }
  | { kind: 'reveal-server'; id: string };

export interface UndoOutcome {
  next: PendingDeleteState;
  restore: Restore;
  /** Selection to put back so the chip and inspector return. */
  selectId: string | null;
  /** Draft id to restore (temp draft only). */
  draftId: string | null;
  /** Paste source the create would have removed, now owed by Undo. */
  removeCutSource: string | null;
}

/**
 * Undo before the timer fires. Nothing was sent yet: for a persisted row the
 * delete overlay is dropped so the cache (including a PATCH that landed
 * during grace) shows through; a temp chip was never overlaid away. A parked
 * create means the row already exists, so it is revealed instead.
 */
export function undoPendingDelete(current: PendingDeleteState): UndoOutcome {
  const none: UndoOutcome = {
    next: current,
    restore: { kind: 'none' },
    selectId: null,
    draftId: null,
    removeCutSource: null,
  };
  // Committed / nothing pending: the grace window is over, so a later Undo is
  // a no-op (the toast is not even rendered).
  if (!current || current.phase !== 'pending') return none;

  // Create returned during grace: the server row is parked, not deleted.
  if (current.tempId && current.serverId) {
    return {
      next: null,
      restore: { kind: 'reveal-server', id: current.serverId },
      selectId: current.serverId,
      // It is persisted now, so it must not become the open draft again.
      draftId: null,
      removeCutSource: current.cutSourceId,
    };
  }

  // Still a local chip: its upsert overlay was never removed.
  if (current.tempId) {
    return {
      next: null,
      restore: { kind: 'unhide-temp', id: current.tempId },
      selectId: current.tempId,
      draftId: current.draftId,
      removeCutSource: null,
    };
  }

  return {
    next: null,
    restore: { kind: 'clear-delete-overlay', id: current.hiddenId },
    selectId: current.hiddenId,
    draftId: null,
    removeCutSource: null,
  };
}

export interface CreateSucceededInput {
  tempId: string;
  serverId: string;
  /** Cut source the paste would remove on a normal success. */
  cutSourceId?: string | null;
}

export type CreateSucceeded =
  | { kind: 'park'; next: PendingDelete; serverId: string }
  | { kind: 'ghost-delete'; next: null; serverId: string }
  | { kind: 'none'; next: PendingDeleteState; serverId: string };

/**
 * A create POST returned. While its temp delete is pending, park the server id
 * on that record: the toast still owns it, so it stays hidden and must not be
 * DELETEd yet. If the grace window already expired (committed temp delete),
 * the new row is a ghost — the same rule a discarded draft uses.
 */
export function noteCreateSucceeded(
  current: PendingDeleteState,
  input: CreateSucceededInput,
): CreateSucceeded {
  const { tempId, serverId } = input;
  const cutSourceId =
    input.cutSourceId && !isTempEventId(input.cutSourceId)
      ? input.cutSourceId
      : null;

  if (
    current?.phase === 'pending' &&
    current.tempId === tempId &&
    !current.serverId
  ) {
    return {
      kind: 'park',
      serverId,
      next: {
        ...current,
        hiddenId: serverId,
        serverId,
        cutSourceId,
      },
    };
  }

  if (current?.phase === 'committed' && current.tempId === tempId) {
    return { kind: 'ghost-delete', next: null, serverId };
  }

  return { kind: 'none', next: current, serverId };
}

/** Toast title for the pending delete, or null when no toast is owed. */
export function pendingDeleteTitle(current: PendingDeleteState): string | null {
  return current?.phase === 'pending' ? current.event.title : null;
}

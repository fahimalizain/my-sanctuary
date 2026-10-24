// Success / error policy for one settled entry of the per-event PATCH chain.
// React-free so it can be unit-tested and so the queue's owner (the app-wide
// CalendarEventsProvider) can keep its handlers stable.
//
// The layer is the durable copy of what the user asked for: a settled PATCH
// never removes intent. Success only records which value the server accepted
// (and its clock stamp), so a later server *read* can retire that field. A
// failure keeps the intent painted and unacknowledged — a failed edit is the
// user's data, not something to silently revert.

import type { CalendarEvent } from '@/app/types';
import type { EventChangeLayer } from './event-change-layer';
import type { EventPatch } from './event-patch-queue';

/**
 * A PATCH succeeded: fold the authoritative row into the durable cache and
 * acknowledge the values that were sent. The intent stays painted — a response
 * to our own write is not a server read, so it may never retire a field.
 */
export function settlePatchSuccess(args: {
  layer: EventChangeLayer;
  id: string;
  sent: EventPatch;
  event: CalendarEvent;
  /** Stamp of the acknowledgment (shared read/write clock). */
  stamp: number;
  upsertCache: (event: CalendarEvent) => void;
}): void {
  const { layer, id, sent, event, stamp, upsertCache } = args;
  layer.acknowledge(id, sent, stamp);
  upsertCache(event);
}

/**
 * A PATCH failed: keep the intent, mark it unsent/retryable. The caller
 * reports the write error; `retryFailedPatches` (or the next edit on this
 * event) re-sends exactly the fields the server has not accepted.
 */
export function settlePatchFailure(args: {
  layer: EventChangeLayer;
  id: string;
}): void {
  args.layer.noteFailure(args.id);
}

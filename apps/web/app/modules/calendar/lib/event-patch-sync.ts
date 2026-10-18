// Success / error policy for one settled entry of the per-event PATCH chain.
// React-free so it can be unit-tested and so the queue's owner (now the
// app-wide CalendarEventsProvider) can keep its handlers stable.

import type { CalendarEvent } from '@/app/types';
import { revertFailedPatch, type EventPatch } from './event-patch-queue';
import type { EventOverlay } from './event-overlays';

/** Overlay still matches server response for fields we paint optimistically. */
function overlayMatchesServerEvent(
  overlay: CalendarEvent,
  server: CalendarEvent,
): boolean {
  return (
    overlay.title === server.title &&
    overlay.description === server.description &&
    overlay.start_time === server.start_time &&
    overlay.end_time === server.end_time &&
    Boolean(overlay.is_all_day) === Boolean(server.is_all_day) &&
    (overlay.start_time_zone?.trim() || '') ===
      (server.start_time_zone?.trim() || '') &&
    (overlay.end_time_zone?.trim() || '') ===
      (server.end_time_zone?.trim() || '') &&
    overlay.calendar_id === server.calendar_id
  );
}

/**
 * A PATCH succeeded: fold the authoritative row into the durable cache, then
 * drop the overlay once nothing else is pending and it still paints exactly
 * what the server returned. A delete overlay stays (the delete owns the id).
 * `idle` false means more edits are queued, so the overlay must stay on top.
 */
export function reconcilePatchSuccess(args: {
  id: string;
  event: CalendarEvent;
  idle: boolean;
  getOverlay: (id: string) => EventOverlay | undefined;
  clear: (id: string) => void;
  upsertCache: (event: CalendarEvent) => void;
}): void {
  const { id, event, idle, getOverlay, clear, upsertCache } = args;

  upsertCache(event);
  // Later patches are still queued: keep the overlay painted over them.
  if (!idle) return;
  const latest = getOverlay(id);
  if (
    !latest ||
    (latest.op === 'upsert' && overlayMatchesServerEvent(latest.event, event))
  ) {
    clear(id);
  }
}

/**
 * A PATCH failed: revert only the failed fields, and only those not
 * superseded by a newer edit, back to the last known server row. The base
 * comes from the query cache (`findServerEvent`), not the page's events
 * array — the page may be gone after navigation. A delete overlay is never
 * reverted. The caller reports the write error itself.
 */
export function reconcilePatchError(args: {
  id: string;
  sent: EventPatch;
  idle: boolean;
  getOverlay: (id: string) => EventOverlay | undefined;
  clear: (id: string) => void;
  upsert: (event: CalendarEvent) => void;
  findServerEvent: (id: string) => CalendarEvent | undefined;
}): void {
  const { id, sent, idle, getOverlay, clear, upsert, findServerEvent } = args;

  const latest = getOverlay(id);
  if (latest?.op !== 'upsert') return;

  const base = findServerEvent(id);
  if (!base) {
    // Nothing to revert to: with nothing queued the overlay would be a lie.
    if (idle) clear(id);
    return;
  }

  const reverted = revertFailedPatch(latest.event, base, sent);
  if (idle && overlayMatchesServerEvent(reverted, base)) {
    clear(id);
  } else if (!overlayMatchesServerEvent(reverted, latest.event)) {
    upsert(reverted);
  }
}

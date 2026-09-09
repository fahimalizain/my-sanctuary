import type { CalendarEvent } from '@/app/types';

/** Client-only id prefix for events painted before POST returns a server id. */
export const TEMP_EVENT_PREFIX = 'tmp_';

export function isTempEventId(id: string): boolean {
  return id.startsWith(TEMP_EVENT_PREFIX);
}

export function newTempEventId(): string {
  return `${TEMP_EVENT_PREFIX}${crypto.randomUUID()}`;
}

/** Pending write painted over server calendar event lists. */
export type EventOverlay =
  | { op: 'upsert'; event: CalendarEvent }
  | { op: 'delete'; id: string };

function overlayId(overlay: EventOverlay): string {
  return overlay.op === 'upsert' ? overlay.event.id : overlay.id;
}

/**
 * Apply overlays to a server list. Last write per id wins.
 * upsert: replace matching id or append.
 * delete: drop that id.
 * Preserve relative order of server rows; append new upserts at the end.
 */
export function applyEventOverlays(
  server: CalendarEvent[],
  overlays: Iterable<EventOverlay>,
): CalendarEvent[] {
  const byId = new Map<string, EventOverlay>();
  for (const overlay of overlays) {
    byId.set(overlayId(overlay), overlay);
  }

  const result: CalendarEvent[] = [];
  const consumed = new Set<string>();

  for (const event of server) {
    const overlay = byId.get(event.id);
    if (!overlay) {
      result.push(event);
      continue;
    }
    consumed.add(event.id);
    if (overlay.op === 'delete') continue;
    result.push(overlay.event);
  }

  for (const [id, overlay] of byId) {
    if (consumed.has(id)) continue;
    if (overlay.op === 'upsert') {
      result.push(overlay.event);
    }
  }

  return result;
}

/** Drop all pending overlays (logout / account switch). */
export function resetEventOverlays(map: Map<string, EventOverlay>): void {
  map.clear();
}

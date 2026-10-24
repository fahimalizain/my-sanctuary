// Temp → server id aliases for optimistic creates. A create paints under a
// client-only `tmp_` id and flips to the server id when the POST returns; any
// gesture or callback that started under the temp id (a drag session, a queued
// inspector blur, a deferred selection) must keep addressing the same event
// afterwards, or it silently no-ops against an id that no longer exists.
//
// React-free so the lookup rules are unit-tested. The provider owns one
// instance for the whole app and drops it on account switch.

import { isTempEventId } from './event-overlays';

export class EventIdAliases {
  private byTempId = new Map<string, string>();

  /**
   * Remember that `tempId` became the server row `serverId`. Only a temp id
   * may be aliased, and only to a real id: aliasing the other way (or to
   * another temp) would mean two creates for one paint, which no caller does.
   */
  record(tempId: string, serverId: string): void {
    if (tempId === serverId) return;
    if (!isTempEventId(tempId) || isTempEventId(serverId)) return;
    this.byTempId.set(tempId, serverId);
  }

  /**
   * The server id a temp create id became, else the id itself. Unknown temp
   * ids are returned unchanged: the create may still be in flight, and the
   * caller decides what an unresolved temp id means.
   */
  resolve(id: string): string {
    if (!isTempEventId(id)) return id;
    return this.byTempId.get(id) ?? id;
  }

  /** Drop every alias (logout / account switch). */
  reset(): void {
    this.byTempId.clear();
  }
}

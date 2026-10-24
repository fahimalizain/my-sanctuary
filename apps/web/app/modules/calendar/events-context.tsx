import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { CalendarEvent } from '@/app/types';
import {
  findCachedCalendarEvent,
  upsertCalendarEventInCache,
  useUpdateCalendarEvent,
} from '@/app/queries/calendar';
import { useAuth } from '@/lib/auth';
import { nextCalendarStamp } from './lib/calendar-server-reads';
import { EventChangeLayer } from './lib/event-change-layer';
import { isTempEventId, type EventOverlay } from './lib/event-overlays';
import { EventIdAliases } from './lib/event-id-aliases';
import { EventPatchQueue, type EventPatch } from './lib/event-patch-queue';
import { settlePatchFailure, settlePatchSuccess } from './lib/event-patch-sync';
import { reportPatchWriteError } from './lib/patch-write-error';
import { UnsavedCreates, type UnsavedCreate } from './lib/unsaved-creates';
import { useCalendarRealtime } from './useCalendarRealtime';

export interface CalendarEventsQueue {
  /**
   * Paint a row. A temp id is a local-only create and is stored wholesale; a
   * server id records only the fields that differ from the current paint, so
   * a paint can never mask a remote edit by accident.
   */
  upsert(event: CalendarEvent): void;
  /** Record field intent for a server id and send what is not on the server. */
  edit(id: string, patch: EventPatch): void;
  /** Overlay a delete for this id. */
  remove(id: string): void;
  /** Drop everything this client holds for the id. */
  clear(id: string): void;
  /** Drop every record (logout / account switch). */
  reset(): void;
  /** Current paint for id (intent, local create row or delete). */
  getOverlay(id: string): EventOverlay | undefined;
  /**
   * Apply the local change layer to a server list. `readStamp` is the start
   * stamp of the window read that produced the list (0 when unknown): only a
   * read that started after an ack may retire that field.
   */
  apply(server: CalendarEvent[], readStamp?: number): CalendarEvent[];
  /** Serialize one PATCH for an event through the app-wide queue. */
  enqueuePatch(id: string, patch: EventPatch): void;
  /** Drop queued edits and ignore the in-flight result for this id. */
  cancelPatch(id: string): void;
  /** Re-send every field a failed PATCH left unacknowledged. */
  retryFailedPatches(): void;
  /** True while a failed PATCH still has intent the server does not have. */
  hasFailedWrites: boolean;
  /**
   * Follow a temp create id to the server row it became. Any gesture or
   * callback that started before the create returned (a drag session, a
   * queued inspector blur, a stale selection) still holds the temp id, and
   * every write must land on the server row.
   */
  resolveId(id: string): string;
  /** Remember the server id a temp create id became. */
  recordIdAlias(tempId: string, serverId: string): void;
  /**
   * A create POST returned: alias the temp id, adopt the server row and drop
   * the local-only row (the caller flushes edits made during the POST next).
   */
  settleCreate(input: { tempId: string; serverEvent: CalendarEvent }): void;
  /** An open draft is a temp row whose title blur has not POSTed yet. */
  markDraft(id: string): void;
  /** The draft was sent / discarded; the row is no longer a draft. */
  releaseDraft(id: string): void;
  getDraftId(): string | null;
  markCalendarPicked(id: string): void;
  isCalendarPicked(id: string): boolean;
  /** A create POST failed: keep the row and remember how to retry it. */
  markUnsavedCreate(record: UnsavedCreate): void;
  forgetUnsavedCreate(tempId: string): void;
  getUnsavedCreate(tempId: string): UnsavedCreate | undefined;
  /** Ids painted as local-only events whose create failed. */
  unsavedEventIds: ReadonlySet<string>;
  retryingCreateIds: ReadonlySet<string>;
  /** Claim the one retry POST for a temp id. False when one is in flight. */
  beginCreateRetry(tempId: string): boolean;
  endCreateRetry(tempId: string): void;
}

const noop = () => {};

const CalendarEventsContext = createContext<CalendarEventsQueue>({
  upsert: noop,
  edit: noop,
  remove: noop,
  clear: noop,
  reset: noop,
  getOverlay: () => undefined,
  apply: (server) => server,
  enqueuePatch: noop,
  cancelPatch: noop,
  retryFailedPatches: noop,
  hasFailedWrites: false,
  resolveId: (id) => id,
  recordIdAlias: noop,
  settleCreate: noop,
  markDraft: noop,
  releaseDraft: noop,
  getDraftId: () => null,
  markCalendarPicked: noop,
  isCalendarPicked: () => false,
  markUnsavedCreate: noop,
  forgetUnsavedCreate: noop,
  getUnsavedCreate: () => undefined,
  unsavedEventIds: new Set(),
  retryingCreateIds: new Set(),
  beginCreateRetry: () => false,
  endCreateRetry: noop,
});

/**
 * Read-time local change layer for calendar events: `server row ⊕ local
 * intent`, plus the local create / delete / retry lifecycle that must outlive
 * the calendar page. TanStack Query remains the durable server copy; nothing
 * this client asked for is dropped because one server response looked
 * consistent (see lib/event-change-layer.ts for the retirement rule).
 *
 * Mount inside QueryClientProvider (next to AuthProvider).
 */
export function CalendarEventsProvider({
  children,
}: {
  children: React.ReactNode;
}) {
  // One app-wide UserHub socket; invalidates events queries on remote changes.
  useCalendarRealtime();

  const { user } = useAuth();
  const accountId = user?.id ?? null;

  const queryClient = useQueryClient();

  // The layer is the physical store. `version` forces re-renders so consumers
  // re-run apply() after mutations; every read method below stays referentially
  // stable and reads the live layer instead.
  const [version, setVersion] = useState(0);
  const bump = useCallback(() => {
    setVersion((v) => v + 1);
  }, []);

  const layerRef = useRef<EventChangeLayer | null>(null);
  if (!layerRef.current) {
    // After navigation there is no mounted events array, so the last known
    // server row comes from the range-keyed query cache.
    layerRef.current = new EventChangeLayer((id) =>
      findCachedCalendarEvent(queryClient, id),
    );
  }
  const layer = layerRef.current;

  // Temp → server id aliases live next to the layer: both describe events this
  // client created before the server had a row for them.
  const idAliasesRef = useRef<EventIdAliases | null>(null);
  if (!idAliasesRef.current) idAliasesRef.current = new EventIdAliases();
  const aliases = idAliasesRef.current;

  // Failed create POSTs: local-only rows with no server id. Provider-owned so
  // the chip, its retry body and its "unsaved" marker survive navigation.
  const unsavedRef = useRef<UnsavedCreates | null>(null);
  if (!unsavedRef.current) unsavedRef.current = new UnsavedCreates();
  const unsaved = unsavedRef.current;
  // One retry POST per temp id at a time; provider-owned for the same reason.
  const retryingRef = useRef(new Set<string>());

  const upsert = useCallback(
    (event: CalendarEvent) => {
      if (isTempEventId(event.id)) {
        layer.setLocalEvent(event.id, event);
        bump();
        return;
      }
      if (layer.recordRow(event.id, event)) bump();
    },
    [bump, layer],
  );

  const edit = useCallback(
    (id: string, patch: EventPatch) => {
      // A temp row is a local-only create: update its live row in place (never
      // a stale render snapshot) and never PATCH it. Its create path owns it
      // until the server id exists.
      if (isTempEventId(id)) {
        layer.editLocal(id, patch);
        bump();
        return;
      }
      layer.edit(id, patch);
      const send = layer.unsentPatch(id);
      if (send) patchQueueRef.current?.enqueue(id, send);
      bump();
    },
    [bump, layer],
  );

  const remove = useCallback(
    (id: string) => {
      layer.remove(id);
      bump();
    },
    [bump, layer],
  );

  const clear = useCallback(
    (id: string) => {
      layer.clear(id);
      bump();
    },
    [bump, layer],
  );

  const reset = useCallback(() => {
    layer.reset();
    bump();
  }, [bump, layer]);

  // Stable: always reads the live layer so an async reconcile sees latest write.
  const getOverlay = useCallback(
    (id: string): EventOverlay | undefined => layer.getOverlay(id),
    [layer],
  );

  const apply = useCallback(
    (server: CalendarEvent[], readStamp = 0): CalendarEvent[] =>
      layer.paint(server, readStamp),
    // Re-bind when the layer changes so consumer memos that depend on `apply`
    // re-run (paint also retires fields a window read has settled).
    [layer, version],
  );

  // mutateAsync identity changes every render; the ref keeps the queue
  // instance (and its pending state) alive across renders.
  const updateEvent = useUpdateCalendarEvent();
  const updateRef = useRef(updateEvent);
  updateRef.current = updateEvent;

  // One PATCH serializer for the whole app. It lives here, not on the page:
  // leaving the route used to drop the queue, and a remount could race the
  // still-in-flight write. Handlers close over the stable callbacks above.
  const patchQueueRef = useRef<EventPatchQueue | null>(null);
  if (!patchQueueRef.current) {
    patchQueueRef.current = new EventPatchQueue({
      send: async (id, input) => {
        const result = await updateRef.current.mutateAsync({ id, input });
        return result.event;
      },
      prepare: (id, patch) =>
        layer.prepareSend(id, patch, findCachedCalendarEvent(queryClient, id)),
      onSkipped: (id, skipped) => {
        // The server row already holds these values: the intent behind them is
        // settled without a request.
        layer.retireSkipped(id, skipped);
        bump();
      },
      onSuccess: (id, sent, event) => {
        settlePatchSuccess({
          layer,
          id,
          sent,
          event,
          stamp: nextCalendarStamp(),
          upsertCache: upsertCalendarEventInCache,
        });
        bump();
      },
      onError: (id, _sent, err) => {
        // Keep the edit painted (and retryable); only the report is shown.
        settlePatchFailure({ layer, id });
        bump();
        // The calendar page registers the lone banner listener (see
        // patch-write-error): with the page gone the report is dropped rather
        // than setState'd on a dead component.
        reportPatchWriteError("Couldn't save event", err);
      },
    });
  }

  // The page calls these; both read the ref so they stay referentially stable
  // and a create that resolves after navigation still flushes into the live
  // queue.
  const enqueuePatch = useCallback(
    (id: string, patch: EventPatch) => {
      edit(id, patch);
    },
    [edit],
  );

  const cancelPatch = useCallback((id: string) => {
    patchQueueRef.current?.cancel(id);
  }, []);

  const retryFailedPatches = useCallback(() => {
    for (const id of layer.ids()) {
      // A request already in flight carries these fields; re-enqueueing them
      // would send the same values twice on success.
      if (patchQueueRef.current?.isBusy(id)) continue;
      const send = layer.unsentPatch(id);
      if (send) patchQueueRef.current?.enqueue(id, send);
    }
    bump();
  }, [bump, layer]);

  const resolveId = useCallback(
    (id: string): string => aliases.resolve(id),
    [aliases],
  );

  const recordIdAlias = useCallback(
    (tempId: string, serverId: string) => {
      aliases.record(tempId, serverId);
    },
    [aliases],
  );

  const settleCreate = useCallback(
    (input: { tempId: string; serverEvent: CalendarEvent }) => {
      const { tempId, serverEvent } = input;
      // Alias before any paint: a drag session or a queued blur still holds
      // the temp id and must address this row from now on.
      aliases.record(tempId, serverEvent.id);
      unsaved.clear(tempId);
      retryingRef.current.delete(tempId);
      layer.settleCreate(tempId);
      upsertCalendarEventInCache(serverEvent);
      bump();
    },
    [aliases, bump, layer, unsaved],
  );

  const markDraft = useCallback(
    (id: string) => {
      layer.markDraft(id);
      bump();
    },
    [bump, layer],
  );

  const releaseDraft = useCallback(
    (id: string) => {
      layer.releaseDraft(id);
      bump();
    },
    [bump, layer],
  );

  const getDraftId = useCallback(() => layer.getDraftId(), [layer]);

  const markCalendarPicked = useCallback(
    (id: string) => {
      layer.markCalendarPicked(id);
      bump();
    },
    [bump, layer],
  );

  const isCalendarPicked = useCallback(
    (id: string) => layer.isCalendarPicked(id),
    [layer],
  );

  const markUnsavedCreate = useCallback(
    (record: UnsavedCreate) => {
      unsaved.mark(record);
      bump();
    },
    [bump, unsaved],
  );

  const forgetUnsavedCreate = useCallback(
    (tempId: string) => {
      unsaved.clear(tempId);
      bump();
    },
    [bump, unsaved],
  );

  const getUnsavedCreate = useCallback(
    (tempId: string) => unsaved.get(tempId),
    [unsaved],
  );

  const beginCreateRetry = useCallback(
    (tempId: string): boolean => {
      if (retryingRef.current.has(tempId)) return false;
      retryingRef.current.add(tempId);
      bump();
      return true;
    },
    [bump],
  );

  const endCreateRetry = useCallback(
    (tempId: string) => {
      if (!retryingRef.current.delete(tempId)) return;
      bump();
    },
    [bump],
  );

  // Provider survives logout; clear every local record on identity change so
  // nothing leaks across accounts (queryClient.clear does not touch the layer).
  // Cancel queued/in-flight patches first: a response that resumes after
  // logout must not paint the previous account's event back. Aliases go too: a
  // temp id from the previous account must not resolve into this one's ids.
  useEffect(() => {
    patchQueueRef.current?.cancelAll();
    aliases.reset();
    unsaved.reset();
    retryingRef.current.clear();
    layer.reset();
    bump();
  }, [accountId, aliases, bump, layer, unsaved]);

  const unsavedEventIds = useMemo<ReadonlySet<string>>(
    () => new Set(unsaved.ids()),
    [unsaved, version],
  );
  const retryingCreateIds = useMemo<ReadonlySet<string>>(
    () => new Set(retryingRef.current),
    [version],
  );
  const hasFailedWrites = useMemo(
    () => layer.hasFailedWrites(),
    [layer, version],
  );

  const value = useMemo<CalendarEventsQueue>(
    () => ({
      upsert,
      edit,
      remove,
      clear,
      reset,
      getOverlay,
      apply,
      enqueuePatch,
      cancelPatch,
      retryFailedPatches,
      hasFailedWrites,
      resolveId,
      recordIdAlias,
      settleCreate,
      markDraft,
      releaseDraft,
      getDraftId,
      markCalendarPicked,
      isCalendarPicked,
      markUnsavedCreate,
      forgetUnsavedCreate,
      getUnsavedCreate,
      unsavedEventIds,
      retryingCreateIds,
      beginCreateRetry,
      endCreateRetry,
    }),
    [
      version,
      upsert,
      edit,
      remove,
      clear,
      reset,
      getOverlay,
      apply,
      enqueuePatch,
      cancelPatch,
      retryFailedPatches,
      hasFailedWrites,
      resolveId,
      recordIdAlias,
      settleCreate,
      markDraft,
      releaseDraft,
      getDraftId,
      markCalendarPicked,
      isCalendarPicked,
      markUnsavedCreate,
      forgetUnsavedCreate,
      getUnsavedCreate,
      unsavedEventIds,
      retryingCreateIds,
      beginCreateRetry,
      endCreateRetry,
    ],
  );

  return (
    <CalendarEventsContext.Provider value={value}>
      {children}
    </CalendarEventsContext.Provider>
  );
}

export function useCalendarEventsQueue(): CalendarEventsQueue {
  return useContext(CalendarEventsContext);
}

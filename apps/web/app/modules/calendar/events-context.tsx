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
import {
  applyEventOverlays,
  resetEventOverlays,
  type EventOverlay,
} from './lib/event-overlays';
import { EventPatchQueue, type EventPatch } from './lib/event-patch-queue';
import {
  reconcilePatchError,
  reconcilePatchSuccess,
} from './lib/event-patch-sync';
import { reportPatchWriteError } from './lib/patch-write-error';
import { useCalendarRealtime } from './useCalendarRealtime';

export interface CalendarEventsQueue {
  /** Snapshot of pending overlays (one entry per event id). */
  overlays: EventOverlay[];
  /** Last write wins per event.id — paints immediately over server data. */
  upsert(event: CalendarEvent): void;
  /** Overlay a delete for this id. */
  remove(id: string): void;
  /** Drop overlay after reconcile / error (server data takes over). */
  clear(id: string): void;
  /** Drop every overlay (logout / account switch). */
  reset(): void;
  /** Current overlay for id, if any. Always reads the live map. */
  getOverlay(id: string): EventOverlay | undefined;
  /** Apply current overlays to a server list. */
  apply(server: CalendarEvent[]): CalendarEvent[];
  /** Serialize one PATCH for an event through the app-wide queue. */
  enqueuePatch(id: string, patch: EventPatch): void;
  /** Drop queued edits and ignore the in-flight result for this id. */
  cancelPatch(id: string): void;
}

const CalendarEventsContext = createContext<CalendarEventsQueue>({
  overlays: [],
  upsert: () => {},
  remove: () => {},
  clear: () => {},
  reset: () => {},
  getOverlay: () => undefined,
  apply: (server) => server,
  enqueuePatch: () => {},
  cancelPatch: () => {},
});

/**
 * Read-time overlay queue for calendar events. Optimistic move/resize
 * paints via upsert; TanStack Query remains the durable source of truth.
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

  // Map is the physical store (last write per id). version forces re-renders
  // so consumers re-run apply() after mutations; getOverlay always reads ref.
  const mapRef = useRef(new Map<string, EventOverlay>());
  const [version, setVersion] = useState(0);

  const bump = useCallback(() => {
    setVersion((v) => v + 1);
  }, []);

  const upsert = useCallback(
    (event: CalendarEvent) => {
      mapRef.current.set(event.id, { op: 'upsert', event });
      bump();
    },
    [bump],
  );

  const remove = useCallback(
    (id: string) => {
      mapRef.current.set(id, { op: 'delete', id });
      bump();
    },
    [bump],
  );

  const clear = useCallback(
    (id: string) => {
      if (!mapRef.current.has(id)) return;
      mapRef.current.delete(id);
      bump();
    },
    [bump],
  );

  const reset = useCallback(() => {
    if (mapRef.current.size === 0) return;
    resetEventOverlays(mapRef.current);
    bump();
  }, [bump]);

  // Stable: always reads the live map so async reconcile sees latest write.
  const getOverlay = useCallback(
    (id: string): EventOverlay | undefined => mapRef.current.get(id),
    [],
  );

  const apply = useCallback(
    (server: CalendarEvent[]): CalendarEvent[] =>
      applyEventOverlays(server, mapRef.current.values()),
    // Re-bind when overlays change so consumer memos that depend on `apply` re-run.
    [version],
  );

  const queryClient = useQueryClient();
  // mutateAsync identity changes every render; the ref keeps the queue
  // instance (and its pending state) alive across renders.
  const updateEvent = useUpdateCalendarEvent();
  const updateRef = useRef(updateEvent);
  updateRef.current = updateEvent;

  // One PATCH serializer for the whole app. It used to live in the calendar
  // page, where leaving the route dropped the queue and a remount could race
  // the still-in-flight write; here it outlives navigation and still
  // reconciles its overlay. Handlers close over the stable callbacks above —
  // never the context value object, which changes on every overlay bump.
  const patchQueueRef = useRef<EventPatchQueue | null>(null);
  if (!patchQueueRef.current) {
    patchQueueRef.current = new EventPatchQueue({
      send: async (id, input) => {
        const result = await updateRef.current.mutateAsync({ id, input });
        return result.event;
      },
      onSuccess: (id, _sent, event, idle) => {
        reconcilePatchSuccess({
          id,
          event,
          idle,
          getOverlay,
          clear,
          upsertCache: upsertCalendarEventInCache,
        });
      },
      onError: (id, sent, err, idle) => {
        reconcilePatchError({
          id,
          sent,
          idle,
          getOverlay,
          clear,
          upsert,
          // After navigation there is no mounted events array, so the last
          // known server row comes from the range-keyed query cache.
          findServerEvent: (eventId) =>
            findCachedCalendarEvent(queryClient, eventId),
        });
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
  const enqueuePatch = useCallback((id: string, patch: EventPatch) => {
    patchQueueRef.current?.enqueue(id, patch);
  }, []);

  const cancelPatch = useCallback((id: string) => {
    patchQueueRef.current?.cancel(id);
  }, []);

  // Provider survives logout; clear overlays on identity change so they
  // never leak across accounts (queryClient.clear does not touch this map).
  // Cancel queued/in-flight patches first: a response that resumes after
  // logout must not paint the previous account's event back onto the overlay.
  useEffect(() => {
    patchQueueRef.current?.cancelAll();
    reset();
  }, [accountId, reset]);

  const value = useMemo<CalendarEventsQueue>(
    () => ({
      overlays: Array.from(mapRef.current.values()),
      upsert,
      remove,
      clear,
      reset,
      getOverlay,
      apply,
      enqueuePatch,
      cancelPatch,
    }),
    [
      version,
      upsert,
      remove,
      clear,
      reset,
      getOverlay,
      apply,
      enqueuePatch,
      cancelPatch,
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

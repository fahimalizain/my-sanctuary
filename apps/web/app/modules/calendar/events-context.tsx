import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import type { CalendarEvent } from '@/app/types';
import { useAuth } from '@/lib/auth';
import {
  applyEventOverlays,
  resetEventOverlays,
  type EventOverlay,
} from './lib/event-overlays';
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
}

const CalendarEventsContext = createContext<CalendarEventsQueue>({
  overlays: [],
  upsert: () => {},
  remove: () => {},
  clear: () => {},
  reset: () => {},
  getOverlay: () => undefined,
  apply: (server) => server,
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

  // Provider survives logout; clear overlays on identity change so they
  // never leak across accounts (queryClient.clear does not touch this map).
  useEffect(() => {
    reset();
  }, [accountId, reset]);

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

  const value = useMemo<CalendarEventsQueue>(
    () => ({
      overlays: Array.from(mapRef.current.values()),
      upsert,
      remove,
      clear,
      reset,
      getOverlay,
      apply,
    }),
    [version, upsert, remove, clear, reset, getOverlay, apply],
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

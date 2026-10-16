import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from 'react';
import {
  removeCalendarEventFromCache,
  upsertCalendarEventInCache,
  useCalendarEventsQuery,
  useCalendarsQuery,
  useCreateCalendarEvent,
  useDeleteCalendarEvent,
  useUpdateCalendarEvent,
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { useCalendarEventsQueue } from '../events-context';
import {
  allDayPreviewIndices,
  timedPreviewSegments,
  type DragPreviewInfo,
  type TimedRange,
} from '../lib/calendar-drag';
import {
  buildAllDayChips,
  buildEventsByDay,
  clickCreateTimesFromSlot,
  eventChipColor,
} from '../lib/calendar-model';
import { hourLabelStep } from '../lib/calendar-zoom';
import { isPersistableDraftTitle } from '../lib/event-draft';
import { DESKTOP_MQ } from '../lib/inspector-anchor';
import { isTempEventId, newTempEventId } from '../lib/event-overlays';
import {
  buildDraftFlushPatch,
  EventPatchQueue,
  revertFailedPatch,
  type EventPatchQueueHandlers,
} from '../lib/event-patch-queue';
import { repairCalendar } from '@/lib/api/calendar';
import {
  repairTargetCalendarId,
  selectSyncHealthBanner,
} from '../lib/sync-health';
import { useCalendarDrag } from './useCalendarDrag';
import { useCalendarZoom } from './useCalendarZoom';
import {
  COL_HEADER_H,
  colorForCalendar,
  defaultWritableCalendar,
  hourGridBackground,
  hourHeight as computeHourHeight,
  isMultiDay,
  isSameDay,
} from '../lib/week-layout';

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

export interface CalendarSessionInput {
  timeMin: string;
  timeMax: string;
  days: Date[];
  dayCount: number;
  scrollerHeight: number;
  scrollerRef: RefObject<HTMLElement | null>;
  setStripLocked: (locked: boolean) => void;
}

/**
 * Events query, calendar filter, inspector, mutations, drag, packing, and
 * now-tick for the calendar page. Owns everything the strip does not.
 */
export function useCalendarSession({
  timeMin,
  timeMax,
  days,
  dayCount,
  scrollerHeight,
  scrollerRef,
  setStripLocked,
}: CalendarSessionInput) {
  const eventsQuery = useCalendarEventsQuery(timeMin, timeMax);
  const events = eventsQuery.data?.events ?? [];
  const sync = eventsQuery.data?.sync;
  const isLoading = eventsQuery.isLoading;
  const isRefreshing = eventsQuery.isFetching && !eventsQuery.isLoading;
  const error =
    eventsQuery.error instanceof Error
      ? eventsQuery.error.message
      : eventsQuery.error
        ? 'Failed to load events'
        : null;
  const eventsEmpty = events.length === 0;

  // Optimistic create/move/resize/delete overlay (read-time; not setQueryData).
  const queue = useCalendarEventsQueue();

  // Calendars for sidebar list + visibility filter.
  const calendarsQuery = useCalendarsQuery();
  const calendars = calendarsQuery.data?.calendars ?? [];
  const calendarsError =
    calendarsQuery.error instanceof Error
      ? calendarsQuery.error.message
      : calendarsQuery.error
        ? 'Failed to load calendars'
        : null;

  const healthBanner = useMemo(
    () =>
      selectSyncHealthBanner({
        sync,
        calendars,
        fetchError: error,
        eventsEmpty,
        isLoading,
      }),
    [sync, calendars, error, eventsEmpty, isLoading],
  );

  const retry = () => {
    const calendarId = repairTargetCalendarId(healthBanner);
    if (calendarId) {
      void repairCalendar(calendarId).finally(() => {
        void eventsQuery.refetch();
      });
      return;
    }
    void eventsQuery.refetch();
  };

  const [selectedCalendarIds, setSelectedCalendarIds] = useState<Set<string>>(
    () => new Set(),
  );
  // Distinguishes pre-init (empty Set = show all) from user unchecking every
  // calendar (empty Set = show none). Flipped once calendars first arrive.
  const [selectionReady, setSelectionReady] = useState(false);

  // Event inspector selection (chip click or after click-to-create).
  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  // Focus title only right after click-to-create, not on every chip select.
  const [focusTitleOnOpen, setFocusTitleOnOpen] = useState(false);
  // Local-only draft (overlay upsert, no POST yet). Ref is the source of
  // truth inside stable callbacks; state mirrors it for React identity.
  const [unpersistedDraftId, setUnpersistedDraftId] = useState<string | null>(
    null,
  );
  const unpersistedDraftIdRef = useRef<string | null>(null);
  const setDraftId = useCallback((id: string | null) => {
    unpersistedDraftIdRef.current = id;
    setUnpersistedDraftId(id);
  }, []);
  // Prevent a second title-blur from firing another POST for the same draft.
  const draftPersistStartedRef = useRef<Set<string>>(new Set());

  const createEvent = useCreateCalendarEvent();
  const updateEvent = useUpdateCalendarEvent();
  const deleteEvent = useDeleteCalendarEvent();

  // Quiet notice for failed writes (distinct from sync health banner).
  const [writeError, setWriteError] = useState<string | null>(null);
  const clearWriteError = useCallback(() => setWriteError(null), []);

  const reportWriteError = useCallback((base: string, err: unknown) => {
    if (err instanceof Error && err.message.trim()) {
      setWriteError(`${base}: ${err.message}`);
      return;
    }
    setWriteError(base);
  }, []);

  // Per-event PATCH chain shared by inspector edits, drag move/resize and the
  // temp-id create flush. Handlers are re-bound each render through a ref so
  // the queue instance (and its pending state) outlives renders.
  const eventsRef = useRef(events);
  eventsRef.current = events;
  const patchSyncRef = useRef<EventPatchQueueHandlers | null>(null);
  patchSyncRef.current = {
    send: async (id, input) => {
      const result = await updateEvent.mutateAsync({ id, input });
      return result.event;
    },
    onSuccess: (id, _sent, event, idle) => {
      upsertCalendarEventInCache(event);
      // Later patches are still queued: keep the overlay painted over them.
      if (!idle) return;
      const latest = queue.getOverlay(id);
      if (
        !latest ||
        (latest.op === 'upsert' &&
          overlayMatchesServerEvent(latest.event, event))
      ) {
        queue.clear(id);
      }
    },
    onError: (id, sent, err, idle) => {
      const latest = queue.getOverlay(id);
      if (latest?.op === 'upsert') {
        // Revert only the failed fields, and only those not superseded by a
        // newer edit, back to the last known server row.
        const base = eventsRef.current.find((e) => e.id === id);
        if (!base) {
          if (idle) queue.clear(id);
        } else {
          const reverted = revertFailedPatch(latest.event, base, sent);
          if (idle && overlayMatchesServerEvent(reverted, base)) {
            queue.clear(id);
          } else if (!overlayMatchesServerEvent(reverted, latest.event)) {
            queue.upsert(reverted);
          }
        }
      }
      reportWriteError("Couldn't save event", err);
    },
  };
  const patchQueueRef = useRef<EventPatchQueue | null>(null);
  if (!patchQueueRef.current) {
    patchQueueRef.current = new EventPatchQueue({
      send: (id, input) => patchSyncRef.current!.send(id, input),
      onSuccess: (...args) => patchSyncRef.current!.onSuccess(...args),
      onError: (...args) => patchSyncRef.current!.onError(...args),
    });
  }
  const patchQueue = patchQueueRef.current;

  const writableCalendar = useMemo(
    () => defaultWritableCalendar(calendars),
    [calendars],
  );

  // Default: select every calendar once the list first arrives.
  useEffect(() => {
    if (calendars.length === 0 || selectionReady) return;
    setSelectedCalendarIds(new Set(calendars.map((c) => c.id)));
    setSelectionReady(true);
  }, [calendars, selectionReady]);

  // Overlay first so inspector shows optimistic create/move/resize.
  const overlaidEvents = useMemo(() => queue.apply(events), [queue, events]);

  const selectedEvent = useMemo(() => {
    if (!selectedEventId) return null;
    return overlaidEvents.find((e) => e.id === selectedEventId) ?? null;
  }, [overlaidEvents, selectedEventId]);

  // Drop selection if the event disappeared (deleted / filtered out of cache).
  useEffect(() => {
    if (selectedEventId && !selectedEvent && !eventsQuery.isFetching) {
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
    }
  }, [selectedEventId, selectedEvent, eventsQuery.isFetching]);

  const selectedEventCalendar = useMemo(() => {
    if (!selectedEvent) return undefined;
    return calendars.find((c) => c.id === selectedEvent.calendar_id);
  }, [calendars, selectedEvent]);

  const discardUnpersistedDraft = useCallback(() => {
    const id = unpersistedDraftIdRef.current;
    if (!id) return;
    // clear (not remove): never on the server; if POST is in flight, empty
    // overlay makes create onSuccess DELETE the just-created server row.
    queue.clear(id);
    setDraftId(null);
  }, [queue, setDraftId]);

  const closeInspector = useCallback(() => {
    discardUnpersistedDraft();
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);
  }, [discardUnpersistedDraft]);

  const selectEvent = useCallback(
    (eventId: string) => {
      const draftId = unpersistedDraftIdRef.current;
      if (draftId && draftId !== eventId) {
        discardUnpersistedDraft();
      }
      setSelectedEventId(eventId);
      setFocusTitleOnOpen(false);
    },
    [discardUnpersistedDraft],
  );

  const beginDraft = useCallback(
    (range: TimedRange) => {
      if (!writableCalendar) {
        closeInspector();
        return;
      }

      // Replace any previous local draft (no POST was made for it).
      const prevDraftId = unpersistedDraftIdRef.current;
      if (prevDraftId) {
        queue.clear(prevDraftId);
        setDraftId(null);
      }

      const tempId = newTempEventId();
      const startIso = range.start.toISOString();
      const endIso = range.end.toISOString();

      const optimistic: CalendarEvent = {
        id: tempId,
        calendar_id: writableCalendar.id,
        google_event_id: '',
        title: '',
        description: '',
        start_time: startIso,
        end_time: endIso,
        last_synced_at: new Date().toISOString(),
        color: colorForCalendar(writableCalendar.id),
      };

      // Paint chip + open inspector — no network until title blur.
      queue.upsert(optimistic);
      setDraftId(tempId);
      setSelectedEventId(tempId);
      setFocusTitleOnOpen(true);
    },
    [writableCalendar, closeInspector, queue, setDraftId],
  );

  const persistDraft = useCallback(() => {
    const tempId = unpersistedDraftIdRef.current;
    if (!tempId) return;

    const latest = queue.getOverlay(tempId);
    if (!latest || latest.op !== 'upsert') return;
    if (!isPersistableDraftTitle(latest.event.title)) return;
    if (draftPersistStartedRef.current.has(tempId)) return;

    const event = latest.event;
    const postedSummary = event.title.trim();
    const postedDescription = event.description ?? '';
    const startIso = event.start_time;
    const endIso = event.end_time;

    draftPersistStartedRef.current.add(tempId);
    clearWriteError();

    createEvent.mutate(
      {
        calendar_id: event.calendar_id,
        summary: postedSummary,
        ...(postedDescription ? { description: postedDescription } : {}),
        start: startIso,
        end: endIso,
      },
      {
        onSuccess: (result) => {
          draftPersistStartedRef.current.delete(tempId);
          const after = queue.getOverlay(tempId);

          // User deleted / discarded while create was in flight — do not
          // cache-write; DELETE the just-created server event so Google has
          // no ghost.
          if (!after || after.op === 'delete') {
            queue.clear(tempId);
            if (unpersistedDraftIdRef.current === tempId) {
              setDraftId(null);
            }
            void deleteEvent.mutateAsync(result.event.id).then(() => {
              removeCalendarEventFromCache(result.event.id);
            });
            return;
          }

          // Remap selection before clear/cache so selectedEvent never
          // resolves null for the old temp id (effect would close inspector).
          setSelectedEventId((prev) =>
            prev === tempId ? result.event.id : prev,
          );
          if (unpersistedDraftIdRef.current === tempId) {
            setDraftId(null);
          }
          queue.clear(tempId);
          upsertCalendarEventInCache(result.event);

          // If the user renamed / moved / annotated the temp event before
          // POST returned, keep those fields under the server id and flush
          // field PATCH first, then calendar MOVE (cannot combine).
          if (after.op === 'upsert') {
            const local = after.event;
            const flush = buildDraftFlushPatch({
              posted: {
                summary: postedSummary,
                description: postedDescription,
                start: startIso,
                end: endIso,
              },
              local,
              serverCalendarId: result.event.calendar_id,
            });

            if (flush) {
              queue.upsert({
                ...result.event,
                title: local.title,
                description: local.description,
                start_time: local.start_time,
                end_time: local.end_time,
                is_all_day: local.is_all_day,
                start_time_zone: local.start_time_zone,
                end_time_zone: local.end_time_zone,
                calendar_id: local.calendar_id,
                color: local.color ?? colorForCalendar(local.calendar_id),
              });
              // Same per-event chain as inspector edits: field PATCH first,
              // then calendar MOVE, ahead of any later edit.
              patchQueue.enqueue(result.event.id, flush);
            }
          }
        },
        onError: (err) => {
          draftPersistStartedRef.current.delete(tempId);
          queue.clear(tempId);
          if (unpersistedDraftIdRef.current === tempId) {
            setDraftId(null);
          }
          setSelectedEventId((prev) => {
            if (prev === tempId) {
              setFocusTitleOnOpen(false);
              return null;
            }
            return prev;
          });
          reportWriteError("Couldn't create event", err);
        },
      },
    );
  }, [
    clearWriteError,
    createEvent,
    deleteEvent,
    queue,
    patchQueue,
    reportWriteError,
    setDraftId,
  ]);

  const handleSaveTitle = useCallback(
    (summary: string) => {
      if (!selectedEventId) return;
      const current = overlaidEvents.find((e) => e.id === selectedEventId);
      if (!current) return;

      clearWriteError();

      // Paint immediately.
      queue.upsert({ ...current, title: summary });

      // Unpersisted draft: title blur is the sole persist trigger.
      if (
        selectedEventId === unpersistedDraftIdRef.current ||
        selectedEventId === unpersistedDraftId
      ) {
        persistDraft();
        return;
      }

      // Temp id with POST in flight — create onSuccess will flush the title.
      if (isTempEventId(selectedEventId)) return;

      patchQueue.enqueue(selectedEventId, { summary });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      patchQueue,
      persistDraft,
      clearWriteError,
    ],
  );

  const handleSaveDescription = useCallback(
    (description: string) => {
      if (!selectedEventId) return;
      const current = overlaidEvents.find((e) => e.id === selectedEventId);
      if (!current) return;

      clearWriteError();

      // Paint immediately.
      queue.upsert({ ...current, description });

      // Unpersisted draft: paint overlay and try persist. Create stays gated
      // by isPersistableDraftTitle — description-only empty-title drafts do
      // not POST. Title blur remains the primary persist trigger.
      if (
        selectedEventId === unpersistedDraftIdRef.current ||
        selectedEventId === unpersistedDraftId
      ) {
        persistDraft();
        return;
      }

      // Temp id with POST in flight — create onSuccess will flush description.
      if (isTempEventId(selectedEventId)) return;

      patchQueue.enqueue(selectedEventId, { description });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      patchQueue,
      persistDraft,
      clearWriteError,
    ],
  );

  const handleSaveCalendar = useCallback(
    (calendarId: string) => {
      if (!selectedEventId) return;
      const current = overlaidEvents.find((e) => e.id === selectedEventId);
      if (!current) return;
      if (current.calendar_id === calendarId) return;

      clearWriteError();

      // Paint immediately (color follows destination calendar).
      queue.upsert({
        ...current,
        calendar_id: calendarId,
        color: colorForCalendar(calendarId),
      });

      // Ensure destination is visible so the chip does not vanish.
      setSelectedCalendarIds((prev) => {
        if (prev.has(calendarId)) return prev;
        const next = new Set(prev);
        next.add(calendarId);
        return next;
      });

      // Unpersisted draft or temp id: overlay only; create POSTs calendar_id.
      // In-flight create onSuccess flushes MOVE after any field PATCH.
      if (
        selectedEventId === unpersistedDraftIdRef.current ||
        selectedEventId === unpersistedDraftId ||
        isTempEventId(selectedEventId)
      ) {
        return;
      }

      patchQueue.enqueue(selectedEventId, { calendar_id: calendarId });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      patchQueue,
      clearWriteError,
    ],
  );

  const handleDeleteEvent = useCallback(async () => {
    if (!selectedEventId) return;
    const id = selectedEventId;

    clearWriteError();

    // Draft or in-flight create: drop overlay only — never DELETE a missing
    // server row. If POST is in flight, clear makes onSuccess DELETE it.
    if (
      isTempEventId(id) ||
      id === unpersistedDraftIdRef.current ||
      id === unpersistedDraftId
    ) {
      patchQueue.cancel(id);
      queue.clear(id);
      if (unpersistedDraftIdRef.current === id || unpersistedDraftId === id) {
        setDraftId(null);
      }
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
      return;
    }

    // Chip gone now; close inspector immediately. Cancel queued/in-flight
    // PATCHes so they neither 404 nor resurrect the row in the cache.
    patchQueue.cancel(id);
    queue.remove(id);
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);

    try {
      await deleteEvent.mutateAsync(id);
      removeCalendarEventFromCache(id);
      queue.clear(id);
    } catch (err) {
      // Clear delete overlay so the event reappears from the server list.
      queue.clear(id);
      reportWriteError("Couldn't delete event", err);
    }
  }, [
    selectedEventId,
    unpersistedDraftId,
    queue,
    patchQueue,
    deleteEvent,
    setDraftId,
    clearWriteError,
    reportWriteError,
  ]);

  const knownCalendarIds = useMemo(
    () => new Set(calendars.map((c) => c.id)),
    [calendars],
  );

  const toggleCalendar = useCallback((calendarId: string) => {
    setSelectedCalendarIds((prev) => {
      const next = new Set(prev);
      if (next.has(calendarId)) {
        next.delete(calendarId);
      } else {
        next.add(calendarId);
      }
      return next;
    });
  }, []);

  // Overlay first, then calendar-visibility filter.
  const visibleEvents = useMemo(() => {
    return overlaidEvents.filter((e) => {
      if (!selectionReady) return true;
      if (selectedCalendarIds.has(e.calendar_id)) return true;
      if (!knownCalendarIds.has(e.calendar_id)) return true;
      return false;
    });
  }, [overlaidEvents, selectedCalendarIds, knownCalendarIds, selectionReady]);

  const { timedEvents, allDayEvents } = useMemo(() => {
    const timed: CalendarEvent[] = [];
    const allDay: CalendarEvent[] = [];
    for (const e of visibleEvents) {
      if (e.is_all_day) {
        allDay.push(e);
        continue;
      }
      const start = new Date(e.start_time);
      const end = new Date(e.end_time);
      if (isMultiDay(start, end)) {
        allDay.push(e);
      } else {
        timed.push(e);
      }
    }
    return { timedEvents: timed, allDayEvents: allDay };
  }, [visibleEvents]);

  // All-day chips first so we know band height for hour stretch.
  const { allDayChips, allDayHeight } = useMemo(
    () => buildAllDayChips(days, allDayEvents, dayCount),
    [days, allDayEvents, dayCount],
  );

  // Headers + all-day are sticky inside the scroller; hours fill the rest.
  const availableHoursPx = useMemo(() => {
    if (scrollerHeight <= 0) return 0;
    return Math.max(0, scrollerHeight - COL_HEADER_H - allDayHeight);
  }, [scrollerHeight, allDayHeight]);

  const autoHourH = useMemo(
    () => computeHourHeight(availableHoursPx),
    [availableHoursPx],
  );

  // Pinch callbacks assigned after drag is created (drag needs hourH first).
  const onPinchStartRef = useRef<(() => void) | null>(null);
  const onPinchEndRef = useRef<(() => void) | null>(null);

  const { hourH } = useCalendarZoom({
    scrollerRef,
    autoHourH,
    headerOffset: COL_HEADER_H + allDayHeight,
    onPinchStartRef,
    onPinchEndRef,
  });
  const totalHoursH = hourH * 24;

  const handleMoveOrResize = useCallback(
    (eventId: string, range: TimedRange) => {
      const current =
        overlaidEvents.find((e) => e.id === eventId) ??
        events.find((e) => e.id === eventId);
      if (!current) return;
      const next: CalendarEvent = {
        ...current,
        start_time: range.start.toISOString(),
        end_time: range.end.toISOString(),
      };
      clearWriteError();
      // Paint immediately; PATCH follows (unless still a temp id).
      queue.upsert(next);

      // Temp events have no server row — create onSuccess will flush times.
      if (isTempEventId(eventId)) return;

      patchQueue.enqueue(eventId, {
        start: next.start_time,
        end: next.end_time,
      });
    },
    [overlaidEvents, events, queue, patchQueue, clearWriteError],
  );

  /** Inspector time/date edits — same overlay + PATCH path as drag/resize. */
  const handleSaveTimes = useCallback(
    (startIso: string, endIso: string) => {
      if (!selectedEventId) return;
      const start = new Date(startIso);
      const end = new Date(endIso);
      if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return;
      handleMoveOrResize(selectedEventId, { start, end });
    },
    [selectedEventId, handleMoveOrResize],
  );

  /** Inspector all-day toggle / all-day date shift. */
  const handleSaveAllDay = useCallback(
    (next: { isAllDay: boolean; startIso: string; endIso: string }) => {
      if (!selectedEventId) return;
      const current = overlaidEvents.find((e) => e.id === selectedEventId);
      if (!current) return;

      clearWriteError();

      const painted: CalendarEvent = {
        ...current,
        is_all_day: next.isAllDay,
        start_time: next.startIso,
        end_time: next.endIso,
        // All-day is civil dates; clear zones on the overlay when switching on.
        ...(next.isAllDay ? { start_time_zone: '', end_time_zone: '' } : {}),
      };
      queue.upsert(painted);

      // Temp / draft: overlay only; create onSuccess flushes field PATCH.
      if (
        selectedEventId === unpersistedDraftIdRef.current ||
        selectedEventId === unpersistedDraftId ||
        isTempEventId(selectedEventId)
      ) {
        return;
      }

      patchQueue.enqueue(selectedEventId, {
        is_all_day: next.isAllDay,
        start: next.startIso,
        end: next.endIso,
      });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      patchQueue,
      clearWriteError,
    ],
  );

  /** Inspector time-zone change (timed events only). */
  const handleSaveTimeZone = useCallback(
    (timeZone: string, startIso: string, endIso: string) => {
      if (!selectedEventId) return;
      const current = overlaidEvents.find((e) => e.id === selectedEventId);
      if (!current) return;

      const zone = timeZone.trim();
      clearWriteError();

      const painted: CalendarEvent = {
        ...current,
        start_time: startIso,
        end_time: endIso,
        start_time_zone: zone,
        end_time_zone: zone,
      };
      queue.upsert(painted);

      if (
        selectedEventId === unpersistedDraftIdRef.current ||
        selectedEventId === unpersistedDraftId ||
        isTempEventId(selectedEventId)
      ) {
        return;
      }

      // Empty zone = Local: queue keeps '' so it supersedes an earlier pending
      // zone, and the wire request omits timeZone on Google.
      patchQueue.enqueue(selectedEventId, {
        start: startIso,
        end: endIso,
        start_time_zone: zone,
      });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      patchQueue,
      clearWriteError,
    ],
  );

  const handleEmptyClick = useCallback(() => {
    // Desktop empty-cell click: discard an open draft only. Leave a real
    // selected event (and its inspector) alone.
    const hadDraft = unpersistedDraftIdRef.current !== null;
    discardUnpersistedDraft();
    if (hadDraft) {
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
    }
  }, [discardUnpersistedDraft]);

  // Mobile drawer peeks without a backdrop, so the first empty tap closes the
  // selection instead of creating (the next tap with nothing selected creates).
  const closeSelectionOnMobile = () => {
    if (!selectedEventId || window.matchMedia(DESKTOP_MQ).matches) return false;
    closeInspector();
    return true;
  };

  const drag = useCalendarDrag({
    hourH,
    setStripLocked,
    onClickCreate: (slot) => {
      if (closeSelectionOnMobile()) return;
      beginDraft(clickCreateTimesFromSlot(slot));
    },
    onDragCreate: beginDraft,
    onMove: handleMoveOrResize,
    onResize: handleMoveOrResize,
    onAllDayCreate: beginDraft,
    onAllDayTapCreate: (range) => {
      if (closeSelectionOnMobile()) return;
      beginDraft(range);
    },
    onChipTap: selectEvent,
    onEmptyClick: handleEmptyClick,
  });

  onPinchStartRef.current = () => {
    drag.cancel();
    setStripLocked(true);
  };
  onPinchEndRef.current = () => {
    setStripLocked(false);
  };

  // Stable across pointermove — only flips at drag start/end (not every move).
  const draggingEventId =
    drag.isDragging && drag.activeEventId ? drag.activeEventId : null;

  const handleSelectChip = useCallback(
    (eventId: string) => {
      if (drag.suppressNextClick()) return;
      selectEvent(eventId);
    },
    [drag.suppressNextClick, selectEvent],
  );

  const handleAllDayChipPointerDown = useCallback(
    (
      e: ReactPointerEvent<HTMLElement>,
      eventId: string,
      chipEl: HTMLElement,
    ) => {
      const event =
        overlaidEvents.find((ev) => ev.id === eventId) ??
        events.find((ev) => ev.id === eventId);
      if (!event) return;
      drag.onAllDayChipPointerDown(e, event, chipEl);
    },
    [overlaidEvents, events, drag],
  );

  const timedPreviewByDay = useMemo(
    () => timedPreviewSegments(drag.preview, drag.previewZone, days, hourH),
    [drag.preview, drag.previewZone, days, hourH],
  );

  const previewInfo = useMemo<DragPreviewInfo | null>(
    () =>
      drag.preview && drag.previewKind && drag.previewPointerType
        ? {
            range: drag.preview,
            kind: drag.previewKind,
            pointerType: drag.previewPointerType,
          }
        : null,
    [drag.preview, drag.previewKind, drag.previewPointerType],
  );

  const activeDragEvent = drag.activeEventId
    ? overlaidEvents.find((e) => e.id === drag.activeEventId)
    : undefined;
  const previewColor = activeDragEvent
    ? eventChipColor(activeDragEvent)
    : writableCalendar
      ? colorForCalendar(writableCalendar.id)
      : colorForCalendar('preview');
  const previewTitle = activeDragEvent?.title ?? '';

  const allDayPreview = useMemo(() => {
    const idx = allDayPreviewIndices(drag.preview, drag.previewZone, days);
    if (!idx) return null;
    return {
      ...idx,
      color: previewColor,
      title: previewTitle,
      range: drag.preview,
    };
  }, [drag.preview, drag.previewZone, days, previewColor, previewTitle]);

  // Tick "now" so the now-line creeps forward while the page is open.
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 60_000);
    return () => window.clearInterval(id);
  }, []);

  // Position timed events per day column (multi-day events excluded).
  const eventsByDay = useMemo(
    () => buildEventsByDay(days, timedEvents, hourH),
    [days, timedEvents, hourH],
  );

  const todayIndex = useMemo(() => {
    const t = new Date();
    const today = new Date(t.getFullYear(), t.getMonth(), t.getDate());
    const idx = days.findIndex((d) => isSameDay(d, today));
    return idx >= 0 ? idx : null;
  }, [days]);

  const hourLabels = useMemo(() => {
    const step = hourLabelStep(hourH);
    return Array.from({ length: 24 }, (_, h) => h).filter(
      (h) => h % step === 0,
    );
  }, [hourH]);

  const hourGridBg = useMemo(() => hourGridBackground(hourH), [hourH]);

  return {
    // Header / banner
    isRefreshing,
    isLoading,
    error,
    retry,
    eventsEmpty,
    sync,
    healthBanner,
    writeError,
    clearWriteError,

    // Sidebar
    calendars,
    calendarsLoading: calendarsQuery.isLoading,
    calendarsError,
    retryCalendars: () => {
      void calendarsQuery.refetch();
    },
    selectedCalendarIds,
    toggleCalendar,

    // Grid layout / packing
    now,
    hourH,
    totalHoursH,
    hourGridBg,
    hourLabels,
    availableHoursPx,
    allDayHeight,
    allDayChips,
    todayIndex,
    eventsByDay,

    // Selection / drag
    selectedEventId,
    draggingEventId,
    handleSelectChip,
    selectEvent,
    isDragging: drag.isDragging,
    onColumnPointerDown: drag.onColumnPointerDown,
    onChipPointerDown: drag.onChipPointerDown,
    onAllDayPointerDown: drag.onAllDayPointerDown,
    onAllDayChipPointerDown: handleAllDayChipPointerDown,
    timedPreviewByDay,
    previewInfo,
    previewColor,
    previewTitle,
    allDayPreview,

    // Inspector
    selectedEvent,
    selectedEventCalendar,
    focusTitleOnOpen,
    closeInspector,
    handleSaveTitle,
    handleSaveDescription,
    handleSaveTimes,
    handleSaveAllDay,
    handleSaveTimeZone,
    handleSaveCalendar,
    handleDeleteEvent,
    isDeleting: deleteEvent.isPending,
  };
}

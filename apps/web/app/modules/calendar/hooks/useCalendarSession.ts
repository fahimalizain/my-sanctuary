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
} from '@/app/queries/calendar';
import type { CalendarEvent } from '@/app/types';
import { isCalendarWritable } from '../components/EventInspector';
import { useCalendarEventsQueue } from '../events-context';
import {
  clipboardItemFromEvent,
  pasteCreateInput,
  pasteDestinationCalendarId,
  type CalendarClipboardItem,
} from '../lib/calendar-clipboard';
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
import { buildDraftFlushPatch } from '../lib/event-patch-queue';
import {
  DELETE_UNDO_MS,
  commitPendingDelete,
  noteCreateFailed,
  noteCreateSucceeded,
  pendingDeleteTitle,
  requestPendingDelete,
  undoPendingDelete,
  type CommitEffect,
  type PendingDeleteState,
} from '../lib/pending-delete';
import { setPatchWriteErrorListener } from '../lib/patch-write-error';
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
  type StripLockReason,
} from '../lib/week-layout';

export interface CalendarSessionInput {
  timeMin: string;
  timeMax: string;
  days: Date[];
  dayCount: number;
  scrollerHeight: number;
  scrollerRef: RefObject<HTMLElement | null>;
  lockStrip: (reason: StripLockReason) => void;
  unlockStrip: (reason: StripLockReason) => void;
  /** Drag autoscroll: move the day strip sideways by `dx` px. */
  scrollStripBy: (dx: number) => void;
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
  lockStrip,
  unlockStrip,
  scrollStripBy,
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
  // Also carries the app-wide PATCH queue: it lives in the provider so it
  // survives navigation, while this page is only a caller.
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
  // Whether the inspector's calendar picker was used for the current draft.
  // An untouched draft lets the server route to the title's category
  // calendar; an explicit pick must survive create.
  const draftCalendarTouchedRef = useRef(false);
  // Cut sources whose delete has begun. Two quick pastes of one cut must
  // DELETE the row once; later pastes just report "nothing to consume".
  const cutRemovalsStartedRef = useRef<Set<string>>(new Set());

  const createEvent = useCreateCalendarEvent();
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

  // The PATCH queue outlives this page, so its late failures come back
  // through this slot. Unmount clears it: a failure after the page is gone
  // must not setState here (the overlay/cache reconcile still runs).
  useEffect(() => {
    setPatchWriteErrorListener(reportWriteError);
    return () => setPatchWriteErrorListener(null);
  }, [reportWriteError]);

  // Deferred delete: the event leaves the grid now, the DELETE waits out the
  // undo toast. The record also lives in a ref so the timer, Undo and a
  // create POST returning mid-grace all read one synchronous state; the React
  // copy only drives the toast.
  const [pendingDelete, setPendingDelete] = useState<PendingDeleteState>(null);
  const pendingDeleteRef = useRef<PendingDeleteState>(null);
  const deleteTimerRef = useRef<number | null>(null);

  // Mutation and queue identities change on every render / overlay bump, so
  // the stable handlers below read them through refs.
  const queueRef = useRef(queue);
  queueRef.current = queue;
  const deleteEventRef = useRef(deleteEvent);
  deleteEventRef.current = deleteEvent;

  const clearDeleteTimer = useCallback(() => {
    if (deleteTimerRef.current === null) return;
    window.clearTimeout(deleteTimerRef.current);
    deleteTimerRef.current = null;
  }, []);

  /** Apply the machine's commit instruction. Never touches React state. */
  const runCommitEffect = useCallback(
    async (effect: CommitEffect) => {
      if (effect.kind === 'none') return;

      if (effect.kind === 'clear-temp-overlay') {
        // clear (not remove): a create POST still in flight sees the empty
        // overlay and DELETEs the row it just made — that is the commit.
        queueRef.current.clear(effect.id);
        return;
      }

      const id = effect.id;
      // Cancel queued/in-flight PATCHes now, not during the undo window, so a
      // write that lands in the meantime still refreshes the cached row.
      queueRef.current.cancelPatch(id);
      try {
        await deleteEventRef.current.mutateAsync(id);
        removeCalendarEventFromCache(id);
        queueRef.current.clear(id);
      } catch (err) {
        // Clear the delete overlay so the event reappears from the server.
        queueRef.current.clear(id);
        reportWriteError("Couldn't delete event", err);
      }
    },
    [reportWriteError],
  );

  const commitDeleteNow = useCallback(() => {
    clearDeleteTimer();
    const outcome = commitPendingDelete(pendingDeleteRef.current);
    if (outcome.effect.kind === 'none') return;
    pendingDeleteRef.current = outcome.next;
    setPendingDelete(outcome.next);
    void runCommitEffect(outcome.effect);
  }, [clearDeleteTimer, runCommitEffect]);

  const startDeleteTimer = useCallback(() => {
    clearDeleteTimer();
    deleteTimerRef.current = window.setTimeout(commitDeleteNow, DELETE_UNDO_MS);
  }, [clearDeleteTimer, commitDeleteNow]);

  /**
   * Remove a cut source whose paste succeeded. Shared by the paste path and
   * Undo of a parked paste, which owes the removal the create would have
   * done. False means there was nothing left to remove.
   */
  const removeCutSource = useCallback(
    async (cutId: string): Promise<boolean> => {
      if (isTempEventId(cutId)) return false;
      // Second paste of the same cut: the first one already started the
      // delete, so report false rather than claim a second removal.
      if (cutRemovalsStartedRef.current.has(cutId)) return false;
      cutRemovalsStartedRef.current.add(cutId);

      queueRef.current.cancelPatch(cutId);
      queueRef.current.remove(cutId);
      setSelectedEventId((prev) => {
        if (prev !== cutId) return prev;
        setFocusTitleOnOpen(false);
        return null;
      });

      try {
        await deleteEventRef.current.mutateAsync(cutId);
        removeCalendarEventFromCache(cutId);
        queueRef.current.clear(cutId);
      } catch (err) {
        // Clear the delete overlay so the source reappears from the server.
        queueRef.current.clear(cutId);
        cutRemovalsStartedRef.current.delete(cutId);
        reportWriteError("Couldn't remove the cut event", err);
        return false;
      }

      return true;
    },
    [reportWriteError],
  );

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

  // The deferred-delete handler re-binds only with the selection (like the old
  // confirm id), not on every overlay bump; it reads the live event here.
  const selectedEventRef = useRef<CalendarEvent | null>(null);
  selectedEventRef.current = selectedEvent;

  /**
   * Deferred delete for the selection: hide it, close the inspector, and let
   * the toast's Undo window run. Nothing is sent until the timer fires.
   */
  const requestDelete = useCallback(() => {
    const id = selectedEventId;
    const event = selectedEventRef.current;
    if (!id || !event) return;

    clearWriteError();

    const outcome = requestPendingDelete(pendingDeleteRef.current, {
      id,
      event,
      draftId: unpersistedDraftIdRef.current === id ? id : null,
    });
    const record = outcome.next;
    // Same event asked for twice while its toast runs: already hidden.
    if (outcome.hide === 'none' || !record || record.phase !== 'pending') {
      return;
    }

    if (outcome.commitPrevious) {
      // One toast (and one timer) at a time: the previous grace window ends
      // now and sends its DELETE before this one starts.
      pendingDeleteRef.current = outcome.commitPrevious.next;
      setPendingDelete(outcome.commitPrevious.next);
      void runCommitEffect(outcome.commitPrevious.effect);
    }

    pendingDeleteRef.current = record;
    setPendingDelete(record);

    if (outcome.hide === 'overlay-delete') {
      // Hide the chip, but leave queued PATCHes alone until commit: one that
      // lands during grace must still refresh the cached row underneath.
      queueRef.current.remove(id);
    } else if (record.draftId) {
      // Temp chip: paint is suppressed, and its overlay must survive so an
      // in-flight create's success can ghost-delete after a commit. Forget
      // the draft id now so later select / close paths cannot queue.clear it
      // and trigger that DELETE before the toast ends.
      setDraftId(null);
    }

    // Close the inspector without closeInspector(): that discards the open
    // draft, and queue.clear would ghost-delete an in-flight create.
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);

    startDeleteTimer();
  }, [
    selectedEventId,
    clearWriteError,
    runCommitEffect,
    setDraftId,
    startDeleteTimer,
  ]);

  /** Undo the pending delete. Restores frontend state only; nothing was sent. */
  const undoDelete = useCallback(() => {
    const outcome = undoPendingDelete(pendingDeleteRef.current);
    clearDeleteTimer();
    if (outcome.restore.kind === 'none') return;

    const { restore } = outcome;
    if (
      restore.kind === 'clear-delete-overlay' ||
      restore.kind === 'reveal-server'
    ) {
      // Drop only the delete overlay we placed (a PATCH that landed during
      // grace shows through from the cache). unhide-temp needs nothing: its
      // upsert overlay was never removed.
      if (queueRef.current.getOverlay(restore.id)?.op === 'delete') {
        queueRef.current.clear(restore.id);
      }
    }

    pendingDeleteRef.current = outcome.next;
    setPendingDelete(outcome.next);

    if (outcome.draftId) setDraftId(outcome.draftId);
    if (outcome.selectId) setSelectedEventId(outcome.selectId);
    setFocusTitleOnOpen(false);

    // A parked paste's cut source is still owed: the create succeeded, so
    // Undo finishes the removal a normal paste success would have done.
    if (outcome.removeCutSource) void removeCutSource(outcome.removeCutSource);
  }, [clearDeleteTimer, removeCutSource, setDraftId]);

  // A real unmount commits the pending delete: the toast goes with the page,
  // and an overlay delete left in the app-wide provider would hide the event
  // forever with no Undo. Strict mode re-runs the effect, so the microtask
  // must see the ref still false — a remount sets it true synchronously first.
  const mountedRef = useRef(false);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      queueMicrotask(() => {
        if (mountedRef.current) return;
        clearDeleteTimer();
        const current = pendingDeleteRef.current;
        if (!current) return;
        const outcome = commitPendingDelete(current);
        pendingDeleteRef.current = outcome.next;
        void runCommitEffect(outcome.effect);
      });
    };
  }, [clearDeleteTimer, runCommitEffect]);

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
      draftCalendarTouchedRef.current = false;

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

    // If the user renamed / moved / annotated the temp event before POST
    // returned, keep those fields under the server id and flush field PATCH
    // first, then calendar MOVE (cannot combine). Shared by the normal reveal
    // and by a parked row (hidden until Undo or the timer decides).
    const flushDraftEdits = (
      serverEvent: CalendarEvent,
      local: CalendarEvent,
    ) => {
      // The server may have rerouted an untouched draft to the title's
      // category calendar. Trust that unless the user picked a calendar in
      // the inspector; a pick must survive create.
      const calendarId = draftCalendarTouchedRef.current
        ? local.calendar_id
        : serverEvent.calendar_id;
      const flush = buildDraftFlushPatch({
        posted: {
          summary: postedSummary,
          description: postedDescription,
          start: startIso,
          end: endIso,
        },
        local: { ...local, calendar_id: calendarId },
        serverCalendarId: serverEvent.calendar_id,
      });

      if (!flush) return;
      queue.upsert({
        ...serverEvent,
        title: local.title,
        description: local.description,
        start_time: local.start_time,
        end_time: local.end_time,
        is_all_day: local.is_all_day,
        start_time_zone: local.start_time_zone,
        end_time_zone: local.end_time_zone,
        calendar_id: calendarId,
        color: draftCalendarTouchedRef.current
          ? (local.color ?? colorForCalendar(calendarId))
          : serverEvent.color,
      });
      // Same per-event chain as inspector edits: field PATCH first, then
      // calendar MOVE, ahead of any later edit.
      queue.enqueuePatch(serverEvent.id, flush);
    };

    createEvent.mutate(
      {
        calendar_id: event.calendar_id,
        ...(draftCalendarTouchedRef.current
          ? { calendar_id_explicit: true }
          : {}),
        summary: postedSummary,
        ...(postedDescription ? { description: postedDescription } : {}),
        start: startIso,
        end: endIso,
      },
      {
        onSuccess: (result) => {
          draftPersistStartedRef.current.delete(tempId);
          const after = queue.getOverlay(tempId);
          const created = noteCreateSucceeded(pendingDeleteRef.current, {
            tempId,
            serverId: result.event.id,
          });

          // Still inside the undo window: keep the new row hidden behind the
          // toast and park its server id, so the timer or Undo owns it. Do
          // not DELETE yet and do not select the server id.
          if (created.kind === 'park') {
            pendingDeleteRef.current = created.next;
            setPendingDelete(created.next);
            queue.clear(tempId);
            upsertCalendarEventInCache(result.event);
            if (unpersistedDraftIdRef.current === tempId) {
              setDraftId(null);
            }

            // A category-routed create may land on a calendar the sidebar has
            // hidden. Reveal the destination so Undo can actually show the
            // chip, exactly as a normal create success would.
            setSelectedCalendarIds((prev) => {
              if (prev.has(result.event.calendar_id)) return prev;
              const next = new Set(prev);
              next.add(result.event.calendar_id);
              return next;
            });

            if (after?.op === 'upsert') {
              flushDraftEdits(result.event, after.event);
            }
            // Hide last: the toast still offers Undo, so no row may paint
            // between the cache write and the delete overlay.
            queue.remove(result.event.id);
            return;
          }

          // The timer fired while the POST was in flight: the chip is gone,
          // so this create must delete the row it just made (same ghost rule
          // as a discarded draft).
          if (created.kind === 'ghost-delete') {
            pendingDeleteRef.current = created.next;
            setPendingDelete(created.next);
            queue.clear(tempId);
            if (unpersistedDraftIdRef.current === tempId) {
              setDraftId(null);
            }
            void deleteEvent.mutateAsync(result.event.id).then(() => {
              removeCalendarEventFromCache(result.event.id);
            });
            return;
          }

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

          // A category-routed create may land on a calendar the sidebar has
          // hidden. Reveal the destination so the new chip does not vanish.
          setSelectedCalendarIds((prev) => {
            if (prev.has(result.event.calendar_id)) return prev;
            const next = new Set(prev);
            next.add(result.event.calendar_id);
            return next;
          });

          if (after.op === 'upsert') {
            flushDraftEdits(result.event, after.event);
          }
        },
        onError: (err) => {
          draftPersistStartedRef.current.delete(tempId);
          // No server row exists to undo; a pending delete for this draft is
          // stale, so drop it (and its timer) with the overlay.
          const next = noteCreateFailed(pendingDeleteRef.current, tempId);
          if (next !== pendingDeleteRef.current) {
            pendingDeleteRef.current = next;
            setPendingDelete(next);
            clearDeleteTimer();
          }
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
    clearDeleteTimer,
    clearWriteError,
    createEvent,
    deleteEvent,
    queue,
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

      queue.enqueuePatch(selectedEventId, { summary });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
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

      queue.enqueuePatch(selectedEventId, { description });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
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
        // A draft's untouched calendar is the server's default (category
        // routing); a pick here is the user's explicit destination.
        draftCalendarTouchedRef.current = true;
        return;
      }

      queue.enqueuePatch(selectedEventId, { calendar_id: calendarId });
    },
    [
      selectedEventId,
      unpersistedDraftId,
      overlaidEvents,
      queue,
      clearWriteError,
    ],
  );

  // The inspector's overflow Delete, the touch handle and Backspace/Delete all
  // take the deferred path: hide now, send the DELETE when the toast expires.
  const handleDeleteEvent = requestDelete;

  /**
   * Clipboard payload for the selected event, cut-aware.
   *
   * A persisted source stays on the grid: the delete happens on paste, so a
   * cancelled chord never costs the event. A temp / unpersisted draft has no
   * server row, so it is discarded here and the payload is a plain copy.
   */
  const cutForClipboard = useCallback((): CalendarClipboardItem | null => {
    const event = selectedEvent;
    const item = clipboardItemFromEvent(event);
    if (!event || !item) return null;

    // No server row to delete later: drop that one chip now. clear (not
    // remove) keeps the in-flight POST's onSuccess DELETE behaviour — the
    // same discard branch persistDraft uses for a vanished draft. Only this
    // id is touched, so a different open draft survives the cut.
    if (
      isTempEventId(event.id) ||
      event.id === unpersistedDraftIdRef.current ||
      event.id === unpersistedDraftId
    ) {
      queue.cancelPatch(event.id);
      queue.clear(event.id);
      if (
        event.id === unpersistedDraftIdRef.current ||
        event.id === unpersistedDraftId
      ) {
        setDraftId(null);
      }
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
      return item;
    }

    // Read-only calendar: leave the event alone and let the chord through
    // to the browser rather than pretend it was cut.
    if (!isCalendarWritable(selectedEventCalendar)) return null;

    return { ...item, cutSourceId: event.id };
  }, [
    selectedEvent,
    selectedEventCalendar,
    unpersistedDraftId,
    queue,
    setDraftId,
  ]);

  /**
   * Create a second one-shot at the item's own times, then remove the cut
   * source if the create succeeded.
   *
   * The boolean reports whether a cut source was actually deleted, so the
   * page can clear `cutSourceId` on that clipboard item. It does NOT mean the
   * key was consumed — the page already claimed the chord before calling.
   * Plain copies, abandoned pastes, and a cut whose delete already started
   * all return false.
   */
  const pasteFromClipboard = useCallback(
    async (item: CalendarClipboardItem): Promise<boolean> => {
      const destinationId = pasteDestinationCalendarId(
        item.calendarId,
        calendars,
      );
      if (!destinationId) {
        setWriteError('No calendar to paste into');
        return false;
      }

      clearWriteError();

      const tempId = newTempEventId();
      const zone = item.timeZone.trim();
      const optimistic: CalendarEvent = {
        id: tempId,
        calendar_id: destinationId,
        google_event_id: '',
        title: item.summary.trim(),
        description: item.description.trim(),
        start_time: item.start,
        end_time: item.end,
        last_synced_at: new Date().toISOString(),
        // Source color when it had one, else the destination's palette.
        color: item.color || colorForCalendar(destinationId),
        // All-day copies pack into the all-day row; timed copies keep the
        // source zone so the overlay matches what the server stores.
        ...(item.isAllDay
          ? { is_all_day: true }
          : zone
            ? { start_time_zone: zone, end_time_zone: zone }
            : {}),
      };

      // Paint the copy. Never a draft id and never persistDraft: the title is
      // already real, so Escape / closeInspector must not discard it or
      // DELETE the in-flight server row.
      queue.upsert(optimistic);
      setSelectedEventId(tempId);
      setFocusTitleOnOpen(false);

      try {
        const result = await createEvent.mutateAsync(
          pasteCreateInput(item, destinationId),
        );

        const created = noteCreateSucceeded(pendingDeleteRef.current, {
          tempId,
          serverId: result.event.id,
          cutSourceId: item.cutSourceId,
        });

        // The paste's delete toast is still up: park the server id, keep the
        // new row hidden, and leave the cut source alone. Undo owes the
        // source removal; letting the toast expire abandons the paste.
        if (created.kind === 'park') {
          pendingDeleteRef.current = created.next;
          setPendingDelete(created.next);
          queue.clear(tempId);
          upsertCalendarEventInCache(result.event);
          // Undo must be able to show the chip, so reveal the destination
          // even when the sidebar had that calendar checked off.
          setSelectedCalendarIds((prev) => {
            if (prev.has(result.event.calendar_id)) return prev;
            const next = new Set(prev);
            next.add(result.event.calendar_id);
            return next;
          });
          queue.remove(result.event.id);
          return false;
        }

        // Grace expired while the POST was in flight: the chip is gone, so
        // this create must remove the row it just made (same ghost rule as a
        // discarded paste).
        if (created.kind === 'ghost-delete') {
          pendingDeleteRef.current = created.next;
          setPendingDelete(created.next);
          queue.clear(tempId);
          setSelectedEventId((prev) => {
            if (prev !== tempId) return prev;
            setFocusTitleOnOpen(false);
            return null;
          });
          void deleteEvent.mutateAsync(result.event.id).then(() => {
            removeCalendarEventFromCache(result.event.id);
          });
          return false;
        }

        // User cut / deleted / discarded the chip while the POST was in
        // flight — do not cache-write; DELETE the just-created server row so
        // Google has no ghost. Same discard branch as persistDraft.
        const after = queue.getOverlay(tempId);
        if (!after || after.op === 'delete') {
          queue.clear(tempId);
          setSelectedEventId((prev) => {
            if (prev !== tempId) return prev;
            setFocusTitleOnOpen(false);
            return null;
          });
          void deleteEvent.mutateAsync(result.event.id).then(() => {
            removeCalendarEventFromCache(result.event.id);
          });
          // Abandoned paste: item.cutSourceId is untouched, so a retry can
          // still remove the cut source.
          return false;
        }

        // Remap selection before clearing the overlay so selectedEvent never
        // resolves null for the temp id (the effect would close the inspector).
        setSelectedEventId((prev) =>
          prev === tempId ? result.event.id : prev,
        );
        queue.clear(tempId);
        upsertCalendarEventInCache(result.event);
      } catch (err) {
        // The create failed, so a pending delete for this temp id is stale.
        const next = noteCreateFailed(pendingDeleteRef.current, tempId);
        if (next !== pendingDeleteRef.current) {
          pendingDeleteRef.current = next;
          setPendingDelete(next);
          clearDeleteTimer();
        }
        queue.clear(tempId);
        setSelectedEventId((prev) => {
          if (prev !== tempId) return prev;
          setFocusTitleOnOpen(false);
          return null;
        });
        reportWriteError("Couldn't create event", err);
        // The cut source was never touched, so a failed paste costs nothing.
        return false;
      }

      const cutId = item.cutSourceId;
      // Not a cut: no source was deleted, so the page has nothing to clear.
      if (!cutId) return false;
      // A temp source has no server row and a second paste of the same cut
      // must not claim another removal; the helper reports both as false.
      return removeCutSource(cutId);
    },
    [
      calendars,
      clearDeleteTimer,
      clearWriteError,
      createEvent,
      deleteEvent,
      queue,
      removeCutSource,
      reportWriteError,
    ],
  );

  /**
   * Cmd/Ctrl+D: clone the selection through the paste path without touching
   * the clipboard. True means the key was ours even if the clone later fails.
   */
  const duplicateSelected = useCallback((): boolean => {
    const item = clipboardItemFromEvent(selectedEvent);
    if (!item) return false;
    // A duplicate is never a cut: the source stays where it is.
    void pasteFromClipboard({ ...item, cutSourceId: null });
    return true;
  }, [selectedEvent, pasteFromClipboard]);

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

  // A temp delete is hidden by suppressing paint, never by an overlay delete:
  // its upsert must survive so an in-flight create's onSuccess can see it.
  // Once a create parks a server id the delete overlay takes over.
  const hiddenTempId =
    pendingDelete?.phase === 'pending' && pendingDelete.serverId === null
      ? pendingDelete.hiddenId
      : null;

  // Overlay first, then calendar-visibility filter.
  const visibleEvents = useMemo(() => {
    return overlaidEvents.filter((e) => {
      if (e.id === hiddenTempId) return false;
      if (!selectionReady) return true;
      if (selectedCalendarIds.has(e.calendar_id)) return true;
      if (!knownCalendarIds.has(e.calendar_id)) return true;
      return false;
    });
  }, [
    overlaidEvents,
    hiddenTempId,
    selectedCalendarIds,
    knownCalendarIds,
    selectionReady,
  ]);

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

      queue.enqueuePatch(eventId, {
        start: next.start_time,
        end: next.end_time,
      });
    },
    [overlaidEvents, events, queue, clearWriteError],
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

      queue.enqueuePatch(selectedEventId, {
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
      queue.enqueuePatch(selectedEventId, {
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
    lockStrip,
    unlockStrip,
    scrollStripBy,
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
    lockStrip('pinch');
  };
  onPinchEndRef.current = () => {
    unlockStrip('pinch');
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

  /**
   * Grab a gutter edge pill for the selected event. All-day and multi-day
   * events have no pills on the 24h axis, so they never start a scrub.
   */
  const handleGutterEdgePointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>, edge: 'start' | 'end') => {
      const event = selectedEvent;
      if (!event || event.is_all_day) return;
      const start = new Date(event.start_time);
      const end = new Date(event.end_time);
      if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return;
      if (isMultiDay(start, end)) return;
      drag.onGutterEdgePointerDown(e, edge, event);
    },
    [selectedEvent, drag],
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

  /**
   * Times the gutter shows: an active timed drag (create / move / resize)
   * replaces the selection's committed times, so the pills track the ghost
   * instead of the chip it left behind. All-day and multi-day events live in
   * the all-day band and have no place on the 24h axis.
   */
  const gutterEdges = useMemo<{ start: Date; end: Date } | null>(() => {
    if (drag.preview && drag.previewZone === 'timed') return drag.preview;
    if (!selectedEvent || selectedEvent.is_all_day) return null;
    const start = new Date(selectedEvent.start_time);
    const end = new Date(selectedEvent.end_time);
    if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) {
      return null;
    }
    if (isMultiDay(start, end)) return null;
    return { start, end };
  }, [drag.preview, drag.previewZone, selectedEvent]);

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
    beginDraft,
    isDragging: drag.isDragging,
    precisionSnap: drag.precisionSnap,
    onColumnPointerDown: drag.onColumnPointerDown,
    onChipPointerDown: drag.onChipPointerDown,
    onAllDayPointerDown: drag.onAllDayPointerDown,
    onAllDayChipPointerDown: handleAllDayChipPointerDown,
    onGutterEdgePointerDown: handleGutterEdgePointerDown,
    timedPreviewByDay,
    previewInfo,
    gutterEdges,
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

    // Deferred delete / undo toast. The title is null when no toast is owed;
    // a blank title is '' and still renders ("untitled event").
    pendingDeleteTitle: pendingDeleteTitle(pendingDelete),
    undoDelete,

    // In-memory clipboard (Cmd/Ctrl+C / X / V / D)
    cutForClipboard,
    pasteFromClipboard,
    duplicateSelected,
  };
}

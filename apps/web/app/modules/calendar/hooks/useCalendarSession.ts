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
  useCalendarEventsQuery,
  useCalendarsQuery,
  useCreateCalendarEvent,
  useDeleteCalendarEvent,
} from '@/app/queries/calendar';
import type { CalendarEvent, NewCalendarEventInput } from '@/app/types';
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
import {
  buildCreateFlushPatch,
  postedFieldsFromCreate,
  type PostedEventFields,
} from '../lib/event-patch-queue';
import {
  DELETE_UNDO_MS,
  commitPendingDelete,
  noteCreateSucceeded,
  pendingDeleteTitle,
  requestPendingDelete,
  undoPendingDelete,
  type CommitEffect,
  type PendingDeleteState,
} from '../lib/pending-delete';
import { setPatchWriteErrorListener } from '../lib/patch-write-error';
import {
  canRetryUnsavedCreate,
  createRetryInput,
  retryableUnsavedIds,
} from '../lib/unsaved-creates';
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
 * What a settled create did with the row it made. `parked` keeps it hidden
 * behind the undo toast, `discarded` DELETEs it (committed delete or an
 * emptied overlay), `revealed` is the normal case where the chip keeps it.
 */
type CreateSettlement = 'revealed' | 'parked' | 'discarded';

/** The quiet write-error banner. */
interface WriteErrorState {
  message: string;
  /** The failure was a create POST, so its unsaved event can still be retried. */
  retryCreates: boolean;
  /** The failure was a PATCH, so its kept intent can be re-sent. */
  retryPatches: boolean;
}

/** Banner line for a failed write: `base: detail` when the error has one. */
function describeWriteError(base: string, err: unknown): string {
  if (err instanceof Error && err.message.trim()) {
    return `${base}: ${err.message}`;
  }
  return base;
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

  // The app-wide local change layer (read-time paint over the cache; never a
  // setQueryData write). Also carries the PATCH queue and the local create /
  // retry lifecycle: it lives in the provider so it survives navigation, while
  // this page is only a caller.
  const queue = useCalendarEventsQueue();
  // Ids painted as local-only events whose create failed: the grid marks them
  // and the inspector offers "Not saved — Retry". Provider-owned, so the
  // marker survives navigation.
  const unsavedEventIds = queue.unsavedEventIds;
  // A create repaints its temp id as the server id when the POST returns, and
  // anything that started before that (a drag session, a queued inspector
  // blur, a stale selection) still holds the temp id. Every id-taking handler
  // below resolves through the provider's alias, so those writes land on the
  // row instead of silently addressing an id that no longer exists.
  const resolveId = queue.resolveId;

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
  // The open draft (a temp row whose title blur has not POSTed yet), the
  // per-create calendar picks and the unsaved-create records all live in the
  // provider now: they describe local-only events, and navigation must not
  // drop what the user typed. `queue.getDraftId()` is the live read.
  // Cut sources whose delete has begun. Two quick pastes of one cut must
  // DELETE the row once; later pastes just report "nothing to consume".
  const cutRemovalsStartedRef = useRef<Set<string>>(new Set());

  const createEvent = useCreateCalendarEvent();
  const deleteEvent = useDeleteCalendarEvent();

  // Quiet notice for failed writes (distinct from sync health banner). A
  // create failure also owns a retry action: its event is still on the grid.
  const [writeError, setWriteError] = useState<WriteErrorState | null>(null);
  const clearWriteError = useCallback(() => setWriteError(null), []);

  const reportWriteError = useCallback((base: string, err: unknown) => {
    setWriteError({
      message: describeWriteError(base, err),
      retryCreates: false,
      retryPatches: false,
    });
  }, []);

  // A PATCH that failed keeps its intent (there is something to retry), so the
  // banner offers Retry even though nothing is unsaved.
  const reportPatchError = useCallback((base: string, err: unknown) => {
    setWriteError({
      message: describeWriteError(base, err),
      retryCreates: false,
      retryPatches: true,
    });
  }, []);

  // The PATCH queue outlives this page, so its late failures come back
  // through this slot. Unmount clears it: a failure after the page is gone
  // must not setState here (the layer keeps the edit and the retry state).
  useEffect(() => {
    setPatchWriteErrorListener(reportPatchError);
    return () => setPatchWriteErrorListener(null);
  }, [reportPatchError]);

  // Deferred delete: the event leaves the grid now, the DELETE waits out the
  // undo toast. The record also lives in a ref so the timer, Undo and a
  // create POST returning mid-grace all read one synchronous state; the React
  // copy only drives the toast.
  const [pendingDelete, setPendingDelete] = useState<PendingDeleteState>(null);
  const pendingDeleteRef = useRef<PendingDeleteState>(null);
  const deleteTimerRef = useRef<number | null>(null);

  // Mutation and queue identities change on every render / layer bump, so
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
        // A temp id with no server row has nothing left to retry: forgetting
        // the record would otherwise keep a phantom "unsaved" event alive.
        queueRef.current.forgetUnsavedCreate(effect.id);
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
   * done. False means there was nothing left to remove (an id that is still
   * a create in flight included: it has no row to delete yet).
   */
  const removeCutSource = useCallback(
    async (cutId: string): Promise<boolean> => {
      // A cut may have been taken while the source's create POST was still in
      // flight. If the POST has returned since, the alias names its row.
      const id = resolveId(cutId);
      if (isTempEventId(id)) return false;
      // Second paste of the same cut: the first one already started the
      // delete, so report false rather than claim a second removal.
      if (cutRemovalsStartedRef.current.has(id)) return false;
      cutRemovalsStartedRef.current.add(id);

      queueRef.current.cancelPatch(id);
      queueRef.current.remove(id);
      setSelectedEventId((prev) => {
        if (prev !== id) return prev;
        setFocusTitleOnOpen(false);
        return null;
      });

      try {
        await deleteEventRef.current.mutateAsync(id);
        removeCalendarEventFromCache(id);
        queueRef.current.clear(id);
      } catch (err) {
        // Clear the delete overlay so the source reappears from the server.
        queueRef.current.clear(id);
        cutRemovalsStartedRef.current.delete(id);
        reportWriteError("Couldn't remove the cut event", err);
        return false;
      }

      return true;
    },
    [reportWriteError, resolveId],
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
    // A create that returned after the selection was made leaves a temp id in
    // state for one render; the alias keeps the inspector on the row instead
    // of resolving null and closing it.
    const id = resolveId(selectedEventId);
    return overlaidEvents.find((e) => e.id === id) ?? null;
  }, [overlaidEvents, selectedEventId, resolveId]);

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
  // confirm id), not on every layer bump; it reads the live event here.
  const selectedEventRef = useRef<CalendarEvent | null>(null);
  selectedEventRef.current = selectedEvent;

  /**
   * Deferred delete for the selection: hide it, close the inspector, and let
   * the toast's Undo window run. Nothing is sent until the timer fires.
   */
  const requestDelete = useCallback(() => {
    const selectedId = selectedEventId;
    const event = selectedEventRef.current;
    if (!selectedId || !event) return;
    // The selection may still carry a temp id for one render after its create
    // returned; the deferred delete must hide and DELETE the server row.
    const id = resolveId(selectedId);

    clearWriteError();

    const outcome = requestPendingDelete(pendingDeleteRef.current, {
      id,
      event,
      draftId: queueRef.current.getDraftId() === id ? id : null,
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
      // in-flight create's success can ghost-delete after a commit. Release
      // the draft id now so later select / close paths cannot queue.clear it
      // and trigger that DELETE before the toast ends.
      queueRef.current.releaseDraft(id);
    }

    // Close the inspector without closeInspector(): that discards the open
    // draft, and queue.clear would ghost-delete an in-flight create.
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);

    startDeleteTimer();
  }, [
    selectedEventId,
    resolveId,
    clearWriteError,
    runCommitEffect,
    startDeleteTimer,
  ]);

  /**
   * Undo the pending delete. Restores frontend state only — the deleted event
   * itself was never sent. Resolves with the cut source Undo actually removed
   * (a parked paste), so the page can clear that cut from the clipboard the
   * same way a normal paste does; null when no cut was removed.
   */
  const undoDelete = useCallback(async (): Promise<string | null> => {
    const outcome = undoPendingDelete(pendingDeleteRef.current);
    clearDeleteTimer();
    if (outcome.restore.kind === 'none') return null;

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

    if (outcome.draftId) queueRef.current.markDraft(outcome.draftId);
    if (outcome.selectId) setSelectedEventId(outcome.selectId);
    setFocusTitleOnOpen(false);

    // A parked paste's cut source is still owed: the create succeeded, so
    // Undo finishes the removal a normal paste success would have done. Only
    // a real removal reports the id; a failure leaves it retryable.
    const cutId = outcome.removeCutSource;
    if (!cutId) return null;
    try {
      return (await removeCutSource(cutId)) ? cutId : null;
    } catch {
      return null;
    }
  }, [clearDeleteTimer, removeCutSource]);

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
    const id = queue.getDraftId();
    if (!id) return;
    // clear (not remove): the chip was never on the server. A draft whose
    // create POST is already sent has released its id, so this can only drop
    // a pre-POST overlay — never DELETE a row that was just created. Clearing
    // the entry also drops the pick and any unsaved record for it.
    queue.clear(id);
  }, [queue]);

  const closeInspector = useCallback(() => {
    discardUnpersistedDraft();
    setSelectedEventId(null);
    setFocusTitleOnOpen(false);
  }, [discardUnpersistedDraft]);

  const selectEvent = useCallback(
    (eventId: string) => {
      // A chip tap that ends a drag holds the id the session started under;
      // after a create returned that is the temp id, so resolve before it
      // lands in state (and before the draft-id compare).
      const id = resolveId(eventId);
      const draftId = queue.getDraftId();
      if (draftId && draftId !== id) {
        discardUnpersistedDraft();
      }
      setSelectedEventId(id);
      setFocusTitleOnOpen(false);
    },
    [discardUnpersistedDraft, resolveId, queue],
  );

  const beginDraft = useCallback(
    (range: TimedRange) => {
      if (!writableCalendar) {
        closeInspector();
        return;
      }

      // Replace any previous local draft (no POST was made for it).
      const prevDraftId = queue.getDraftId();
      if (prevDraftId) {
        queue.clear(prevDraftId);
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
      queue.markDraft(tempId);
      setSelectedEventId(tempId);
      setFocusTitleOnOpen(true);
    },
    [writableCalendar, closeInspector, queue],
  );

  /**
   * Keep the fields the user changed on a temp-id event while its create POST
   * was in flight: paint them under the server id and PATCH them through the
   * same per-event chain inspector edits use (fields first, the exclusive
   * calendar move second). Shared by drafts and pastes, so a duplicate is
   * flushed exactly like a draft and its chip never snaps back to the posted
   * row.
   */
  const flushCreateEdits = useCallback(
    (args: {
      serverEvent: CalendarEvent;
      local: CalendarEvent;
      /** Body the create POST carried: the baseline to diff against. */
      posted: PostedEventFields;
      /** Inspector calendar pick made before or during the POST. */
      calendarPicked: boolean;
    }) => {
      const { serverEvent, local, posted, calendarPicked } = args;
      // The server may have rerouted an untouched draft to the title's
      // category calendar. Trust that unless the user picked a calendar in
      // the inspector; a pick must survive create.
      const calendarId = calendarPicked
        ? local.calendar_id
        : serverEvent.calendar_id;
      const flush = buildCreateFlushPatch({
        posted,
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
        color: calendarPicked
          ? (local.color ?? colorForCalendar(calendarId))
          : serverEvent.color,
      });
      // Same per-event chain as inspector edits: field PATCH first, then
      // calendar MOVE, ahead of any later edit.
      queue.enqueuePatch(serverEvent.id, flush);
    },
    [queue],
  );

  /** A category-routed create may land on a calendar the sidebar has hidden. */
  const revealCreatedCalendar = useCallback((event: CalendarEvent) => {
    setSelectedCalendarIds((prev) => {
      if (prev.has(event.calendar_id)) return prev;
      const next = new Set(prev);
      next.add(event.calendar_id);
      return next;
    });
  }, []);

  /** Drop the selection when it still points at this (gone) id. */
  const dropSelectionIf = useCallback((id: string) => {
    setSelectedEventId((prev) => {
      if (prev !== id) return prev;
      setFocusTitleOnOpen(false);
      return null;
    });
  }, []);

  /**
   * One create POST returned, draft or paste alike. Decides what happens to
   * the row it made, then — for a kept row — records the temp → server alias
   * and flushes edits made while the POST was in flight.
   */
  const finishCreateSuccess = useCallback(
    (input: {
      tempId: string;
      serverEvent: CalendarEvent;
      /** Body the POST carried: what the server already stored. */
      posted: PostedEventFields;
      /** Cut source a paste would remove on success; may be a temp id. */
      cutSourceId?: string | null;
    }): CreateSettlement => {
      const { tempId, serverEvent, posted } = input;

      // Read the pick before the temp row settles: one made while the POST was
      // in flight must survive create. Keyed by temp id, so a newer draft's
      // pick cannot leak into this create's flush.
      const calendarPicked = queue.isCalendarPicked(tempId);
      // The row exists now (or is being ghost-deleted): either way this id is
      // settled, so no unsaved marker and no retry may outlive it.
      queue.forgetUnsavedCreate(tempId);
      // A create-failure banner was about this POST (a first attempt or a
      // retry); a landed create clears it. A PATCH error is left alone.
      setWriteError((prev) => (prev?.retryCreates ? null : prev));

      // A cut may name a row whose own create returned since the chord; an
      // unresolved temp source stays temp and is never deleted.
      const cutSourceId = input.cutSourceId
        ? resolveId(input.cutSourceId)
        : null;
      const after = queue.getOverlay(tempId);
      const created = noteCreateSucceeded(pendingDeleteRef.current, {
        tempId,
        serverId: serverEvent.id,
        cutSourceId,
      });

      // Still inside the undo window: keep the new row hidden behind the
      // toast and park its server id, so the timer or Undo owns it. Do not
      // DELETE yet and do not select the server id.
      if (created.kind === 'park') {
        pendingDeleteRef.current = created.next;
        setPendingDelete(created.next);
        // Alias + cache + drop the temp row before any paint: a drag session
        // or a queued blur still holds the temp id and must address this row.
        queue.settleCreate({ tempId, serverEvent });

        // Reveal the destination so Undo can actually show the chip, exactly
        // as a normal create success would.
        revealCreatedCalendar(serverEvent);

        if (after?.op === 'upsert') {
          flushCreateEdits({
            serverEvent,
            local: after.event,
            posted,
            calendarPicked,
          });
        }
        // Hide last: the toast still offers Undo, so no row may paint
        // between the cache write and the delete overlay.
        queue.remove(serverEvent.id);
        return 'parked';
      }

      // The timer fired while the POST was in flight: the chip is gone, so
      // this create must delete the row it just made (same ghost rule as a
      // discarded draft).
      if (created.kind === 'ghost-delete') {
        pendingDeleteRef.current = created.next;
        setPendingDelete(created.next);
        queue.clear(tempId);
        dropSelectionIf(tempId);
        // No alias: the row is being deleted, so a stale temp id must not
        // start PATCHing it.
        void deleteEvent.mutateAsync(serverEvent.id).then(() => {
          removeCalendarEventFromCache(serverEvent.id);
        });
        return 'discarded';
      }

      // Defensive: an emptied overlay means the chip was removed while the
      // POST was in flight (now only a committed temp delete does that, and
      // noteCreateSucceeded catches it first). Do not cache-write; DELETE the
      // just-created server event so Google has no ghost.
      if (!after || after.op === 'delete') {
        queue.clear(tempId);
        dropSelectionIf(tempId);
        void deleteEvent.mutateAsync(serverEvent.id).then(() => {
          removeCalendarEventFromCache(serverEvent.id);
        });
        return 'discarded';
      }

      // Remap selection before the temp row settles so selectedEvent never
      // resolves null for the old temp id (the effect would close it).
      setSelectedEventId((prev) => (prev === tempId ? serverEvent.id : prev));
      // Alias + cache + drop the temp row before any paint.
      queue.settleCreate({ tempId, serverEvent });

      // Reveal the destination so the new chip does not vanish.
      revealCreatedCalendar(serverEvent);

      flushCreateEdits({
        serverEvent,
        local: after.event,
        posted,
        calendarPicked,
      });
      return 'revealed';
    },
    [
      deleteEvent,
      dropSelectionIf,
      flushCreateEdits,
      queue,
      resolveId,
      revealCreatedCalendar,
    ],
  );

  /**
   * A create POST failed. There is no server row to fall back on, so the temp
   * chip keeps painting exactly what the user left there — title, description,
   * times, all-day / zone and calendar, including edits made while the POST
   * was in flight — and becomes a retryable unsaved event.
   *
   * Nothing is cleared and nothing is deselected: a failure must never cost
   * the event. A "delete" already in its undo window keeps owning the paint
   * (Undo brings the chip back as unsaved); a delete that already committed
   * cleared the overlay, so there is nothing left to keep.
   */
  const finishCreateFailure = useCallback(
    (tempId: string, err: unknown, input: NewCalendarEventInput) => {
      const overlay = queue.getOverlay(tempId);
      if (overlay?.op === 'upsert') {
        // Keep the paint and the calendar pick: the retry body is built from
        // this overlay, so every field the user sees is what gets POSTed.
        queue.markUnsavedCreate({ tempId, input });
      } else {
        queue.forgetUnsavedCreate(tempId);
      }
      setWriteError({
        message: describeWriteError("Couldn't create event", err),
        retryCreates: true,
        retryPatches: false,
      });
    },
    [queue],
  );

  /**
   * Re-POST one unsaved event. The body is its current local state, so edits
   * made while it was unsaved travel with the retry, and the settlement is the
   * same one a draft / paste create uses (alias, cache write, overlay flush of
   * anything typed during this POST).
   */
  const retryUnsavedCreate = useCallback(
    async (tempId: string): Promise<boolean> => {
      const record = queue.getUnsavedCreate(tempId);
      if (!record) return false;

      const hidden = pendingDeleteRef.current;
      const gate = {
        hiddenId: hidden?.phase === 'pending' ? hidden.hiddenId : null,
        inFlight: queue.retryingCreateIds,
      };
      if (!canRetryUnsavedCreate(tempId, gate)) return false;
      // Atomic claim: the snapshot above may be one render old, and two
      // gestures may reach this in one tick.
      if (!queue.beginCreateRetry(tempId)) return false;

      try {
        // The chip may have been cut / deleted since the failure.
        const overlay = queue.getOverlay(tempId);
        if (!overlay || overlay.op !== 'upsert') {
          queue.forgetUnsavedCreate(tempId);
          return false;
        }

        const input = createRetryInput({
          failed: record.input,
          local: overlay.event,
          calendarPicked: queue.isCalendarPicked(tempId),
        });
        if (!input) return false;

        try {
          const result = await createEvent.mutateAsync(input);
          finishCreateSuccess({
            tempId,
            serverEvent: result.event,
            posted: postedFieldsFromCreate(input),
            // A retried paste deliberately carries no cut source: the
            // clipboard still holds that cut, and deleting the source here
            // would consume something the user never pasted again (the page
            // could not even clear the item). The source stays until a fresh
            // paste.
          });
          return true;
        } catch (err) {
          // Still unsaved: keep the paint (now with this attempt's body flags).
          finishCreateFailure(tempId, err, input);
          return false;
        }
      } finally {
        queue.endCreateRetry(tempId);
      }
    },
    [createEvent, finishCreateFailure, finishCreateSuccess, queue],
  );

  /**
   * A save landed on a temp id. An unsaved create re-POSTs the event as it
   * stands now; a create still in flight keeps its overlay-only edit and lets
   * its onSuccess flush. Non-temp ids are not ours: false lets the caller keep
   * its overlay-only branch.
   */
  const retryIfUnsaved = useCallback(
    (id: string): boolean => {
      if (!queue.getUnsavedCreate(id)) return false;
      void retryUnsavedCreate(id);
      return true;
    },
    [queue, retryUnsavedCreate],
  );

  /** Banner Retry: re-POST every unsaved event the undo toast is not hiding. */
  const retryUnsavedCreates = useCallback(() => {
    const hidden = pendingDeleteRef.current;
    const ids = retryableUnsavedIds(Array.from(queue.unsavedEventIds), {
      hiddenId: hidden?.phase === 'pending' ? hidden.hiddenId : null,
      inFlight: queue.retryingCreateIds,
    });
    void (async () => {
      for (const id of ids) {
        await retryUnsavedCreate(id);
      }
    })();
  }, [queue, retryUnsavedCreate]);

  /**
   * Banner Retry: save everything that is still only local — the kept intent of
   * a failed PATCH and every unsaved create. Creating first is deliberate: a
   * create failure is the louder loss.
   */
  const retryWrites = useCallback(() => {
    clearWriteError();
    queue.retryFailedPatches();
    retryUnsavedCreates();
  }, [clearWriteError, queue, retryUnsavedCreates]);

  const persistDraft = useCallback(() => {
    const tempId = queue.getDraftId();
    if (!tempId) return;

    const latest = queue.getOverlay(tempId);
    if (!latest || latest.op !== 'upsert') return;
    if (!isPersistableDraftTitle(latest.event.title)) return;

    const event = latest.event;
    const description = event.description ?? '';
    // A pick made before the send is the explicit destination this POST
    // carries; one made while it flies is a local edit the success flush moves.
    const calendarPickedAtSend = queue.isCalendarPicked(tempId);

    clearWriteError();

    // One body, used both as the request and as the success-flush baseline:
    // the PATCH made on success diffs against what the server received.
    const input: NewCalendarEventInput = {
      calendar_id: event.calendar_id,
      ...(calendarPickedAtSend ? { calendar_id_explicit: true } : {}),
      summary: event.title.trim(),
      ...(description ? { description } : {}),
      start: event.start_time,
      end: event.end_time,
    };

    // The POST is about to be sent: the event is real from here on, so release
    // the draft marker. Every discard path (empty click, Escape, another chip,
    // a new draft) keys on it and can no longer empty this overlay — which
    // would turn create onSuccess into a ghost DELETE of the row it just made.
    // Later edits to this temp id are overlay-only and flushed on success.
    queue.releaseDraft(tempId);

    createEvent.mutate(input, {
      onSuccess: (result) => {
        finishCreateSuccess({
          tempId,
          serverEvent: result.event,
          posted: postedFieldsFromCreate(input),
        });
      },
      onError: (err) => finishCreateFailure(tempId, err, input),
    });
  }, [
    clearWriteError,
    createEvent,
    finishCreateFailure,
    finishCreateSuccess,
    queue,
  ]);

  const handleSaveTitle = useCallback(
    (summary: string) => {
      if (!selectedEventId) return;
      // A create that returned between the render and this blur leaves a temp
      // id in the selection; the alias names the row the write must reach.
      const id = resolveId(selectedEventId);
      const current = overlaidEvents.find((e) => e.id === id);
      if (!current) return;

      clearWriteError();

      // Paint immediately.
      queue.upsert({ ...current, title: summary });

      // Unpersisted draft: title blur is the sole persist trigger.
      if (id === queue.getDraftId()) {
        persistDraft();
        return;
      }

      // Still a temp id: an unsaved create re-POSTs with this edit; a create
      // still in flight keeps the overlay-only edit and flushes on success.
      // Never a PATCH against a temp id.
      if (isTempEventId(id)) {
        retryIfUnsaved(id);
        return;
      }

      queue.enqueuePatch(id, { summary });
    },
    [
      selectedEventId,
      resolveId,
      overlaidEvents,
      queue,
      persistDraft,
      retryIfUnsaved,
      clearWriteError,
    ],
  );

  const handleSaveDescription = useCallback(
    (description: string) => {
      if (!selectedEventId) return;
      const id = resolveId(selectedEventId);
      const current = overlaidEvents.find((e) => e.id === id);
      if (!current) return;

      clearWriteError();

      // Paint immediately.
      queue.upsert({ ...current, description });

      // Unpersisted draft: paint overlay and try persist. Create stays gated
      // by isPersistableDraftTitle — description-only empty-title drafts do
      // not POST. Title blur remains the primary persist trigger.
      if (id === queue.getDraftId()) {
        persistDraft();
        return;
      }

      // Still a temp id: an unsaved create re-POSTs with this edit; a create
      // still in flight keeps the overlay-only edit and flushes on success.
      if (isTempEventId(id)) {
        retryIfUnsaved(id);
        return;
      }

      queue.enqueuePatch(id, { description });
    },
    [
      selectedEventId,
      resolveId,
      overlaidEvents,
      queue,
      persistDraft,
      retryIfUnsaved,
      clearWriteError,
    ],
  );

  const handleSaveCalendar = useCallback(
    (calendarId: string) => {
      if (!selectedEventId) return;
      const id = resolveId(selectedEventId);
      const current = overlaidEvents.find((e) => e.id === id);
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

      // Unpersisted draft, unsaved create or temp id: overlay only. A pick made
      // while the create flies is flushed as a MOVE after any field PATCH; a
      // pick on an unsaved create becomes the retry POST's explicit destination.
      if (id === queue.getDraftId() || isTempEventId(id)) {
        // A draft's untouched calendar is the server's default (category
        // routing); a pick here is the user's explicit destination. Keyed by
        // temp id so it survives until the create settles (and a concurrent
        // draft's pick never reaches this create's flush).
        queue.markCalendarPicked(id);
        if (isTempEventId(id)) retryIfUnsaved(id);
        return;
      }

      queue.enqueuePatch(id, { calendar_id: calendarId });
    },
    [
      selectedEventId,
      resolveId,
      overlaidEvents,
      queue,
      retryIfUnsaved,
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
   * cancelled chord never costs the event. An open unpersisted draft has no
   * server row, so it is discarded here and the payload is a plain copy. An
   * unsaved create is local-only in the same way — a cut takes it off the grid
   * (nothing is sent, now or on paste).
   *
   * A temp id whose create is still in flight is a real event as well. It
   * keeps its chip, and the cut is settled on paste: the alias names the
   * server row if the create has returned by then, and the paste reports
   * nothing removed if it has not (nothing is lost — the copy still lands).
   */
  const cutForClipboard = useCallback((): CalendarClipboardItem | null => {
    const event = selectedEvent;
    const item = clipboardItemFromEvent(event);
    if (!event || !item) return null;

    // The selection may still carry the temp id for one render after its
    // create returned; the cut must name the row it became.
    const id = resolveId(event.id);

    const isOpenDraft = id === queue.getDraftId();

    // No server row to delete later: drop that one chip now. clear (not
    // remove) is enough for a pre-POST overlay. Only this id is touched, so a
    // different open draft survives the cut.
    if (isOpenDraft) {
      queue.cancelPatch(id);
      queue.clear(id);
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
      return item;
    }

    // Unpersisted by failure, not by draft: the chip is local-only too, so the
    // cut takes it (and its retry record) off the grid. There is no POST in
    // flight that a cleared overlay could turn into a ghost DELETE.
    if (queue.getUnsavedCreate(id)) {
      queue.cancelPatch(id);
      queue.clear(id);
      queue.forgetUnsavedCreate(id);
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
      return item;
    }

    // Read-only calendar: leave the event alone and let the chord through
    // to the browser rather than pretend it was cut.
    if (!isCalendarWritable(selectedEventCalendar)) return null;

    // Still a temp id (create in flight): the source has no row to delete
    // yet, so it keeps its chip and the paste resolves the cut later. Never
    // clear the overlay here — create onSuccess reads an emptied overlay as a
    // discard and DELETEs the row the user never pasted.
    return { ...item, cutSourceId: id };
  }, [selectedEvent, selectedEventCalendar, resolveId, queue]);

  /**
   * Create a second one-shot at the item's own times, then remove the cut
   * source if the create succeeded.
   *
   * The boolean reports whether a cut source was actually deleted, so the
   * page can clear `cutSourceId` on that clipboard item. It does NOT mean the
   * key was consumed — the page already claimed the chord before calling.
   * Plain copies, abandoned pastes, a cut whose delete already started, and
   * a cut whose source create is still in flight all return false.
   */
  const pasteFromClipboard = useCallback(
    async (item: CalendarClipboardItem): Promise<boolean> => {
      const destinationId = pasteDestinationCalendarId(
        item.calendarId,
        calendars,
      );
      if (!destinationId) {
        setWriteError({
          message: 'No calendar to paste into',
          retryCreates: false,
          retryPatches: false,
        });
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

      // One body: the request, and the baseline the success flush diffs
      // against. A paste may POST all-day / a zone, so the baseline cannot
      // assume the draft's timed, zoneless create.
      const input = pasteCreateInput(item, destinationId);

      let settlement: CreateSettlement;
      try {
        const result = await createEvent.mutateAsync(input);
        settlement = finishCreateSuccess({
          tempId,
          serverEvent: result.event,
          posted: postedFieldsFromCreate(input),
          cutSourceId: item.cutSourceId,
        });
      } catch (err) {
        // The copy stays on the grid as an unsaved event (kept, retryable);
        // the cut source was never touched, so a failed paste costs nothing.
        finishCreateFailure(tempId, err, input);
        return false;
      }

      // Parked (the delete toast still owns the row) or discarded: there is
      // no visible copy, so a cut source stays put and a retry can still
      // remove it.
      if (settlement !== 'revealed') return false;

      const cutId = item.cutSourceId;
      // Not a cut: no source was deleted, so the page has nothing to clear.
      if (!cutId) return false;
      // The helper resolves the alias first, so a source whose create has
      // returned by now is really cut. One that is still in flight has no row
      // to delete: false leaves the copy in place and keeps the cut on the
      // clipboard for a later paste, and a second paste of the same cut must
      // not claim another removal either.
      return removeCutSource(cutId);
    },
    [
      calendars,
      clearWriteError,
      createEvent,
      finishCreateFailure,
      finishCreateSuccess,
      queue,
      removeCutSource,
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
    () => buildAllDayChips(days, allDayEvents, dayCount, unsavedEventIds),
    [days, allDayEvents, dayCount, unsavedEventIds],
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
      // A drag session holds the id it started under; the alias maps a temp
      // id whose create returned mid-gesture onto the server row, so the drop
      // commits instead of finding nothing.
      const id = resolveId(eventId);
      const current =
        overlaidEvents.find((e) => e.id === id) ??
        events.find((e) => e.id === id);
      if (!current) return;
      const next: CalendarEvent = {
        ...current,
        start_time: range.start.toISOString(),
        end_time: range.end.toISOString(),
      };
      clearWriteError();
      // Paint immediately; PATCH follows unless the id is still temp (a create
      // in flight, or an unsaved create whose retry will carry the times).
      queue.upsert(next);

      // Still a temp id: no server row yet — a create in flight flushes these
      // times on success; an unsaved create re-POSTs now, so a drag is not
      // silently lost if the user never opens the inspector again.
      if (isTempEventId(id)) {
        retryIfUnsaved(id);
        return;
      }

      queue.enqueuePatch(id, {
        start: next.start_time,
        end: next.end_time,
      });
    },
    [overlaidEvents, events, resolveId, queue, clearWriteError, retryIfUnsaved],
  );

  /**
   * Inspector time/date edits — same overlay + PATCH path as drag/resize, but
   * an explicit save: an unsaved create re-POSTs with the new times (a chip
   * drag stays local; its times travel with the next retry).
   */
  const handleSaveTimes = useCallback(
    (startIso: string, endIso: string) => {
      if (!selectedEventId) return;
      const start = new Date(startIso);
      const end = new Date(endIso);
      if (Number.isNaN(start.getTime()) || Number.isNaN(end.getTime())) return;
      // The shared move path also re-POSTs an unsaved create with its new
      // times, so an inspector save and a chip drop behave the same.
      handleMoveOrResize(selectedEventId, { start, end });
    },
    [selectedEventId, handleMoveOrResize],
  );

  /** Inspector all-day toggle / all-day date shift. */
  const handleSaveAllDay = useCallback(
    (next: { isAllDay: boolean; startIso: string; endIso: string }) => {
      if (!selectedEventId) return;
      const id = resolveId(selectedEventId);
      const current = overlaidEvents.find((e) => e.id === id);
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

      // Temp / draft: overlay only. A create in flight flushes the field PATCH
      // on success; an unsaved create re-POSTs with this flag and its times.
      if (id === queue.getDraftId() || isTempEventId(id)) {
        if (isTempEventId(id)) retryIfUnsaved(id);
        return;
      }

      queue.enqueuePatch(id, {
        is_all_day: next.isAllDay,
        start: next.startIso,
        end: next.endIso,
      });
    },
    [
      selectedEventId,
      resolveId,
      overlaidEvents,
      queue,
      retryIfUnsaved,
      clearWriteError,
    ],
  );

  /** Inspector time-zone change (timed events only). */
  const handleSaveTimeZone = useCallback(
    (timeZone: string, startIso: string, endIso: string) => {
      if (!selectedEventId) return;
      const id = resolveId(selectedEventId);
      const current = overlaidEvents.find((e) => e.id === id);
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

      if (id === queue.getDraftId() || isTempEventId(id)) {
        // A zone is part of the retried create body (its full local state).
        if (isTempEventId(id)) retryIfUnsaved(id);
        return;
      }

      // Empty zone = Local: queue keeps '' so it supersedes an earlier pending
      // zone, and the wire request omits timeZone on Google.
      queue.enqueuePatch(id, {
        start: startIso,
        end: endIso,
        start_time_zone: zone,
      });
    },
    [
      selectedEventId,
      resolveId,
      overlaidEvents,
      queue,
      retryIfUnsaved,
      clearWriteError,
    ],
  );

  const handleEmptyClick = useCallback(() => {
    // Desktop empty-cell click: discard an open draft only. Leave a real
    // selected event, and an unsaved create (a failed POST is not a draft),
    // alone with its inspector.
    const hadDraft = queue.getDraftId() !== null;
    discardUnpersistedDraft();
    if (hadDraft) {
      setSelectedEventId(null);
      setFocusTitleOnOpen(false);
    }
  }, [discardUnpersistedDraft, queue]);

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
  // Resolved so a create returning mid-drag keeps the server chip dimmed.
  const draggingEventId =
    drag.isDragging && drag.activeEventId
      ? resolveId(drag.activeEventId)
      : null;

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
      // A chip tap that outlived its create can still carry the temp id.
      const id = resolveId(eventId);
      const event =
        overlaidEvents.find((ev) => ev.id === id) ??
        events.find((ev) => ev.id === id);
      if (!event) return;
      drag.onAllDayChipPointerDown(e, event, chipEl);
    },
    [overlaidEvents, events, resolveId, drag],
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

  // The drag session keeps the id it started under. A create returning
  // mid-gesture clears the temp overlay, so the resolved lookup is what keeps
  // the ghost's color / title instead of resetting them to the default.
  const activeDragId = drag.activeEventId;
  const activeDragEvent = activeDragId
    ? overlaidEvents.find((e) => e.id === resolveId(activeDragId))
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
    writeError: writeError?.message ?? null,
    clearWriteError,
    // A create failure keeps its event on the grid, and a failed PATCH keeps
    // its intent painted, so the banner can save either instead of only
    // reporting the loss.
    canRetryWrites:
      (writeError?.retryCreates === true && unsavedEventIds.size > 0) ||
      (writeError?.retryPatches === true && queue.hasFailedWrites),
    retryWrites,
    retryUnsavedCreate,
    retryUnsavedCreates,
    // Ids painted as local-only events whose create failed; the grid marks
    // them and the inspector offers "Not saved — Retry".
    unsavedEventIds,

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
    // Resolved for painting: a create that returned after the selection was
    // made leaves a temp id in state for one render, and the chip on the grid
    // already carries the server id.
    selectedEventId: selectedEventId ? resolveId(selectedEventId) : null,
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

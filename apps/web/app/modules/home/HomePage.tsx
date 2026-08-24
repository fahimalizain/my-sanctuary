// Home — the daily Agenda (ADR 0004 § Surfaces — Home): a ranked LIVING
// list of tasks + routine occurrences above a muted Completed dump (ADR
// 0004 amendment — parked rows: terminal tasks + done occurrences, newest
// embed `updated_at` first, no grip, not a drop target), date selector
// (prev / today / next + calendar pick), add-task picker, reorder by grab
// handle on living rows only (dnd-kit — same-day only, ADR 0004 amendment:
// reorder is a handle, chevrons are gone), check-off, skip, start (today's
// pending occurrences), reschedule (the calendar popover — Tomorrow / pick
// a day), occurrence rename, and the existing TaskModal for task edits.
// Replaces the mock timeline (SkewedTimeline stays in components/, unused
// — no drive-by delete).

import { useEffect, useMemo, useRef, useState } from 'react';
import { Link, useNavigate } from '@tanstack/react-router';
import {
  DndContext,
  DragOverlay,
  MouseSensor,
  TouchSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { ChevronLeft, ChevronRight, Loader2, Plus, Repeat } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { FocusTimer } from '@/app/components/FocusTimer';
import { QuotesSection } from '@/app/components/QuotesSection';
import { TaskModal } from '@/app/components/TaskModal';
import { quotes } from '@/app/mock-data';
import {
  setAgendaItems,
  useAgendaQuery,
  useCreateAgendaItem,
  useDeleteAgendaItem,
  useMoveAgendaItem,
  useRescheduleAgendaItem,
} from '@/app/queries/agenda';
import { queryKeys } from '@/app/queries/keys';
import {
  useCompleteOccurrence,
  useReopenOccurrence,
  useSkipOccurrence,
  useStartOccurrence,
  useUpdateOccurrence,
} from '@/app/queries/occurrences';
import {
  setTasksCache,
  useDeleteTask,
  useMoveTask,
  useRunTaskAction,
  useUpdateTask,
} from '@/app/queries/tasks';
import { queryClient } from '@/lib/queryClient';
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import { TaskPickerDialog } from './TaskPickerDialog';
import { addCivilDays } from '@/app/modules/routines/rrule-preview';
import {
  agendaDateLabel,
  agendaMoveTargetAt,
  applyAgendaMove,
  partitionAgendaItems,
} from './agenda-helpers';
import type {
  AgendaItemRecord,
  MoveTaskInput,
  NewAgendaItemInput,
  OccurrenceRecord,
  OccurrenceStatus,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskStatus,
  UpdateTaskInput,
} from '@/app/types';

export function HomePage() {
  const navigate = useNavigate();
  // The viewed date. Empty until the first load: the server decides "today"
  // (ADR 0004 amendment — the browser's local date must NOT pick the default
  // Home date), then the first GET's `today` becomes the viewed date.
  const [date, setDate] = useState('');
  // `date === ''` → the sentinel key ['agenda','today'] and a `getAgenda()`
  // with no `?date=` (ADR 0004 — the browser never computes today).
  const agendaQuery = useAgendaQuery(date);
  const items = agendaQuery.data?.items ?? [];
  // The server's civil today (primary calendar time zone, chrono-tz) —
  // refreshed from EVERY GET; anchors the header label, the "Today" button,
  // and the Play gate.
  const serverToday = agendaQuery.data?.today ?? '';
  const isLoading = agendaQuery.isLoading;
  // Load failures: the date-keyed query error. Replaces the list when there
  // are no rows; with rows on screen it reads as a refresh-failure notice.
  const loadError =
    agendaQuery.error instanceof Error
      ? agendaQuery.error.message
      : agendaQuery.error
        ? 'Failed to load agenda'
        : null;
  const itemsRef = useRef<AgendaItemRecord[]>([]);
  itemsRef.current = items;
  // Latest `date` for the mutation callbacks below (same "latest value"
  // pattern as RoutinesPage's routinesRef).
  const dateRef = useRef(date);
  dateRef.current = date;
  // The two piles (ADR 0004 amendment): `living` is the ranked, sortable
  // list (sort_order asc); `completed` is the parked dump below it (newest
  // embed `updated_at` first). Both derive from the same `items` — the
  // server has no concept of a pile; partitioning is purely presentational.
  const { living, completed } = useMemo(
    () => partitionAgendaItems(items),
    [items],
  );
  // Latest `living` for the drag handlers (same pattern as itemsRef):
  // dnd-kit resolves the drop index against what is currently on screen —
  // the sortable list — never against the full pile, which also holds the
  // parked rows.
  const livingRef = useRef(living);
  livingRef.current = living;
  // Action failures (move/complete/skip/etc.): a banner above the still-
  // visible list — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);
  // The row currently dragged — feeds the DragOverlay title chip.
  const [activeDrag, setActiveDrag] = useState<AgendaItemRecord | null>(null);

  const [pickerOpen, setPickerOpen] = useState(false);
  const [taskModal, setTaskModal] = useState<TaskRecord | null>(null);
  const [renaming, setRenaming] = useState<OccurrenceRecord | null>(null);
  const [renameTitle, setRenameTitle] = useState('');
  const [renameError, setRenameError] = useState<string | null>(null);
  const [renameSaving, setRenameSaving] = useState(false);

  // First-load seed (mandatory — prevents a flash-refetch): the sentinel
  // query succeeded — copy its payload onto the civil-today key and adopt
  // that date as the viewed date. The follow-up `useAgendaQuery(today)` is
  // then a cache hit: no second GET, no empty flash.
  useEffect(() => {
    if (date !== '' || !agendaQuery.isSuccess || !agendaQuery.data) return;
    const today = agendaQuery.data.today;
    queryClient.setQueryData(queryKeys.agenda.byDate(today), agendaQuery.data);
    setDate(today);
  }, [date, agendaQuery.isSuccess, agendaQuery.data]);

  // Optimistic agenda writes: the page owns the paint, the hooks own the
  // API call + in-flight cancel. `setItems` writes the viewed date's key.
  const setItems = (
    updater:
      | AgendaItemRecord[]
      | ((prev: AgendaItemRecord[]) => AgendaItemRecord[]),
  ) => setAgendaItems(dateRef.current, updater);

  const sortItems = (list: AgendaItemRecord[]): AgendaItemRecord[] =>
    [...list].sort((a, b) => a.sort_order - b.sort_order);

  // Agenda write mutations: thin wrappers that cancel the viewed-
  // date query on mutate so an in-flight refetch can never resolve over the
  // page's optimistic cache mid-write. No invalidation on success — the
  // handlers merge the authoritative row themselves.
  const moveAgendaItemMutation = useMoveAgendaItem();
  const rescheduleAgendaItemMutation = useRescheduleAgendaItem();
  const createAgendaItemMutation = useCreateAgendaItem();
  const deleteAgendaItemMutation = useDeleteAgendaItem();
  const completeOccurrenceMutation = useCompleteOccurrence();
  const skipOccurrenceMutation = useSkipOccurrence();
  const reopenOccurrenceMutation = useReopenOccurrence();
  const startOccurrenceMutation = useStartOccurrence();
  const updateOccurrenceMutation = useUpdateOccurrence();
  // Task writes reuse the shared task mutations from `queries/tasks.ts`;
  // after each success the handler also patches the `['tasks']` cache so
  // Board/Lists see the fresh row (same sibling-rank contract —
  // never invalidate).
  const runTaskActionMutation = useRunTaskAction();
  const updateTaskMutation = useUpdateTask();
  const deleteTaskMutation = useDeleteTask();
  const moveTaskMutation = useMoveTask();

  // Agenda reorder sensors: a vertical list with a dedicated grip means a
  // short move activates — Mouse 8px + Touch 8px, still NO 250ms hold
  // (unlike the board, whose delay beats its horizontal pan). PointerSensor
  // is gone: Chrome DevTools device mode and many phones speak touch
  // events, not pointer, so it never fires for them. The listeners live
  // only on the grip (touch-none), so taps on any other row control never
  // reach a sensor; the distance constraint stops a tap from dragging while
  // a swipe on the handle still starts a drag.
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 8 } }),
    useSensor(TouchSensor, { activationConstraint: { distance: 8 } }),
  );

  const changeDate = (next: string) => {
    if (!next || next === date) return;
    setDate(next);
  };

  // ──────────────────────────────────────────
  // Reorder (grab handle → POST the absolute rank)
  // ──────────────────────────────────────────

  /** Optimistic move onto the slot at `toIndex` — the same snapshot /
   *  POST / merge path the chevrons used, with `agendaMoveTargetAt` keeping
   *  the rank math server-exact. from/to resolve against the LIVING pile
   *  (parked rows sit in the Completed dump, not the sortable list, so a
   *  parked row between living ones must not shift the drop target), while
   *  `applyAgendaMove` still shifts the FULL pile — the server shifts every
   *  peer on the date, parked ranks included. */
  const handleMoveTo = async (itemId: string, toIndex: number) => {
    const snapshot = itemsRef.current;
    const livingNow = partitionAgendaItems(snapshot).living;
    const fromIndex = livingNow.findIndex((entry) => entry.id === itemId);
    const target = agendaMoveTargetAt(livingNow, fromIndex, toIndex);
    if (target === null) return;
    // Optimistic paint that mirrors the server's shift exactly.
    setItems(applyAgendaMove(snapshot, itemId, target));
    setActionError(null);
    try {
      const data = await moveAgendaItemMutation.mutateAsync({
        id: itemId,
        input: { sort_order: target },
        date: dateRef.current,
      });
      // Merge the authoritative row (fresh embeds); ranks already match.
      setItems((prev) =>
        sortItems(
          prev.map((entry) => (entry.id === data.item.id ? data.item : entry)),
        ),
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
    }
  };

  /** dnd-kit drop: resolve the active/over ids against the CURRENT LIVING
   *  pile — the sortable list on screen, which never contains parked rows
   *  (ranks may also have shifted since the drag started) — and hand the
   *  target slot to the shared move path. A drop on itself, no target, or a
   *  stale id is a no-op — `agendaMoveTargetAt` returns null. */
  const handleDragEnd = (event: DragEndEvent) => {
    setActiveDrag(null);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const toIndex = livingRef.current.findIndex(
      (entry) => entry.id === over.id,
    );
    void handleMoveTo(String(active.id), toIndex);
  };

  // ──────────────────────────────────────────
  // Reschedule (move a slot to another day — optimistic drop)
  // ──────────────────────────────────────────

  /** Move a row to another day (ADR 0004 amendment — reschedule). Optimistic:
   *  the row drops from this pile immediately (it belongs to the target date
   *  now); a failure reverts the list and banners. A pick of the row's own
   *  date is a 200 no-op server-side, so it short-circuits here. The success
   *  merge keeps the pile exact for the unusual "landed back on the viewed
   *  date" echo (e.g. the viewed date changed mid-flight). */
  const handleReschedule = async (
    item: AgendaItemRecord,
    targetDate: string,
  ) => {
    if (!targetDate || targetDate === item.local_date) return;
    const snapshot = itemsRef.current;
    setItems((prev) => prev.filter((entry) => entry.id !== item.id));
    setActionError(null);
    try {
      const data = await rescheduleAgendaItemMutation.mutateAsync({
        id: item.id,
        input: { date: targetDate },
        date: dateRef.current,
      });
      setItems((prev) => {
        const next = prev.filter((entry) => entry.id !== item.id);
        if (data.item.local_date === dateRef.current) next.push(data.item);
        return sortItems(next);
      });
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
    }
  };

  // ──────────────────────────────────────────
  // Check-off / skip (optimistic, per the verb contracts)
  // ──────────────────────────────────────────

  /** Task → the existing `/complete` (Board → Done) or, for a checked-off
   *  row, back to PLANNED via the existing Board move (ADR 0004 amendment —
   *  uncheck is the Board reopen, no new endpoint). Completing parks the
   *  row under the Completed dump right away: the optimistic embed stamps
   *  a fresh `updated_at` so the dump's newest-first sort jumps the row to
   *  the top. Unchecking does NOT stamp — living sorts by `sort_order`, and
   *  partition drops the row back in at its stored rank. */
  const handleCompleteTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    const snapshot = itemsRef.current;
    // Uncheck: COMPLETED → PLANNED (living again — Play/Reschedule return
    // because the row is no longer a finished card).
    if (task.status === 'COMPLETED') {
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.task
            ? { ...entry, task: { ...entry.task, status: 'PLANNED' } }
            : entry,
        ),
      );
      setActionError(null);
      try {
        const data = await moveTaskMutation.mutateAsync({
          id: task.id,
          input: { status: 'PLANNED' } satisfies MoveTaskInput,
        });
        setItems((prev) =>
          prev.map((entry) =>
            entry.id === item.id && entry.task
              ? { ...entry, task: data.task }
              : entry,
          ),
        );
        // Patch the shared tasks cache so Board/Lists see the fresh row.
        setTasksCache((prev) =>
          prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
        );
      } catch (err) {
        setItems(snapshot);
        setActionError(err instanceof Error ? err.message : 'Uncheck failed');
      }
      return;
    }
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.task
          ? {
              ...entry,
              task: {
                ...entry.task,
                status: 'COMPLETED',
                // Fresh stamp → the Completed dump sorts this row first.
                updated_at: new Date().toISOString(),
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await runTaskActionMutation.mutateAsync({
        id: task.id,
        action: 'complete',
      });
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.task
            ? { ...entry, task: data.task }
            : entry,
        ),
      );
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Complete failed');
    }
  };

  /** Task → the existing `/start` (Board → In Progress). The FocusTimer is a
   *  separate surface; this just flips the card. */
  const handleStartTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    setActionError(null);
    try {
      const data = await runTaskActionMutation.mutateAsync({
        id: task.id,
        action: 'start',
      });
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.task
            ? { ...entry, task: data.task }
            : entry,
        ),
      );
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Start failed');
    }
  };

  /** Remove from today = hard-delete the membership row (unpin). The task
   *  stays on the Board. Occurrence-kind items are refused by the API —
   *  skip is the decline. */
  const handleRemoveTask = async (item: AgendaItemRecord) => {
    const snapshot = itemsRef.current;
    setItems((prev) => prev.filter((entry) => entry.id !== item.id));
    setActionError(null);
    try {
      await deleteAgendaItemMutation.mutateAsync({
        id: item.id,
        date: dateRef.current,
      });
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Remove failed');
    }
  };

  /** Occurrence complete/skip — one shared optimistic path over the two
   *  verbs (the verb matrix is server-side; the UI just flips the chip).
   *  Complete ALSO stamps a fresh `updated_at` on the embed so the row
   *  parks at the top of the Completed dump; skip stays living (sorted by
   *  `sort_order`), so its stamp is untouched. */
  const setOccurrenceStatus = async (
    item: AgendaItemRecord,
    status: OccurrenceStatus,
    verb: 'complete' | 'skip',
  ) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = itemsRef.current;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.occurrence
          ? {
              ...entry,
              occurrence: {
                ...entry.occurrence,
                status,
                updated_at:
                  verb === 'complete'
                    ? new Date().toISOString()
                    : entry.occurrence.updated_at,
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data =
        verb === 'complete'
          ? await completeOccurrenceMutation.mutateAsync({
              id: occurrence.id,
              date: dateRef.current,
            })
          : await skipOccurrenceMutation.mutateAsync({
              id: occurrence.id,
              date: dateRef.current,
            });
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.occurrence
            ? { ...entry, occurrence: data.occurrence }
            : entry,
        ),
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Update failed');
    }
  };

  /** Occurrence reopen — the uncheck/unskip path (ADR 0004 amendment):
   *  done/skipped → pending with the chip ids cleared (`calendar_id` +
   *  `google_event_id` → null, matching the server's reopen). Same
   *  optimistic shape as setOccurrenceStatus: paint pending immediately,
   *  merge the authoritative occurrence on success, snapshot-rollback +
   *  banner on failure. Play returns when showStartOccurrence is on and the
   *  status is pending; Calendar/RescheduleControl return via the existing
   *  canReschedule. */
  const handleReopenOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = itemsRef.current;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.occurrence
          ? {
              ...entry,
              occurrence: {
                ...entry.occurrence,
                status: 'pending',
                calendar_id: null,
                google_event_id: null,
              },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await reopenOccurrenceMutation.mutateAsync({
        id: occurrence.id,
        date: dateRef.current,
      });
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.occurrence
            ? { ...entry, occurrence: data.occurrence }
            : entry,
        ),
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Update failed');
    }
  };

  /** Occurrence start: creates the one-shot Google log and flips
   *  the chip to in_progress. Today-only on the server; the Play button is
   *  only rendered for today's pending occurrences. Optimistic → the chip
   *  reads In progress immediately; a 401/400 rolls back with a banner. */
  const handleStartOccurrence = async (item: AgendaItemRecord) => {
    const occurrence = item.occurrence;
    if (!occurrence) return;
    const snapshot = itemsRef.current;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.occurrence
          ? {
              ...entry,
              occurrence: { ...entry.occurrence, status: 'in_progress' },
            }
          : entry,
      ),
    );
    setActionError(null);
    try {
      const data = await startOccurrenceMutation.mutateAsync({
        id: occurrence.id,
        date: dateRef.current,
      });
      setItems((prev) =>
        prev.map((entry) =>
          entry.id === item.id && entry.occurrence
            ? { ...entry, occurrence: data.occurrence }
            : entry,
        ),
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Start failed');
    }
  };

  // ──────────────────────────────────────────
  // Add task (picker → POST /api/agenda/items)
  // ──────────────────────────────────────────

  const handlePickTask = async (task: TaskRecord): Promise<string | null> => {
    setActionError(null);
    try {
      // The server appends at max+1 for the date — the sorted insert lands it
      // at the back of the pile.
      const data = await createAgendaItemMutation.mutateAsync({
        input: {
          kind: 'task',
          ref_id: task.id,
          date,
        } satisfies NewAgendaItemInput,
        date: dateRef.current,
      });
      setItems((prev) => sortItems([...prev, data.item]));
      return null;
    } catch (err) {
      return err instanceof Error ? err.message : 'Add failed';
    }
  };

  // ──────────────────────────────────────────
  // TaskModal (existing component — PATCH / DELETE / move)
  // ──────────────────────────────────────────

  const mergeTask = (task: TaskRecord) => {
    setItems((prev) =>
      prev.map((entry) =>
        entry.task && entry.task.id === task.id ? { ...entry, task } : entry,
      ),
    );
  };

  const handleTaskModalSubmit = async (values: {
    title: string;
    description: string;
    durationMinutes: number;
    priority: TaskPriority;
    difficulty: TaskDifficulty;
  }): Promise<string | null> => {
    if (!taskModal) return null;
    setActionError(null);
    try {
      // Merge the fresh embed straight into the rows (same refresh the
      // agenda reload would give, without the spinner flash).
      const data = await updateTaskMutation.mutateAsync({
        id: taskModal.id,
        input: {
          title: values.title,
          description: values.description,
          duration_minutes: values.durationMinutes,
          priority: values.priority,
          difficulty: values.difficulty,
        } satisfies UpdateTaskInput,
      });
      mergeTask(data.task);
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      return null; // the modal closes itself on success
    } catch (err) {
      return err instanceof Error ? err.message : 'Save failed';
    }
  };

  const handleTaskModalDelete = async (
    taskId: string,
  ): Promise<string | null> => {
    setActionError(null);
    try {
      await deleteTaskMutation.mutateAsync(taskId);
    } catch (err) {
      return err instanceof Error ? err.message : 'Delete failed';
    }
    // The membership row is an orphan after the task dies — the next GET
    // omits it server-side; drop it locally now.
    setItems((prev) => prev.filter((entry) => entry.task?.id !== taskId));
    // Patch the shared tasks cache so Board/Lists drop the task too.
    setTasksCache((prev) => prev.filter((entry) => entry.id !== taskId));
    return null;
  };

  const handleTaskModalMove = async (
    taskId: string,
    status: TaskStatus,
  ): Promise<string | null> => {
    setActionError(null);
    try {
      const data = await moveTaskMutation.mutateAsync({
        id: taskId,
        input: { status } satisfies MoveTaskInput,
      });
      mergeTask(data.task);
      // Patch the shared tasks cache so Board/Lists see the fresh row.
      setTasksCache((prev) =>
        prev.map((entry) => (entry.id === data.task.id ? data.task : entry)),
      );
      // The modal's status pills read `task.status` from its props — keep the
      // edited copy fresh so the selection follows the server.
      setTaskModal((prev) => (prev && prev.id === taskId ? data.task : prev));
      return null;
    } catch (err) {
      return err instanceof Error ? err.message : 'Move failed';
    }
  };

  // ──────────────────────────────────────────
  // Occurrence rename (day-level override; empty clears to inherit)
  // ──────────────────────────────────────────

  const openRename = (occurrence: OccurrenceRecord) => {
    setRenaming(occurrence);
    setRenameTitle(occurrence.title ?? '');
    setRenameError(null);
  };

  const closeRename = () => {
    setRenaming(null);
    setRenameError(null);
  };

  const handleRenameSave = async () => {
    if (!renaming) return;
    setRenameSaving(true);
    setRenameError(null);
    try {
      // Empty/whitespace clears the override — the day inherits again.
      const data = await updateOccurrenceMutation.mutateAsync({
        id: renaming.id,
        input: { title: renameTitle.trim() },
        date: dateRef.current,
      });
      setItems((prev) =>
        prev.map((entry) =>
          entry.occurrence && entry.occurrence.id === data.occurrence.id
            ? { ...entry, occurrence: data.occurrence }
            : entry,
        ),
      );
    } catch (err) {
      setRenameSaving(false);
      setRenameError(err instanceof Error ? err.message : 'Save failed');
      return;
    }
    setRenameSaving(false);
    setRenaming(null);
  };

  // ──────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────

  // Header + Play gate anchor on the SERVER's civil today (ADR 0004
  // amendment) — never the browser's local date. Before the first load both
  // are empty and the label reads "Today" with no rows on screen.
  const dateLabel = agendaDateLabel(date || serverToday, serverToday);
  const isToday = !!serverToday && (date || serverToday) === serverToday;
  const excludedTaskIds = new Set(
    items
      .filter((entry) => entry.kind === 'task' && entry.task)
      .map((entry) => entry.task!.id),
  );

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 pt-8 pb-28">
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-8">
          {/* Main column — the agenda */}
          <div className="lg:col-span-2">
            <header className="flex items-center justify-between gap-4 mb-5">
              <h1 className="font-heading text-3xl font-bold text-foreground">
                {dateLabel}
              </h1>
              <div className="flex items-center gap-3 flex-shrink-0">
                <Link
                  to="/routines"
                  className="inline-flex items-center gap-1.5 text-sm font-medium text-primary hover:underline"
                >
                  <Repeat className="h-4 w-4" />
                  Routines
                </Link>
                <Button
                  onClick={() => setPickerOpen(true)}
                  className="flex-shrink-0"
                >
                  <Plus className="h-4 w-4 mr-1" />
                  Add task
                </Button>
              </div>
            </header>

            {/* Date controls — prev / date input / next / Today */}
            <div className="flex flex-wrap items-center gap-2 mb-6">
              <Button
                variant="outline"
                size="icon"
                onClick={() => changeDate(addCivilDays(date, -1))}
                aria-label="Previous day"
                className="border-input text-foreground hover:bg-muted"
              >
                <ChevronLeft className="h-4 w-4" />
              </Button>
              <input
                type="date"
                value={date}
                onChange={(e) => changeDate(e.target.value)}
                aria-label="Pick a date"
                className="px-3 py-2 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              <Button
                variant="outline"
                size="icon"
                onClick={() => changeDate(addCivilDays(date, 1))}
                aria-label="Next day"
                className="border-input text-foreground hover:bg-muted"
              >
                <ChevronRight className="h-4 w-4" />
              </Button>
              <Button
                variant="outline"
                onClick={() => changeDate(serverToday)}
                className="border-input text-foreground hover:bg-muted"
              >
                Today
              </Button>
              <span className="ml-1 text-sm text-muted-foreground">{date}</span>
            </div>

            {/* Load error banner — replaces the list only when there are no
                rows; with rows on screen it reads as a refresh-failure
                notice above the still-visible list */}
            {loadError && (
              <div className="mb-6 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
                <p className="text-sm">{loadError}</p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => void agendaQuery.refetch()}
                >
                  Retry
                </Button>
              </div>
            )}

            {/* Action error banner (move/complete/skip failures) — the list
                stays mounted */}
            {actionError && (
              <div className="mb-6 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
                <p className="text-sm">{actionError}</p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setActionError(null)}
                >
                  Dismiss
                </Button>
              </div>
            )}

            {/* Loading — only while the list is empty (first load or a date
                switch; a retry keeps the banner until the refetch lands) */}
            {isLoading && items.length === 0 && (
              <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
                <Loader2 className="h-5 w-5 animate-spin" />
                Loading agenda…
              </div>
            )}

            {/* Empty state */}
            {!isLoading && !loadError && items.length === 0 && (
              <div className="rounded-xl border border-dashed border-border bg-card/50 p-10 text-center">
                <p className="font-heading text-lg font-semibold text-foreground mb-1">
                  Nothing on this day
                </p>
                <p className="text-sm text-muted-foreground">
                  Add a task, or create a routine.
                </p>
                <div className="mt-4 flex justify-center gap-3">
                  <Button onClick={() => setPickerOpen(true)}>
                    <Plus className="h-4 w-4 mr-1" />
                    Add a task
                  </Button>
                  <Button
                    variant="outline"
                    onClick={() => navigate({ to: '/routines' })}
                    className="border-input text-foreground hover:bg-muted"
                  >
                    Create a routine
                  </Button>
                </div>
              </div>
            )}

            {/* The living pile — sort_order asc; same-day reorder only (each
                date is its own pile, no cross-date drag). Parked rows are
                NOT here: the Completed dump renders below, outside the
                DndContext, so it is never a drop target. */}
            {living.length > 0 && (
              <DndContext
                sensors={sensors}
                collisionDetection={closestCenter}
                onDragStart={(event) =>
                  setActiveDrag(
                    itemsRef.current.find(
                      (entry) => entry.id === event.active.id,
                    ) ?? null,
                  )
                }
                onDragEnd={handleDragEnd}
                onDragCancel={() => setActiveDrag(null)}
              >
                <SortableContext
                  items={living.map((entry) => entry.id)}
                  strategy={verticalListSortingStrategy}
                >
                  <div className="space-y-2">
                    {living.map((item) => (
                      <SortableAgendaItemRow
                        key={item.id}
                        item={item}
                        onReschedule={(entry, targetDate) =>
                          void handleReschedule(entry, targetDate)
                        }
                        onCompleteTask={(entry) =>
                          void handleCompleteTask(entry)
                        }
                        onStartTask={(entry) => void handleStartTask(entry)}
                        onRemoveTask={(entry) => void handleRemoveTask(entry)}
                        onOpenTask={(task) => setTaskModal(task)}
                        onCompleteOccurrence={(entry) =>
                          entry.occurrence?.status === 'done'
                            ? void handleReopenOccurrence(entry)
                            : void setOccurrenceStatus(
                                entry,
                                'done',
                                'complete',
                              )
                        }
                        onSkipOccurrence={(entry) =>
                          entry.occurrence?.status === 'skipped'
                            ? void handleReopenOccurrence(entry)
                            : void setOccurrenceStatus(entry, 'skipped', 'skip')
                        }
                        onStartOccurrence={(entry) =>
                          void handleStartOccurrence(entry)
                        }
                        onRenameOccurrence={(occurrence) =>
                          openRename(occurrence)
                        }
                        showStartOccurrence={isToday}
                      />
                    ))}
                  </div>
                </SortableContext>

                {/* Floating title chip while dragging — the source row stays
                    in place, dimmed (opacity-40 on its wrapper) */}
                <DragOverlay>
                  {activeDrag && (
                    <div className="cursor-grabbing rounded-lg border border-border bg-background px-3 py-2 shadow-xl ring-1 ring-border/60">
                      <span className="text-sm font-medium text-foreground">
                        {activeDrag.kind === 'occurrence' &&
                        activeDrag.occurrence
                          ? activeDrag.occurrence.resolved_title
                          : (activeDrag.task?.display_title ?? '')}
                      </span>
                    </div>
                  )}
                </DragOverlay>
              </DndContext>
            )}

            {/* The Completed dump (ADR 0004 amendment): parked rows —
                terminal tasks (COMPLETED | DISCARDED) and done occurrences
                — newest embed `updated_at` first. Sibling below the living
                list, OUTSIDE the DndContext: plain rows with no grip, never
                a drop target. Unchecking a row here repartitions it back
                into the living pile at its stored `sort_order`. */}
            {completed.length > 0 && (
              <section className="space-y-2 pt-4">
                <h2 className="px-1 text-xs font-medium tracking-wide text-muted-foreground">
                  Completed
                </h2>
                <div className="space-y-2">
                  {completed.map((item) => (
                    <AgendaItemRow
                      key={item.id}
                      item={item}
                      showHandle={false}
                      isDragging={false}
                      onReschedule={(entry, targetDate) =>
                        void handleReschedule(entry, targetDate)
                      }
                      onCompleteTask={(entry) =>
                        void handleCompleteTask(entry)
                      }
                      onStartTask={(entry) => void handleStartTask(entry)}
                      onRemoveTask={(entry) => void handleRemoveTask(entry)}
                      onOpenTask={(task) => setTaskModal(task)}
                      onCompleteOccurrence={(entry) =>
                        entry.occurrence?.status === 'done'
                          ? void handleReopenOccurrence(entry)
                          : void setOccurrenceStatus(
                              entry,
                              'done',
                              'complete',
                            )
                      }
                      onSkipOccurrence={(entry) =>
                        entry.occurrence?.status === 'skipped'
                          ? void handleReopenOccurrence(entry)
                          : void setOccurrenceStatus(entry, 'skipped', 'skip')
                      }
                      onStartOccurrence={(entry) =>
                        void handleStartOccurrence(entry)
                      }
                      onRenameOccurrence={(occurrence) =>
                        openRename(occurrence)
                      }
                      showStartOccurrence={isToday}
                    />
                  ))}
                </div>
              </section>
            )}
          </div>

          {/* Right panel — Focus & Quotes (unchanged) */}
          <div className="space-y-6">
            <FocusTimer />
            <QuotesSection quotes={quotes} />
          </div>
        </div>

        {/* Add-task picker */}
        <TaskPickerDialog
          open={pickerOpen}
          onOpenChange={setPickerOpen}
          excludedTaskIds={excludedTaskIds}
          onPick={handlePickTask}
        />

        {/* Task edit modal — the existing TaskModal (edit/delete/move) */}
        <TaskModal
          open={taskModal !== null}
          onOpenChange={(open) => {
            if (!open) setTaskModal(null);
          }}
          task={taskModal ?? undefined}
          onSubmit={handleTaskModalSubmit}
          onDelete={handleTaskModalDelete}
          onMove={handleTaskModalMove}
        />

        {/* Occurrence rename dialog — a day-level override; empty clears */}
        <Dialog
          open={renaming !== null}
          onOpenChange={(open) => {
            if (!open) closeRename();
          }}
        >
          <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-[480px] bg-card border-border">
            <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
              <DialogTitle className="text-foreground">
                Rename for this day
              </DialogTitle>
              <DialogDescription>
                Changes this occurrence only — never the routine. Leave empty to
                inherit the routine title again.
              </DialogDescription>
            </DialogHeader>
            <hr className="shrink-0 border-border" />
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 py-4">
              <input
                type="text"
                value={renameTitle}
                onChange={(e) => setRenameTitle(e.target.value)}
                placeholder={
                  renaming
                    ? renaming.title == null
                      ? `Inherit: ${renaming.resolved_title}`
                      : 'Day override — empty to inherit'
                    : undefined
                }
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              {renameError && (
                <p className="mt-3 text-sm text-destructive">{renameError}</p>
              )}
            </div>
            <div className="flex shrink-0 justify-end gap-3 border-t border-border px-6 py-4">
              <Button
                variant="outline"
                onClick={closeRename}
                disabled={renameSaving}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={() => void handleRenameSave()}
                disabled={renameSaving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {renameSaving && (
                  <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                )}
                Save
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      </div>
    </div>
  );
}

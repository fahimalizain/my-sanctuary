// Home — the daily Agenda (ADR 0004 § Surfaces — Home): date-scoped mixed
// list of tasks + routine occurrences, date selector (prev / today / next +
// calendar pick), add-task picker, reorder, check-off, skip, start (today's
// pending occurrences), reschedule (Tomorrow / pick a day), occurrence
// rename, and the existing TaskModal for task edits. Replaces the mock
// timeline (SkewedTimeline stays in components/, unused — no drive-by
// delete).

import { useCallback, useEffect, useRef, useState } from 'react';
import { Link, useNavigate } from '@tanstack/react-router';
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
import { API_BASE_URL } from '@/lib/api';
import { AgendaItemRow } from './AgendaItemRow';
import { TaskPickerDialog } from './TaskPickerDialog';
import { addCivilDays, civilToday } from '@/app/modules/routines/rrule-preview';
import {
  agendaDateLabel,
  agendaMoveTarget,
  applyAgendaMove,
  readError,
} from './agenda-helpers';
import type {
  AgendaItemRecord,
  AgendaItemResponse,
  AgendaResponse,
  MoveAgendaItemInput,
  MoveTaskInput,
  MoveTaskResponse,
  NewAgendaItemInput,
  OccurrenceActionResponse,
  OccurrenceRecord,
  OccurrenceResponse,
  OccurrenceStatus,
  RescheduleAgendaItemInput,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskResponse,
  TaskStatus,
  UpdateTaskInput,
} from '@/app/types';

export function HomePage() {
  const navigate = useNavigate();
  // Default date = the browser's civil today (ADR 0004: "today" is the civil
  // date implied by now; acceptable for v1).
  const [date, setDate] = useState(civilToday());
  const [items, setItems] = useState<AgendaItemRecord[]>([]);
  const itemsRef = useRef<AgendaItemRecord[]>([]);
  itemsRef.current = items;
  // Latest `date` for the load callback below (same "latest value" pattern
  // as RoutinesPage's routinesRef).
  const dateRef = useRef(date);
  dateRef.current = date;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the list when there are
  // no rows; with rows on screen it reads as a refresh-failure notice.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (move/complete/skip/etc.): a banner above the still-
  // visible list — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  const [pickerOpen, setPickerOpen] = useState(false);
  const [taskModal, setTaskModal] = useState<TaskRecord | null>(null);
  const [renaming, setRenaming] = useState<OccurrenceRecord | null>(null);
  const [renameTitle, setRenameTitle] = useState('');
  const [renameError, setRenameError] = useState<string | null>(null);
  const [renameSaving, setRenameSaving] = useState(false);

  const sortItems = (list: AgendaItemRecord[]): AgendaItemRecord[] =>
    [...list].sort((a, b) => a.sort_order - b.sort_order);

  // A monotone token drops superseded fetches: changing the date (or
  // starting a retry) invalidates any in-flight load for the old date, so a
  // late response can never paint yesterday's rows under today's header.
  const loadSeq = useRef(0);

  const load = useCallback(() => {
    const seq = ++loadSeq.current;
    const requestedDate = dateRef.current;
    // Full-page loader only while the list is empty — reloads fired while
    // rows are on screen never flash the spinner.
    setIsLoading(itemsRef.current.length === 0);
    setLoadError(null);
    fetch(
      `${API_BASE_URL}/api/agenda?date=${encodeURIComponent(requestedDate)}`,
      { credentials: 'include' },
    )
      .then(async (res) => {
        if (seq !== loadSeq.current) return; // superseded
        if (!res.ok) throw new Error(await readError(res));
        const data = (await res.json()) as AgendaResponse;
        setItems(sortItems(data.items ?? []));
      })
      .catch((err: unknown) => {
        if (seq !== loadSeq.current) return; // superseded
        setLoadError(
          err instanceof Error ? err.message : 'Failed to load agenda',
        );
      })
      .finally(() => {
        if (seq !== loadSeq.current) return;
        setIsLoading(false);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load, date]);

  const changeDate = (next: string) => {
    if (!next || next === date) return;
    loadSeq.current++; // drop any in-flight load for the old date
    setItems([]); // never show the old date's rows under the new header
    setDate(next);
  };

  // ──────────────────────────────────────────
  // Reorder (up/down — POST the absolute rank)
  // ──────────────────────────────────────────

  const handleMove = async (itemId: string, direction: 'up' | 'down') => {
    const target = agendaMoveTarget(itemsRef.current, itemId, direction);
    if (target === null) return;
    const snapshot = itemsRef.current;
    // Optimistic paint that mirrors the server's shift exactly.
    setItems(applyAgendaMove(snapshot, itemId, target));
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(`${API_BASE_URL}/api/agenda/items/${itemId}/move`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          sort_order: target,
        } satisfies MoveAgendaItemInput),
      });
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
      return;
    }
    // Merge the authoritative row (fresh embeds); ranks already match.
    const data = (await res.json()) as AgendaItemResponse;
    setItems((prev) =>
      sortItems(
        prev.map((entry) => (entry.id === data.item.id ? data.item : entry)),
      ),
    );
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
    let res: Response;
    try {
      res = await fetch(
        `${API_BASE_URL}/api/agenda/items/${item.id}/reschedule`,
        {
          method: 'POST',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            date: targetDate,
          } satisfies RescheduleAgendaItemInput),
        },
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Move failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
      return;
    }
    const data = (await res.json()) as AgendaItemResponse;
    setItems((prev) => {
      const next = prev.filter((entry) => entry.id !== item.id);
      if (data.item.local_date === dateRef.current) next.push(data.item);
      return sortItems(next);
    });
  };

  // ──────────────────────────────────────────
  // Check-off / skip (optimistic, per the verb contracts)
  // ──────────────────────────────────────────

  /** Task → the existing `/complete` (Board → Done). The agenda row stays as
   *  a crossed-off row for the rest of that date (ADR 0004 § Crossing off). */
  const handleCompleteTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    const snapshot = itemsRef.current;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.task
          ? { ...entry, task: { ...entry.task, status: 'COMPLETED' } }
          : entry,
      ),
    );
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(`${API_BASE_URL}/api/tasks/${task.id}/complete`, {
        method: 'POST',
        credentials: 'include',
      });
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Complete failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
      return;
    }
    const data = (await res.json()) as MoveTaskResponse;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.task
          ? { ...entry, task: data.task }
          : entry,
      ),
    );
  };

  /** Task → the existing `/start` (Board → In Progress). The FocusTimer is a
   *  separate surface; this just flips the card. */
  const handleStartTask = async (item: AgendaItemRecord) => {
    const task = item.task;
    if (!task) return;
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(`${API_BASE_URL}/api/tasks/${task.id}/start`, {
        method: 'POST',
        credentials: 'include',
      });
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Start failed');
      return;
    }
    if (!res.ok) {
      setActionError(await readError(res));
      return;
    }
    const data = (await res.json()) as MoveTaskResponse;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.task
          ? { ...entry, task: data.task }
          : entry,
      ),
    );
  };

  /** Remove from today = hard-delete the membership row (unpin). The task
   *  stays on the Board. Occurrence-kind items are refused by the API —
   *  skip is the decline. */
  const handleRemoveTask = async (item: AgendaItemRecord) => {
    const snapshot = itemsRef.current;
    setItems((prev) => prev.filter((entry) => entry.id !== item.id));
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(`${API_BASE_URL}/api/agenda/items/${item.id}`, {
        method: 'DELETE',
        credentials: 'include',
      });
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Remove failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
    }
  };

  /** Occurrence complete/skip — one shared optimistic path over the two
   *  verbs (the verb matrix is server-side; the UI just flips the chip). */
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
          ? { ...entry, occurrence: { ...entry.occurrence, status } }
          : entry,
      ),
    );
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(
        `${API_BASE_URL}/api/occurrences/${occurrence.id}/${verb}`,
        { method: 'POST', credentials: 'include' },
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Update failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
      return;
    }
    const data = (await res.json()) as OccurrenceResponse;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.occurrence
          ? { ...entry, occurrence: data.occurrence }
          : entry,
      ),
    );
  };

  /** Occurrence start (slice 6): creates the one-shot Google log and flips
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
          ? { ...entry, occurrence: { ...entry.occurrence, status: 'in_progress' } }
          : entry,
      ),
    );
    setActionError(null);
    let res: Response;
    try {
      res = await fetch(
        `${API_BASE_URL}/api/occurrences/${occurrence.id}/start`,
        { method: 'POST', credentials: 'include' },
      );
    } catch (err) {
      setItems(snapshot);
      setActionError(err instanceof Error ? err.message : 'Start failed');
      return;
    }
    if (!res.ok) {
      setItems(snapshot);
      setActionError(await readError(res));
      return;
    }
    const data = (await res.json()) as OccurrenceActionResponse;
    setItems((prev) =>
      prev.map((entry) =>
        entry.id === item.id && entry.occurrence
          ? { ...entry, occurrence: data.occurrence }
          : entry,
      ),
    );
  };

  // ──────────────────────────────────────────
  // Add task (picker → POST /api/agenda/items)
  // ──────────────────────────────────────────

  const handlePickTask = async (task: TaskRecord): Promise<string | null> => {
    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/agenda/items`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        kind: 'task',
        ref_id: task.id,
        date,
      } satisfies NewAgendaItemInput),
    });
    if (!res.ok) return await readError(res);
    // The server appends at max+1 for the date — the sorted insert lands it
    // at the back of the pile.
    const data = (await res.json()) as AgendaItemResponse;
    setItems((prev) => sortItems([...prev, data.item]));
    return null;
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
    const res = await fetch(`${API_BASE_URL}/api/tasks/${taskModal.id}`, {
      method: 'PATCH',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        title: values.title,
        description: values.description,
        duration_minutes: values.durationMinutes,
        priority: values.priority,
        difficulty: values.difficulty,
      } satisfies UpdateTaskInput),
    });
    if (!res.ok) return await readError(res);
    // Merge the fresh embed straight into the rows (same refresh the
    // agenda reload would give, without the spinner flash).
    const data = (await res.json()) as TaskResponse;
    mergeTask(data.task);
    return null; // the modal closes itself on success
  };

  const handleTaskModalDelete = async (
    taskId: string,
  ): Promise<string | null> => {
    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/tasks/${taskId}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) return await readError(res);
    // The membership row is an orphan after the task dies — the next GET
    // omits it server-side; drop it locally now.
    setItems((prev) => prev.filter((entry) => entry.task?.id !== taskId));
    return null;
  };

  const handleTaskModalMove = async (
    taskId: string,
    status: TaskStatus,
  ): Promise<string | null> => {
    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/tasks/${taskId}/move`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status } satisfies MoveTaskInput),
    });
    if (!res.ok) return await readError(res);
    const data = (await res.json()) as MoveTaskResponse;
    mergeTask(data.task);
    // The modal's status pills read `task.status` from its props — keep the
    // edited copy fresh so the selection follows the server.
    setTaskModal((prev) => (prev && prev.id === taskId ? data.task : prev));
    return null;
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
    const res = await fetch(`${API_BASE_URL}/api/occurrences/${renaming.id}`, {
      method: 'PATCH',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      // Empty/whitespace clears the override — the day inherits again.
      body: JSON.stringify({ title: renameTitle.trim() }),
    });
    if (!res.ok) {
      setRenameSaving(false);
      setRenameError(await readError(res));
      return;
    }
    const data = (await res.json()) as OccurrenceResponse;
    setItems((prev) =>
      prev.map((entry) =>
        entry.occurrence && entry.occurrence.id === data.occurrence.id
          ? { ...entry, occurrence: data.occurrence }
          : entry,
      ),
    );
    setRenameSaving(false);
    setRenaming(null);
  };

  // ──────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────

  const dateLabel = agendaDateLabel(date, civilToday());
  const isToday = date === civilToday();
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
                onClick={() => changeDate(civilToday())}
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
                  onClick={() => {
                    setLoadError(null);
                    load();
                  }}
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

            {/* Loading — only while the list is empty (first load, a date
                switch, or a retry after a hard error) */}
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

            {/* The mixed pile — API order = sort_order */}
            {items.length > 0 && (
              <div className="space-y-2">
                {items.map((item, index) => (
                  <AgendaItemRow
                    key={item.id}
                    item={item}
                    isFirst={index === 0}
                    isLast={index === items.length - 1}
                    onMove={(itemId, direction) =>
                      void handleMove(itemId, direction)
                    }
                    onReschedule={(entry, targetDate) =>
                      void handleReschedule(entry, targetDate)
                    }
                    onCompleteTask={(entry) => void handleCompleteTask(entry)}
                    onStartTask={(entry) => void handleStartTask(entry)}
                    onRemoveTask={(entry) => void handleRemoveTask(entry)}
                    onOpenTask={(task) => setTaskModal(task)}
                    onCompleteOccurrence={(entry) =>
                      void setOccurrenceStatus(entry, 'done', 'complete')
                    }
                    onSkipOccurrence={(entry) =>
                      void setOccurrenceStatus(entry, 'skipped', 'skip')
                    }
                    onStartOccurrence={(entry) =>
                      void handleStartOccurrence(entry)
                    }
                    onRenameOccurrence={(occurrence) => openRename(occurrence)}
                    showStartOccurrence={isToday}
                  />
                ))}
              </div>
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

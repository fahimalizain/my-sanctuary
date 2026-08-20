import { useEffect, useRef, useState } from 'react';
import { Clock, Flag, Gauge, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { CategoryPicker } from './CategoryPicker';
import { DisplaceDialog } from './DisplaceDialog';
import {
  useTitleClassification,
  type ClassifyStatus,
} from '@/app/hooks/useTitleClassification';
import { buildClassifyUrl } from '@/app/hooks/classify-url';
import { cn } from '@/lib/utils';
import { API_BASE_URL } from '@/lib/api';
import {
  TASK_PRIORITIES,
  TASK_PRIORITY_LABELS,
  type CategoriesResponse,
  type Category,
  type ClassifyResponse,
  type MoveDisplaceInput,
  type TaskCategorySummary,
  type TaskDifficulty,
  type TaskList,
  type TaskListsResponse,
  type TaskPriority,
  type TaskRecord,
  type TaskStatus,
} from '@/app/types';

interface TaskFormValues {
  title: string;
  description: string;
  durationMinutes: number;
  priority: TaskPriority;
  difficulty: TaskDifficulty;
}

interface TaskModalProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Reserved — create is page-level now (no category anchor), so callers
   *  never pass this and the component does not use it. Kept on the type so
   *  code that still passes it compiles. The server files new tasks by
   *  title match. */
  category?: TaskCategorySummary;
  /** The task being edited (undefined = create). */
  task?: TaskRecord;
  /** Create-mode destination column. When set (and `task` is undefined),
   *  Status pills render locked to this value. Omitted = today's create
   *  (Lists page: no pills). */
  createStatus?: TaskStatus;
  /** Persists the form. Return an error message to show on the form (e.g.
   *  the server's "title does not match a category"), or null to close.
   *  `displace` is only passed by create into an OCCUPIED In Progress
   *  column, after the park dialog confirmed: the parent creates the task,
   *  then follows with ONE /move carrying the parked runner. */
  onSubmit: (
    values: TaskFormValues,
    displace?: MoveDisplaceInput,
  ) => Promise<string | null>;
  /** Deletes the task (edit mode only). Return an error or null to close. */
  onDelete?: (taskId: string) => Promise<string | null>;
  /** Immediate status change (edit only). Return an error string to show on
   *  the form, or null on success. `displace` is set when the user confirmed
   *  parking the current runner. sort_order is omitted — no drop: the server
   *  applies the column default. When omitted, Status renders read-only. */
  onMove?: (
    taskId: string,
    status: TaskStatus,
    displace?: MoveDisplaceInput,
  ) => Promise<string | null>;
  /** The task currently IN_PROGRESS, if any (may be this task). Used to
   *  decide whether tapping In Progress needs the park dialog. */
  runningTask?: TaskRecord | null;
}

const difficultyConfig: Record<TaskDifficulty, { label: string }> = {
  easy: { label: 'Easy' },
  medium: { label: 'Medium' },
  hard: { label: 'Hard' },
};

// The five board columns (ADR 0002 § Status model) — friendly labels, never
// the raw OPEN/COMPLETED values, in the user-facing pills.
const statusConfig: Record<TaskStatus, { label: string }> = {
  OPEN: { label: 'Backlog' },
  PLANNED: { label: 'Planned' },
  IN_PROGRESS: { label: 'In Progress' },
  COMPLETED: { label: 'Done' },
  DISCARDED: { label: 'Discarded' },
};

/** One-shot classify for Save: `GET /api/tasks/classify?title=&category_id=`.
 *  Returns null on any HTTP/network failure (callers surface their own
 *  message). */
async function classifyOnce(
  text: string,
  lock: string | null,
): Promise<ClassifyResponse | null> {
  try {
    const res = await fetch(`${API_BASE_URL}${buildClassifyUrl(text, lock)}`, {
      credentials: 'include',
    });
    if (!res.ok) return null;
    return (await res.json()) as ClassifyResponse;
  } catch {
    return null;
  }
}

export function TaskModal({
  open,
  onOpenChange,
  task,
  createStatus,
  onSubmit,
  onDelete,
  onMove,
  runningTask,
}: TaskModalProps) {
  const isEditing = !!task;
  // (Re)initialize the form when the dialog opens or the target task id
  // changes — never on a new `task` object with the same id, or a /move
  // merge (parent sets a fresh record with the new status) would clobber
  // unsaved edits. The status pills read `task.status` from props directly,
  // so they still follow the merged record.
  useEffect(() => {
    if (!open) {
      // Parent closed the modal — the park dialog must close with it even
      // when it was the one on screen (overlay close, ESC, parent unmount).
      setDisplaceOpen(false);
      setClassifyTitle(null);
      setLockId(null);
      lastClassifyRef.current = null;
      return;
    }
    // The visible hole: create starts empty; edit opens with the computed
    // `display_title` (the split-off hole), never the stored full title.
    setTitle(task?.display_title || '');
    setDescription(task?.description || '');
    setPriority(task?.priority || 'medium');
    setDifficulty(task?.difficulty || 'easy');
    setDuration(String(task?.duration_minutes || 15));
    setFormError(null);
    setDisplaceOpen(false);
    if (task) {
      // A tracked edit locks to its computed category; untracked has no lock.
      const lock = task.category.is_untracked ? null : task.category.id;
      setLockId(lock);
      // Edit-open identity (critical): the FIRST classify sends the stored
      // FULL `task.title`, not the hole — classifying the hole would fill it
      // and can canonicalize the affixes (e.g. "… | SpicyHome" → "Spicy Home").
      // `lastClassifyRef.input` marks the current input at classify time so
      // Save knows the untouched seed corresponds to this classify.
      lastClassifyRef.current = { input: task.display_title, lock };
      setClassifyTitle(task.title);
    } else {
      setLockId(null);
      lastClassifyRef.current = null;
      setClassifyTitle(null);
    }
  }, [open, task?.id]);

  // The modal owns its picker data: it fetches lists + categories on open
  // (ListsPage does not load categories today), so callers need no new props.
  const [lists, setLists] = useState<TaskList[]>([]);
  const [categories, setCategories] = useState<Category[]>([]);
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLists([]);
    setCategories([]);
    void Promise.all([
      fetch(`${API_BASE_URL}/api/lists`, { credentials: 'include' }),
      fetch(`${API_BASE_URL}/api/categories`, { credentials: 'include' }),
    ])
      .then(async ([listsRes, catsRes]) => {
        if (cancelled) return;
        const listsData = listsRes.ok
          ? ((await listsRes.json()) as TaskListsResponse)
          : null;
        const catsData = catsRes.ok
          ? ((await catsRes.json()) as CategoriesResponse)
          : null;
        setLists(listsData?.lists ?? []);
        setCategories(catsData?.categories ?? []);
      })
      .catch(() => {
        if (!cancelled) {
          setLists([]);
          setCategories([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [priority, setPriority] = useState<TaskPriority>('medium');
  const [difficulty, setDifficulty] = useState<TaskDifficulty>('easy');
  const [duration, setDuration] = useState('15');
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // A /move is in flight — pills and Save/Delete are disabled.
  const [movingStatus, setMovingStatus] = useState(false);
  // Occupied In Progress: the park dialog is open, move not yet dispatched.
  const [displaceOpen, setDisplaceOpen] = useState(false);

  // The category lock: an explicit user pick. Passed as `category_id` to
  // classify, so a locked title persists its affixes (e.g. "Work | SpicyHome")
  // and a locked empty title is a legal classify. `null` = unlocked: classify
  // runs title-only.
  const [lockId, setLockId] = useState<string | null>(null);
  // The title snapshot the classify-hook fires on: set on blur (the hole),
  // on edit-open (the stored full title), and on lock change (the hole).
  const [classifyTitle, setClassifyTitle] = useState<string | null>(null);
  // What the latest classify was sent with — Save re-classifies when the
  // current input+lock no longer matches it.
  const lastClassifyRef = useRef<{ input: string; lock: string | null } | null>(
    null,
  );

  // Blur-preview of where the title will be filed (GET /api/tasks/classify).
  // Advisory only — the server stays the authority on Save. resetKey changes
  // on open/close and task.id, so the hook self-resets there. Only the
  // explicit lock is sent — autofill (a classify match without a lock) must
  // NOT send `category_id`, or it would lock the task and freeze the chrome.
  const classifyStatus: ClassifyStatus = useTitleClassification({
    title,
    classifyTitle,
    categoryId: lockId,
    initialTitle: isEditing ? task!.title : undefined,
    active: open,
    resetKey: `${open}:${task?.id ?? 'new'}`,
  });

  // Snap-to-hole: after an unlocked classify match carries chrome, collapse
  // the input's full typed string down to the server's `display_title` (e.g.
  // "S | SpicyHome" → "S") — otherwise the chrome paints "| SpicyHome" while
  // the input still holds the full string, and the user sees the suffix twice.
  // Locking the match keeps the result on screen: an unlocked re-classify of
  // just the hole would miss and wipe the chrome. `classifyTitle` stays the
  // full blurred string — the lock change re-fires classify with it, which
  // resolves as identity, so the persisted spelling is preserved as typed.
  // Only snap while the visible title is still the persist string — never
  // after the user typed past the result, and never when there is no chrome.
  useEffect(() => {
    if (
      classifyStatus.state !== 'matched' ||
      lockId !== null ||
      (classifyStatus.prefix === '' && classifyStatus.suffix === '') ||
      title.trim() !== classifyStatus.persistTitle.trim() ||
      title.trim() === classifyStatus.displayTitle.trim()
    ) {
      return;
    }
    const snapped = classifyStatus.displayTitle;
    const lock = classifyStatus.category.id;
    setTitle(snapped);
    setLockId(lock);
    lastClassifyRef.current = { input: snapped, lock };
  }, [classifyStatus, lockId, title]);

  // What the picker shows vs. what the API gets: the explicit lock always
  // wins; when unlocked (picker "No category"), a unique classify match still
  // shows in the combobox as a type-first autofill. It is never sent as
  // `category_id` (only `lockId` is).
  const pickerSelectedId =
    lockId ??
    (classifyStatus.state === 'matched' ? classifyStatus.category.id : null);

  /** The chrome (prefix/suffix) around the hole from the latest classify.
   *  Loading/idle carry none; matched/nomatch/conflict all carry the affixes
   *  the server reported (a locked category previews them even on nomatch). */
  const chrome: { prefix: string; suffix: string } =
    classifyStatus.state === 'matched'
      ? classifyStatus
      : classifyStatus.state === 'nomatch'
        ? classifyStatus
        : classifyStatus.state === 'conflict'
          ? classifyStatus
          : { prefix: '', suffix: '' };

  // The "Files to ● X" hint target: a lock always names its category; an
  // unlocked match names the classify result.
  const lockedCategory = lockId
    ? (categories.find((entry) => entry.id === lockId) ?? null)
    : null;
  const hintCategory =
    lockedCategory ??
    (classifyStatus.state === 'matched' ? classifyStatus.category : null);
  // A changed lock in edit mode reads as a refile (the target differs from
  // the stored one); otherwise it files into the target.
  const willRefile =
    isEditing && hintCategory !== null && hintCategory.id !== task!.category.id;

  /** Persists the form via `onSubmit` — shared by the Save button and the
   *  create-mode park-dialog confirm (which passes `displace`). */
  const submitForm = async (displace?: MoveDisplaceInput) => {
    setSaving(true);
    setFormError(null);

    // Resolve the title to persist: use the latest classify's persist_title
    // when it corresponds to the current input+lock AND — with a lock set —
    // the matched result is for that same lock; otherwise await one classify
    // and POST its persist_title. Never send the raw hole. The lock check is
    // the real fix: selecting a category only fires a classify inside an
    // effect (after paint), so until it settles the cached result is stale
    // for the new lock — reusing it would persist the PREVIOUS lock's title
    // (e.g. "Work" instead of "Work | SpicyHome").
    const last = lastClassifyRef.current;
    const corresponds =
      last !== null &&
      last.lock === lockId &&
      last.input.trim() === title.trim();
    let persistTitle: string;
    if (
      classifyStatus.state === 'matched' &&
      corresponds &&
      (lockId === null || classifyStatus.category.id === lockId)
    ) {
      persistTitle = classifyStatus.persistTitle;
    } else {
      // The user typed (or changed the lock) since the last classify, or the
      // state is idle (create without a blur, or a degraded preview) — verify
      // right before persisting.
      const fresh = await classifyOnce(title, lockId);
      if (fresh && 'Matched' in fresh) {
        persistTitle = fresh.Matched.persist_title;
      } else if (fresh && 'Untracked' in fresh && fresh.Untracked.conflict) {
        setSaving(false);
        setFormError(
          `Title matches ${fresh.Untracked.categories
            .map((entry) => entry.title)
            .join(', ')
            .replace(/, ([^,]*)$/, ' and $1')} — be more specific`,
        );
        return;
      } else {
        setSaving(false);
        setFormError(
          'Title does not match a category — pick a category or retitle',
        );
        return;
      }
    }

    const error = await onSubmit(
      {
        title: persistTitle,
        description,
        durationMinutes: parseInt(duration, 10) || 15,
        priority,
        difficulty,
      },
      displace,
    );
    setSaving(false);
    if (error) {
      setFormError(error);
      return;
    }
    onOpenChange(false);
  };

  const handleSave = async () => {
    // Create into an OCCUPIED In Progress column: park the runner BEFORE any
    // write — no orphan Backlog card is ever created just to discover the
    // slot is taken. Cancel stays on the form (nothing created); confirm
    // re-enters this path with `displace` via submitForm. The Save button is
    // disabled on an empty title, so the dialog only opens with a real one.
    if (!isEditing && createStatus === 'IN_PROGRESS' && runningTask) {
      setDisplaceOpen(true);
      return;
    }
    await submitForm();
  };

  /** Picker change: the picked category becomes the lock and fires a classify
   *  of the CURRENT hole (empty input + lock is allowed). `null` unlocks. */
  const handleSelectCategory = (id: string | null) => {
    lastClassifyRef.current = { input: title, lock: id };
    setClassifyTitle(title);
    setLockId(id);
  };

  // Track the input at blur so Save can tell a stale classify apart.
  const handleTitleBlur = () => {
    lastClassifyRef.current = { input: title, lock: lockId };
    setClassifyTitle(title);
  };

  const handleDelete = async () => {
    if (!task) return;
    setSaving(true);
    setFormError(null);
    const error = (await onDelete?.(task.id)) ?? null;
    setSaving(false);
    if (error) {
      setFormError(error);
      return;
    }
    onOpenChange(false);
  };

  /** Runs an immediate status change (edit only, like a drop — Save never
   *  carries status). The parent merges the response into `task` so the
   *  selected pill follows the server; on error the message lands in
   *  `formError` and the modal stays open. */
  const runMove = async (dest: TaskStatus, displace?: MoveDisplaceInput) => {
    if (!task || !onMove) return;
    setMovingStatus(true);
    setFormError(null);
    const error = await onMove(task.id, dest, displace);
    setMovingStatus(false);
    if (error) setFormError(error);
  };

  const handleStatusClick = (dest: TaskStatus) => {
    if (!task || !onMove || movingStatus) return;
    if (task.status === dest) return; // tapping the current status is a no-op
    // The running slot is occupied by a DIFFERENT task: park it first (ADR
    // 0002 § UI). Cancel = nothing; confirm dispatches one /move with
    // `displace`.
    if (dest === 'IN_PROGRESS' && runningTask && runningTask.id !== task.id) {
      setDisplaceOpen(true);
      return;
    }
    void runMove(dest);
  };

  const handleDisplaceConfirm = (
    parkStatus: 'PLANNED' | 'COMPLETED' | 'DISCARDED',
  ) => {
    if (!runningTask) return;
    setDisplaceOpen(false);
    // Create mode: the park dialog opened BEFORE the create write
    // (handleSave intercepted). Confirming now runs the same submit path as
    // Save, with the displace attached — the parent creates then moves in
    // one flow, so nothing was written while the dialog was open.
    if (!task && createStatus === 'IN_PROGRESS') {
      void submitForm({
        id: runningTask.id,
        status: parkStatus,
      });
      return;
    }
    // Edit mode: immediate status change with displace (existing behavior).
    if (!task) return;
    void runMove('IN_PROGRESS', {
      id: runningTask.id,
      status: parkStatus,
    });
  };

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-[480px] bg-card border-border">
          {/* Header accent bar — edit mode uses the task's computed category
            color; create has no category anchor (no color is fine) */}
          <div
            className="h-2 shrink-0"
            style={{
              backgroundColor: isEditing ? task!.category.color : undefined,
            }}
          />

          <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
            <DialogTitle className="text-foreground">
              {isEditing ? 'Edit Task' : 'New Task'}
            </DialogTitle>
            <DialogDescription>
              {isEditing
                ? 'Update the details of your task below.'
                : 'Pick a category or type a title that matches one.'}
            </DialogDescription>
          </DialogHeader>
          <hr className="shrink-0 border-border" />

          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 pb-4">
            {/* Task Title — one field that reads [chrome prefix][hole][chrome
                suffix]. The category lock picker lives BELOW it (after the
                classify hints), so the title reads first. */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Task Name
              </label>
              <div className="flex items-center w-full rounded-xl border border-input bg-background text-foreground transition-all focus-within:ring-2 focus-within:ring-primary/20 focus-within:border-primary">
                {chrome.prefix !== '' && (
                  <span className="select-none pl-4 py-3 text-muted-foreground whitespace-nowrap">
                    {chrome.prefix}
                  </span>
                )}
                <input
                  type="text"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  onBlur={handleTitleBlur}
                  placeholder={
                    isEditing ? 'What needs to be done?' : `e.g. Review Q3`
                  }
                  className={cn(
                    'flex-1 min-w-0 bg-transparent py-3 focus:outline-none',
                    chrome.prefix !== '' ? 'pl-1' : 'pl-4',
                    chrome.suffix !== '' ? 'pr-1' : 'pr-4',
                  )}
                />
                {chrome.suffix !== '' && (
                  <span className="select-none pr-4 py-3 text-muted-foreground whitespace-nowrap">
                    {chrome.suffix}
                  </span>
                )}
              </div>

              {/* Classification preview — fires on blur / lock change,
                advisory only. The lock's target is shown even before the
                first classify resolves. */}
              {classifyStatus.state === 'loading' && (
                <p className="text-xs text-muted-foreground flex items-center gap-1.5">
                  <Loader2 className="h-3.5 w-3.5 animate-spin inline text-muted-foreground" />
                  Checking…
                </p>
              )}
              {classifyStatus.state === 'matched' && hintCategory && (
                <p
                  className={cn(
                    'text-xs flex items-center gap-1.5',
                    willRefile ? 'text-foreground' : 'text-muted-foreground',
                  )}
                >
                  <span
                    className="inline-block h-2.5 w-2.5 rounded-full align-middle"
                    style={{ backgroundColor: hintCategory.color }}
                  />
                  {willRefile
                    ? `Will refile to ${hintCategory.title}`
                    : `Files to ${hintCategory.title}`}
                </p>
              )}
              {/* A lock always has a target — show it while the preview is
                  idle (before the first classify settles). */}
              {lockId !== null &&
                lockedCategory !== null &&
                classifyStatus.state === 'idle' && (
                  <p className="text-xs text-muted-foreground flex items-center gap-1.5">
                    <span
                      className="inline-block h-2.5 w-2.5 rounded-full align-middle"
                      style={{ backgroundColor: lockedCategory.color }}
                    />
                    {isEditing && lockedCategory.id !== task!.category.id
                      ? `Will refile to ${lockedCategory.title}`
                      : `Files to ${lockedCategory.title}`}
                  </p>
                )}
              {classifyStatus.state === 'nomatch' && (
                <p className="text-xs text-destructive">
                  No category matches — Save will fail
                </p>
              )}
              {classifyStatus.state === 'conflict' && (
                <p className="text-xs text-destructive">
                  Matches{' '}
                  {classifyStatus.categories
                    .map((entry) => entry.title)
                    .join(', ')
                    .replace(/, ([^,]*)$/, ' and $1')}{' '}
                  — be more specific
                </p>
              )}
              {classifyStatus.state === 'idle' && !isEditing && (
                <p className="text-xs text-muted-foreground">
                  Pick a category or type a title that matches one.
                </p>
              )}
              {classifyStatus.state === 'idle' &&
                isEditing &&
                task!.category.is_untracked && (
                  <p className="text-xs text-muted-foreground">
                    Retitle to match a living category to save. Move and delete
                    still work.
                  </p>
                )}
            </div>

            {/* Category — the lock picker sits after the title + its classify
                hints, before Description. `selectedId` is the picker view
                (lock, or an unlocked classify match shown as autofill); only
                an explicit lock (`hasLock`) gets the clear X. */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Category
              </label>
              <CategoryPicker
                lists={lists}
                categories={categories}
                selectedId={pickerSelectedId}
                hasLock={lockId !== null}
                onSelect={handleSelectCategory}
              />
            </div>

            {/* Description */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Description{' '}
                <span className="text-muted-foreground/60 font-normal">
                  (optional)
                </span>
              </label>
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder="Add more details about this task..."
                rows={3}
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all resize-none"
              />
            </div>

            {/* Priority Selection */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground flex items-center gap-2">
                <Flag className="h-4 w-4 text-muted-foreground" />
                Priority
              </label>
              <div className="flex gap-2">
                {TASK_PRIORITIES.map((p) => (
                  <button
                    key={p}
                    type="button"
                    onClick={() => setPriority(p)}
                    className={cn(
                      'flex items-center gap-2 px-4 py-2 rounded-xl border-2 text-sm font-medium transition-all',
                      priority === p
                        ? 'bg-primary/10 border-primary text-primary'
                        : 'bg-background border-input text-muted-foreground hover:border-primary/30',
                    )}
                  >
                    {TASK_PRIORITY_LABELS[p]}
                  </button>
                ))}
              </div>
            </div>

            {/* Difficulty Selection */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground flex items-center gap-2">
                <Gauge className="h-4 w-4 text-muted-foreground" />
                Difficulty
              </label>
              <div className="flex gap-2">
                {(Object.keys(difficultyConfig) as TaskDifficulty[]).map(
                  (d) => (
                    <button
                      key={d}
                      type="button"
                      onClick={() => setDifficulty(d)}
                      className={cn(
                        'flex items-center gap-2 px-4 py-2 rounded-xl border-2 text-sm font-medium transition-all',
                        difficulty === d
                          ? 'bg-primary/10 border-primary text-primary'
                          : 'bg-background border-input text-muted-foreground hover:border-primary/30',
                      )}
                    >
                      {difficultyConfig[d].label}
                    </button>
                  ),
                )}
              </div>
            </div>

            {/* Duration — own row; five status pills will not fit beside it on
              a phone */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground flex items-center gap-2">
                <Clock className="h-4 w-4 text-muted-foreground" />
                Duration
              </label>
              <div className="relative">
                <input
                  type="number"
                  value={duration}
                  onChange={(e) => setDuration(e.target.value)}
                  min="1"
                  step="5"
                  className="w-full px-4 py-3 pr-12 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
                <span className="absolute right-4 top-1/2 -translate-y-1/2 text-sm text-muted-foreground">
                  min
                </span>
              </div>
            </div>

            {/* Status — edit mode: with onMove, immediate column pills (a drop
              with no drop position: `sort_order` is omitted and the server
              applies the column default); without it, keep the old read-only
              box so hypothetical callers stay intact. Create mode: when
              `createStatus` is set (board column +), the same five pills
              render locked to that destination — changing column means
              Cancel and tapping another +. Omitted (Lists page) means no
              Status section at all. */}
            {isEditing ? (
              onMove ? (
                <div className="space-y-2 mb-6">
                  <label className="text-sm font-medium text-foreground flex items-center gap-2">
                    Status
                    {movingStatus && (
                      <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
                    )}
                  </label>
                  <div className="flex flex-wrap gap-2">
                    {(Object.keys(statusConfig) as TaskStatus[]).map(
                      (status) => (
                        <button
                          key={status}
                          type="button"
                          onClick={() => handleStatusClick(status)}
                          disabled={movingStatus}
                          className={cn(
                            'flex items-center gap-2 px-4 py-2 rounded-xl border-2 text-sm font-medium transition-all disabled:cursor-not-allowed disabled:opacity-50',
                            task!.status === status
                              ? 'bg-primary/10 border-primary text-primary'
                              : 'bg-background border-input text-muted-foreground hover:border-primary/30',
                          )}
                        >
                          {statusConfig[status].label}
                        </button>
                      ),
                    )}
                  </div>
                </div>
              ) : (
                <div className="space-y-2 mb-6">
                  <label className="text-sm font-medium text-foreground">
                    Status
                  </label>
                  <div className="w-full px-4 py-3 rounded-xl border border-input bg-muted/40 text-sm font-medium text-muted-foreground">
                    {task!.status}
                  </div>
                </div>
              )
            ) : createStatus ? (
              <div className="space-y-2 mb-6">
                <label className="text-sm font-medium text-foreground flex items-center gap-2">
                  Status
                </label>
                <div className="flex flex-wrap gap-2">
                  {(Object.keys(statusConfig) as TaskStatus[]).map((status) => (
                    <button
                      key={status}
                      type="button"
                      disabled
                      aria-label={`New task lands in ${statusConfig[status].label}`}
                      className={cn(
                        'flex items-center gap-2 px-4 py-2 rounded-xl border-2 text-sm font-medium transition-all disabled:cursor-not-allowed',
                        createStatus === status
                          ? 'bg-primary/10 border-primary text-primary'
                          : 'bg-background border-input text-muted-foreground',
                      )}
                    >
                      {statusConfig[status].label}
                    </button>
                  ))}
                </div>
              </div>
            ) : null}

            {/* Category — the lock picker above owns the choice now; the old
                static edit-only tag is gone in favor of it. */}

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}
          </div>

          <div className="flex shrink-0 items-center justify-between border-t border-border px-6 py-4">
            {isEditing ? (
              <Button
                variant="ghost"
                onClick={handleDelete}
                disabled={saving || movingStatus}
                className="text-destructive hover:text-destructive hover:bg-destructive/10"
              >
                Delete
              </Button>
            ) : (
              <div />
            )}
            <div className="flex gap-3">
              <Button
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={saving}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={handleSave}
                disabled={
                  saving ||
                  movingStatus ||
                  !title.trim() ||
                  classifyStatus.state === 'loading' ||
                  classifyStatus.state === 'nomatch' ||
                  classifyStatus.state === 'conflict' ||
                  (isEditing &&
                    task!.category.is_untracked &&
                    classifyStatus.state !== 'matched')
                }
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {isEditing ? 'Save Changes' : 'Create Task'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>

      {/* Occupied In Progress park dialog — same component as the board's
        drag conflict; Cancel / overlay close dispatches nothing */}
      <DisplaceDialog
        open={displaceOpen}
        onOpenChange={(open) => {
          if (!open) setDisplaceOpen(false);
        }}
        runningTitle={runningTask?.title ?? ''}
        incomingTitle={title.trim() || 'this task'}
        onConfirm={handleDisplaceConfirm}
      />
    </>
  );
}

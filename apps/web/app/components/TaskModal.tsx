import { useEffect, useState } from 'react';
import { Clock, Flag, Gauge, Loader2, Tag } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { DisplaceDialog } from './DisplaceDialog';
import { cn } from '@/lib/utils';
import type {
  MoveDisplaceInput,
  TaskCategorySummary,
  TaskDifficulty,
  TaskPriority,
  TaskRecord,
  TaskStatus,
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
   *  the server's "title does not match a category"), or null to close. */
  onSubmit: (values: TaskFormValues) => Promise<string | null>;
  /** Deletes the task (edit mode only). Return an error or null to close. */
  onDelete?: (taskId: string) => Promise<string | null>;
  /** Immediate status change (edit only). Return an error string to show on
   *  the form, or null on success. `displace` is set when the user confirmed
   *  parking the current runner. sort_order is always 0 (prepend — no drop).
   *  When omitted, Status renders read-only. */
  onMove?: (
    taskId: string,
    status: TaskStatus,
    displace?: MoveDisplaceInput,
  ) => Promise<string | null>;
  /** The task currently IN_PROGRESS, if any (may be this task). Used to
   *  decide whether tapping In Progress needs the park dialog. */
  runningTask?: TaskRecord | null;
}

const priorityConfig: Record<TaskPriority, { label: string }> = {
  low: { label: 'Low' },
  medium: { label: 'Medium' },
  high: { label: 'High' },
};

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
    if (!open) return;
    setTitle(task?.title || '');
    setDescription(task?.description || '');
    setPriority(task?.priority || 'medium');
    setDifficulty(task?.difficulty || 'easy');
    setDuration(String(task?.duration_minutes || 15));
    setFormError(null);
    setDisplaceOpen(false);
  }, [open, task?.id]);

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

  const handleSave = async () => {
    setSaving(true);
    setFormError(null);
    const error = await onSubmit({
      title,
      description,
      durationMinutes: parseInt(duration, 10) || 15,
      priority,
      difficulty,
    });
    setSaving(false);
    if (error) {
      setFormError(error);
      return;
    }
    onOpenChange(false);
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
    if (!task || !runningTask) return;
    setDisplaceOpen(false);
    void runMove('IN_PROGRESS', {
      id: runningTask.id,
      status: parkStatus,
      sort_order: 0,
    });
  };

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="sm:max-w-[480px] p-0 gap-0 overflow-hidden bg-card border-border">
          {/* Header accent bar — edit mode uses the task's computed category
            color; create has no category anchor (no color is fine) */}
          <div
            className="h-2"
            style={{
              backgroundColor: isEditing ? task!.category.color : undefined,
            }}
          />

          <div className="p-6">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">
                {isEditing ? 'Edit Task' : 'New Task'}
              </DialogTitle>
              <DialogDescription>
                {isEditing
                  ? 'Update the details of your task below.'
                  : 'Tasks are filed by their title — type a title that matches a category.'}
              </DialogDescription>
            </DialogHeader>

            {/* Task Title */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Task Name
              </label>
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder={
                  isEditing
                    ? 'What needs to be done?'
                    : `e.g. Work or Review | Work`
                }
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              {!isEditing && (
                <p className="text-xs text-muted-foreground">
                  The title must match exactly one category — the server
                  explains if it does not.
                </p>
              )}
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
                {(Object.keys(priorityConfig) as TaskPriority[]).map((p) => (
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
                    {priorityConfig[p].label}
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
              with no drop position: always prepend); without it, keep the
              old read-only box so hypothetical callers stay intact. Create
              mode: when `createStatus` is set (board column +), the same
              five pills render locked to that destination — changing column
              means Cancel and tapping another +. Omitted (Lists page) means
              no Status section at all. */}
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

            {/* Category tag — edit mode only: the computed filing result of an
              existing task. Create is page-level and files by title match, so
              there is no category to show here. */}
            {isEditing && (
              <div className="flex items-center gap-2 mb-6 p-3 bg-muted rounded-xl">
                <Tag className="h-4 w-4 text-primary" />
                <span className="text-sm text-foreground">Category:</span>
                <span className="text-sm font-medium text-primary">
                  {task!.category.title}
                </span>
                {task!.category.is_untracked && (
                  <span className="text-xs text-muted-foreground">
                    (untracked)
                  </span>
                )}
              </div>
            )}

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}

            {/* Actions */}
            <div className="flex items-center justify-between pt-4 border-t border-border">
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
                  disabled={!title.trim() || saving || movingStatus}
                  className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                >
                  {isEditing ? 'Save Changes' : 'Create Task'}
                </Button>
              </div>
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
        incomingTitle={task?.title ?? ''}
        onConfirm={handleDisplaceConfirm}
      />
    </>
  );
}

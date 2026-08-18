import { useEffect, useState } from 'react';
import { Clock, Flag, Tag } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { cn } from '@/lib/utils';
import type {
  TaskCategorySummary,
  TaskPriority,
  TaskRecord,
} from '@/app/types';

interface TaskFormValues {
  title: string;
  description: string;
  durationMinutes: number;
  priority: TaskPriority;
}

interface TaskModalProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The category anchor for a NEW task — its title appears in the hint.
   *  The server still decides the final match (any category may win). */
  category?: TaskCategorySummary;
  /** The task being edited (undefined = create). */
  task?: TaskRecord;
  /** Persists the form. Return an error message to show on the form (e.g.
   *  the server's "title does not match a category"), or null to close. */
  onSubmit: (values: TaskFormValues) => Promise<string | null>;
  /** Deletes the task (edit mode only). Return an error or null to close. */
  onDelete?: (taskId: string) => Promise<string | null>;
}

const priorityConfig: Record<TaskPriority, { label: string }> = {
  low: { label: 'Low' },
  medium: { label: 'Medium' },
  high: { label: 'High' },
};

export function TaskModal({
  open,
  onOpenChange,
  category,
  task,
  onSubmit,
  onDelete,
}: TaskModalProps) {
  const isEditing = !!task;
  // (Re)initialize the form whenever the dialog opens for a different target;
  // state is set here rather than in useState initializers so switching
  // between tasks reopens with the right values.
  useEffect(() => {
    if (!open) return;
    setTitle(task?.title || '');
    setDescription(task?.description || '');
    setPriority(task?.priority || 'medium');
    setDuration(String(task?.duration_minutes || 15));
    setFormError(null);
  }, [open, task]);

  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [priority, setPriority] = useState<TaskPriority>('medium');
  const [duration, setDuration] = useState('15');
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const handleSave = async () => {
    setSaving(true);
    setFormError(null);
    const error = await onSubmit({
      title,
      description,
      durationMinutes: parseInt(duration, 10) || 15,
      priority,
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

  // Hint for create mode: the anchor category that opened the dialog, plus
  // the pipe-suffix convention of the seeded patterns.
  const hintCategory = isEditing ? task!.category : category;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[480px] p-0 gap-0 overflow-hidden bg-card border-border">
        {/* Header accent bar */}
        <div
          className="h-2"
          style={{ backgroundColor: hintCategory?.color || undefined }}
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
                The title must match exactly one category — the server explains
                if it does not.
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

          {/* Duration and Status Row */}
          <div className="grid grid-cols-2 gap-4 mb-6">
            {/* Duration */}
            <div className="space-y-2">
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

            {/* Read-only status (this slice never transitions tasks) */}
            {isEditing && (
              <div className="space-y-2">
                <label className="text-sm font-medium text-foreground">
                  Status
                </label>
                <div className="w-full px-4 py-3 rounded-xl border border-input bg-muted/40 text-sm font-medium text-muted-foreground">
                  {task!.status}
                </div>
              </div>
            )}
          </div>

          {/* Category tag — computed by the server from the title */}
          <div className="flex items-center gap-2 mb-6 p-3 bg-muted rounded-xl">
            <Tag className="h-4 w-4 text-primary" />
            <span className="text-sm text-foreground">
              {isEditing ? 'Category:' : 'Files into category:'}
            </span>
            <span className="text-sm font-medium text-primary">
              {hintCategory ? hintCategory.title : '—'}
            </span>
            {hintCategory?.is_untracked && (
              <span className="text-xs text-muted-foreground">(untracked)</span>
            )}
          </div>

          {formError && (
            <p className="mb-4 text-sm text-destructive">{formError}</p>
          )}

          {/* Actions */}
          <div className="flex items-center justify-between pt-4 border-t border-border">
            {isEditing ? (
              <Button
                variant="ghost"
                onClick={handleDelete}
                disabled={saving}
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
                disabled={!title.trim() || saving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {isEditing ? 'Save Changes' : 'Create Task'}
              </Button>
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

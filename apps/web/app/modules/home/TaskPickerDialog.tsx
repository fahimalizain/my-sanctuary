// The add-task picker for Home (ADR 0004 § Surfaces — Home): a Dialog that
// lists living OPEN/PLANNED/IN_PROGRESS tasks NOT already on the selected
// date, with a free-text search over title/display title. Picking hands the
// task to HomePage, which POSTs the agenda membership row. No create-task
// here (ADR 0004 § Out of scope — new finite work is TaskModal on the Board).

import { useEffect, useState } from 'react';
import { Loader2, Search } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { useListsQuery } from '@/app/queries/lists';
import { useTasksQuery } from '@/app/queries/tasks';
import { cn } from '@/lib/utils';
import type { TaskRecord } from '../../types';
import { TASK_PRIORITY_LABELS } from '../../types';
import { agendaTaskMatches, filterAgendaPickerTasks } from './agenda-helpers';

interface TaskPickerDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Task ids already on the selected date (the picker hides them). */
  excludedTaskIds: Set<string>;
  /** Persists the pick (POST /api/agenda/items). Return an error message to
   *  show inside the dialog, or null to close it. */
  onPick: (task: TaskRecord) => Promise<string | null>;
}

export function TaskPickerDialog({
  open,
  onOpenChange,
  excludedTaskIds,
  onPick,
}: TaskPickerDialogProps) {
  const [query, setQuery] = useState('');
  const [pickingId, setPickingId] = useState<string | null>(null);
  const [pickError, setPickError] = useState<string | null>(null);

  // Shared queries (slice 6): tasks come from the one `['tasks']` cache the
  // Board and Lists write, so moves made on the board are already visible —
  // no refetch on every open (the 30s staleTime plus the shared cache
  // replace the old "board may have moved" refetch). Lists stay seed-gated:
  // GET /api/lists performs the first-visit seed, so tasks fetch after it.
  const listsQuery = useListsQuery({ enabled: open });
  const tasksQuery = useTasksQuery({ enabled: open && listsQuery.isSuccess });
  const tasks = tasksQuery.data?.tasks ?? [];
  const isLoading =
    listsQuery.isLoading || (listsQuery.isSuccess && tasksQuery.isLoading);
  const loadError =
    (listsQuery.error instanceof Error
      ? listsQuery.error.message
      : listsQuery.error
        ? 'Failed to load tasks'
        : null) ??
    (tasksQuery.error instanceof Error
      ? tasksQuery.error.message
      : tasksQuery.error
        ? 'Failed to load tasks'
        : null);

  // Local UX reset on open: the search box and the pick error start fresh
  // every time the dialog opens. The shared tasks cache is deliberately NOT
  // cleared — that is the point of the shared cache.
  useEffect(() => {
    if (open) {
      setQuery('');
      setPickError(null);
    }
  }, [open]);

  const pickable = filterAgendaPickerTasks(tasks, excludedTaskIds);
  const visible = pickable.filter((task) => agendaTaskMatches(task, query));

  const handlePick = async (task: TaskRecord) => {
    setPickingId(task.id);
    setPickError(null);
    const error = await onPick(task);
    setPickingId(null);
    if (error) {
      setPickError(error);
      return;
    }
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-[480px] bg-card border-border">
        <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
          <DialogTitle className="text-foreground">Add a task</DialogTitle>
          <DialogDescription>
            Pick from living tasks — they land on this day only; the Board keeps
            them.
          </DialogDescription>
        </DialogHeader>
        <hr className="shrink-0 border-border" />

        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 pb-4">
          {/* Search */}
          <div className="relative mb-4 mt-4">
            <Search className="absolute left-3.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search tasks…"
              className="w-full pl-10 pr-4 py-2.5 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
            />
          </div>

          {loadError && (
            <div className="mb-4 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-xl px-4 py-3">
              <p className="text-sm">{loadError}</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  // Lists first — tasks are seed-gated on lists success and
                  // re-run automatically; when lists are already loaded,
                  // refetch tasks too.
                  void listsQuery.refetch();
                  if (listsQuery.isSuccess) void tasksQuery.refetch();
                }}
              >
                Retry
              </Button>
            </div>
          )}

          {isLoading && (
            <div className="flex items-center justify-center py-16 gap-2 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin" />
              Loading tasks…
            </div>
          )}

          {!isLoading && !loadError && tasks.length === 0 && (
            <p className="py-16 text-center text-sm text-muted-foreground">
              No tasks yet — create one on the Board, then add it here.
            </p>
          )}

          {!isLoading &&
            !loadError &&
            tasks.length > 0 &&
            visible.length === 0 && (
              <p className="py-16 text-center text-sm text-muted-foreground">
                {pickable.length === 0
                  ? 'Everything living is already on this day.'
                  : 'No tasks match that search.'}
              </p>
            )}

          {visible.length > 0 && (
            <div className="space-y-2">
              {visible.map((task) => (
                <button
                  key={task.id}
                  type="button"
                  onClick={() => void handlePick(task)}
                  disabled={pickingId !== null}
                  className={cn(
                    'w-full flex items-center gap-2.5 rounded-lg border border-border bg-background p-3 text-left transition-colors',
                    'hover:border-primary/40 disabled:opacity-60 disabled:cursor-not-allowed',
                  )}
                >
                  <span
                    className="h-2.5 w-2.5 rounded-full flex-shrink-0"
                    style={{ backgroundColor: task.category.color }}
                  />
                  <span className="flex-1 min-w-0">
                    <span className="block truncate text-sm font-medium text-foreground">
                      {task.display_title}
                    </span>
                    <span className="block text-xs text-muted-foreground">
                      {TASK_PRIORITY_LABELS[task.priority]} ·{' '}
                      {task.duration_minutes} min · {task.status}
                    </span>
                  </span>
                  {pickingId === task.id && (
                    <Loader2 className="h-4 w-4 animate-spin text-muted-foreground flex-shrink-0" />
                  )}
                </button>
              ))}
            </div>
          )}

          {pickError && (
            <p className="mt-4 text-sm text-destructive">{pickError}</p>
          )}
        </div>

        <div className="flex shrink-0 justify-end gap-3 border-t border-border px-6 py-4">
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={pickingId !== null}
            className="border-input text-foreground hover:bg-muted"
          >
            Cancel
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

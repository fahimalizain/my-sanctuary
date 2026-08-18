import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, MoreHorizontal, Pencil, Plus, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { API_BASE_URL } from '@/lib/api';
import type { TaskList, TaskListsResponse } from '@/app/types';

// The server's error envelope is `{"error": "message"}`; fall back to a
// generic message when the body is not JSON.
async function readError(res: Response): Promise<string> {
  try {
    const data: unknown = await res.json();
    if (
      data &&
      typeof data === 'object' &&
      'error' in data &&
      typeof (data as { error: unknown }).error === 'string'
    ) {
      return (data as { error: string }).error;
    }
  } catch {
    // Not JSON — fall through to the generic message.
  }
  return `Request failed with status ${res.status}`;
}

interface ListFormState {
  mode: 'create' | 'edit';
  list?: TaskList;
}

export function ListsPage() {
  const [lists, setLists] = useState<TaskList[]>([]);
  // Latest `lists` for the dependency-free `load` callback below (writing a
  // ref during render is the "latest value" pattern). Reading it lets `load`
  // decide whether to show the full-page loader without closing over a stale
  // array.
  const listsRef = useRef<TaskList[]>([]);
  listsRef.current = lists;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the grid with the
  // error+retry banner when there are no lists to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (delete 409, etc.): rendered as a banner above the
  // still-visible grid — cards are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);
  const [form, setForm] = useState<ListFormState | null>(null);
  const [name, setName] = useState('');
  const [color, setColor] = useState('#2a5c8a');
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(() => {
    // Full-page loader only when the grid is empty (first load, or a retry
    // after a hard error cleared it) — the same rule as CalendarPage
    // (`isLoading: prev.events.length === 0`), so reloads fired while cards
    // are on screen never flash the spinner.
    setIsLoading(listsRef.current.length === 0);
    setLoadError(null);
    fetch(`${API_BASE_URL}/api/lists`, { credentials: 'include' })
      .then(async (res) => {
        if (!res.ok) throw new Error(await readError(res));
        const data = (await res.json()) as TaskListsResponse;
        setLists(data.lists ?? []);
      })
      .catch((err: unknown) => {
        const message = err instanceof Error ? err.message : 'Failed to load lists';
        setLoadError(message);
      })
      .finally(() => setIsLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const openCreate = () => {
    setForm({ mode: 'create' });
    setName('');
    setColor('#2a5c8a');
    setFormError(null);
  };

  const openEdit = (list: TaskList) => {
    setForm({ mode: 'edit', list });
    setName(list.name);
    setColor(list.color);
    setFormError(null);
  };

  const closeForm = () => {
    setForm(null);
    setSaving(false);
    setFormError(null);
  };

  const handleSubmit = async () => {
    if (!form) return;
    const trimmed = name.trim();
    if (!trimmed || !color) return;

    setSaving(true);
    setFormError(null);
    setActionError(null);
    const res =
      form.mode === 'create'
        ? await fetch(`${API_BASE_URL}/api/lists`, {
            method: 'POST',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color }),
          })
        : await fetch(`${API_BASE_URL}/api/lists/${form.list!.id}`, {
            method: 'PATCH',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color }),
          });
    if (!res.ok) {
      setSaving(false);
      setFormError(await readError(res));
      return;
    }
    closeForm();
    load();
  };

  const handleDelete = async (list: TaskList) => {
    const confirmed = window.confirm(`Delete "${list.name}"?`);
    if (!confirmed) return;

    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/lists/${list.id}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) {
      const message = await readError(res);
      // 409 from the backend: living root categories still reference the list.
      setActionError(
        res.status === 409
          ? `"${list.name}" is still in use and cannot be deleted.`
          : message,
      );
      return;
    }
    load();
  };

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              My Lists
            </h1>
            <p className="text-muted-foreground">
              Manage your life domains — tasks live inside categories later
            </p>
          </div>
          <Button
            onClick={openCreate}
            className="bg-sanctuary-green hover:bg-sanctuary-green/90"
          >
            <Plus className="h-4 w-4 mr-2" />
            New List
          </Button>
        </header>

        {/* Load error banner — replaces the grid only when there are no lists
            to show; with cards on screen it reads as a refresh-failure notice
            above the still-visible grid */}
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

        {/* Action error banner (e.g. a delete 409) — the grid stays mounted */}
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

        {/* Loading — only while the grid is empty (first load or a retry after
            a hard error); reloads with cards on screen never flash this */}
        {isLoading && lists.length === 0 && (
          <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading lists…
          </div>
        )}

        {/* Lists Grid — hidden only when there is nothing to show (first load
            in flight, or a load error that replaced the cards). Action errors
            never unmount the grid. */}
        {(lists.length > 0 || (!isLoading && !loadError)) && (
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-6">
            {lists.map((list) => (
              <ListCard
                key={list.id}
                list={list}
                onEdit={openEdit}
                onDelete={handleDelete}
              />
            ))}
          </div>
        )}
      </div>

      {/* New / Edit List Dialog */}
      <Dialog open={form !== null} onOpenChange={(open) => !open && closeForm()}>
        <DialogContent className="sm:max-w-[420px] p-0 gap-0 overflow-hidden bg-card border-border">
          <div className="h-2" style={{ backgroundColor: color }} />
          <div className="p-6">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">
                {form?.mode === 'edit' ? 'Edit List' : 'New List'}
              </DialogTitle>
              <DialogDescription>
                {form?.mode === 'edit'
                  ? 'Update the name or color of your list.'
                  : 'Create a new list to organize your tasks.'}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Name
              </label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g. Work"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Color
              </label>
              <div className="flex items-center gap-3">
                <input
                  type="color"
                  value={color}
                  onChange={(e) => setColor(e.target.value)}
                  className="h-10 w-14 rounded-lg border border-input bg-background cursor-pointer"
                  aria-label="List color"
                />
                <span className="text-sm text-muted-foreground font-mono">
                  {color}
                </span>
              </div>
            </div>

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}

            <div className="flex justify-end gap-3">
              <Button
                variant="outline"
                onClick={closeForm}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={handleSubmit}
                disabled={!name.trim() || saving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {form?.mode === 'edit' ? 'Save Changes' : 'Create List'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

interface ListCardProps {
  list: TaskList;
  onEdit: (list: TaskList) => void;
  onDelete: (list: TaskList) => void;
}

function ListCard({ list, onEdit, onDelete }: ListCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);

  return (
    <div
      className="rounded-xl overflow-hidden"
      style={{ backgroundColor: list.color }}
    >
      <div className="p-4">
        <div className="flex items-center justify-between mb-4 gap-2">
          <h3 className="font-heading text-lg font-semibold text-primary-foreground truncate">
            {list.name}
          </h3>
          <div className="relative flex-shrink-0">
            <button
              onClick={() => setMenuOpen((open) => !open)}
              className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors"
              aria-label={`Options for ${list.name}`}
            >
              <MoreHorizontal className="h-5 w-5 text-primary-foreground/70" />
            </button>
            {menuOpen && (
              <>
                {/* Invisible backdrop to close the menu on outside click */}
                <div
                  className="fixed inset-0 z-10"
                  onClick={() => setMenuOpen(false)}
                />
                <div className="absolute right-0 top-9 z-20 w-36 bg-popover rounded-lg shadow-lg border border-border py-1">
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      onEdit(list);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted transition-colors"
                  >
                    <Pencil className="h-4 w-4" />
                    Edit
                  </button>
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      onDelete(list);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-destructive hover:bg-destructive/10 transition-colors"
                  >
                    <Trash2 className="h-4 w-4" />
                    Delete
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>

      {/* Tasks area — populated in slice 3 */}
      <div className="bg-black/20 p-4">
        <p className="text-sm text-primary-foreground/70 italic">
          No tasks yet
        </p>
      </div>
    </div>
  );
}

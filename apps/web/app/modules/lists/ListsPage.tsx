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
import type {
  CategoriesResponse,
  Category,
  NewCategoryInput,
  TaskList,
  TaskListsResponse,
  UpdateCategoryInput,
} from '@/app/types';

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

interface PatternDraft {
  regex: string;
  googleCalendarId: string;
}

interface CategoryFormState {
  mode: 'create' | 'edit';
  /** Create-root target (undefined for a child category). */
  list?: TaskList;
  /** Create-child target (undefined for a root category). */
  parent?: Category;
  /** Edit target. */
  category?: Category;
}

interface ListFormState {
  mode: 'create' | 'edit';
  list?: TaskList;
}

export function ListsPage() {
  const [lists, setLists] = useState<TaskList[]>([]);
  const [categories, setCategories] = useState<Category[]>([]);
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

  // List dialog state.
  const [listForm, setListForm] = useState<ListFormState | null>(null);
  const [listName, setListName] = useState('');
  const [listColor, setListColor] = useState('#2a5c8a');

  // Category dialog state.
  const [form, setForm] = useState<CategoryFormState | null>(null);
  const [title, setTitle] = useState('');
  const [color, setColor] = useState('#2a5c8a');
  const [isProductive, setIsProductive] = useState(false);
  const [googleCalendarId, setGoogleCalendarId] = useState('');
  const [patterns, setPatterns] = useState<PatternDraft[]>([]);

  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(() => {
    // Full-page loader only when the grid is empty (first load, or a retry
    // after a hard error cleared it) — the same rule as CalendarPage
    // (`isLoading: prev.events.length === 0`), so reloads fired while cards
    // are on screen never flash the spinner.
    setIsLoading(listsRef.current.length === 0);
    setLoadError(null);
    // Sequential on purpose: GET /api/lists performs the first-visit seed (it
    // inserts the default lists AND the category taxonomy), so the categories
    // request must run after it — a parallel fetch would often return [] on
    // first paint and never retry.
    fetch(`${API_BASE_URL}/api/lists`, { credentials: 'include' })
      .then(async (listsRes) => {
        if (!listsRes.ok) throw new Error(await readError(listsRes));
        const listsData = (await listsRes.json()) as TaskListsResponse;
        setLists(listsData.lists ?? []);
        return fetch(`${API_BASE_URL}/api/categories`, { credentials: 'include' });
      })
      .then(async (categoriesRes) => {
        if (!categoriesRes.ok) throw new Error(await readError(categoriesRes));
        const categoriesData = (await categoriesRes.json()) as CategoriesResponse;
        setCategories(categoriesData.categories ?? []);
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

  // ──────────────────────────────────────────
  // List actions
  // ──────────────────────────────────────────

  const openCreateList = () => {
    setListForm({ mode: 'create' });
    setListName('');
    setListColor('#2a5c8a');
    setFormError(null);
  };

  const openEditList = (list: TaskList) => {
    setListForm({ mode: 'edit', list });
    setListName(list.name);
    setListColor(list.color);
    setFormError(null);
  };

  const closeListForm = () => {
    setListForm(null);
    setSaving(false);
    setFormError(null);
  };

  const handleListSubmit = async () => {
    if (!listForm) return;
    const trimmed = listName.trim();
    if (!trimmed || !listColor) return;

    setSaving(true);
    setFormError(null);
    setActionError(null);
    const res =
      listForm.mode === 'create'
        ? await fetch(`${API_BASE_URL}/api/lists`, {
            method: 'POST',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color: listColor }),
          })
        : await fetch(`${API_BASE_URL}/api/lists/${listForm.list!.id}`, {
            method: 'PATCH',
            credentials: 'include',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ name: trimmed, color: listColor }),
          });
    if (!res.ok) {
      setSaving(false);
      setFormError(await readError(res));
      return;
    }
    closeListForm();
    load();
  };

  const handleDeleteList = async (list: TaskList) => {
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

  // ──────────────────────────────────────────
  // Category actions
  // ──────────────────────────────────────────

  const openCreateRoot = (list: TaskList) => {
    setForm({ mode: 'create', list });
    setTitle('');
    setColor(list.color);
    setIsProductive(false);
    setGoogleCalendarId('');
    setPatterns([]);
    setFormError(null);
  };

  const openCreateChild = (parent: Category) => {
    setForm({ mode: 'create', parent });
    setTitle('');
    setColor(parent.color || '#2a5c8a');
    setIsProductive(false);
    setGoogleCalendarId('');
    setPatterns([]);
    setFormError(null);
  };

  const openEditCategory = (category: Category) => {
    setForm({ mode: 'edit', category });
    setTitle(category.title);
    setColor(category.color || '#2a5c8a');
    setIsProductive(category.is_productive);
    setGoogleCalendarId(category.google_calendar_id ?? '');
    setPatterns(
      category.patterns.map((pattern) => ({
        regex: pattern.regex,
        googleCalendarId: pattern.google_calendar_id ?? '',
      })),
    );
    setFormError(null);
  };

  const closeForm = () => {
    setForm(null);
    setSaving(false);
    setFormError(null);
  };

  const handleCategorySubmit = async () => {
    if (!form) return;
    const trimmedTitle = title.trim();
    if (!trimmedTitle || !color) return;

    const bodyPatterns = patterns
      .filter((pattern) => pattern.regex.trim().length > 0)
      .map((pattern) => ({
        regex: pattern.regex.trim(),
        google_calendar_id: pattern.googleCalendarId.trim() || null,
      }));

    setSaving(true);
    setFormError(null);
    setActionError(null);

    let res: Response;
    if (form.mode === 'edit') {
      const body: UpdateCategoryInput = {
        title: trimmedTitle,
        color,
        is_productive: isProductive,
        google_calendar_id: googleCalendarId.trim() || null,
        patterns: bodyPatterns,
      };
      res = await fetch(`${API_BASE_URL}/api/categories/${form.category!.id}`, {
        method: 'PATCH',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
    } else {
      const body: NewCategoryInput = {
        title: trimmedTitle,
        color,
        is_productive: isProductive,
        google_calendar_id: googleCalendarId.trim() || null,
        // Roots carry the target list; children carry no list_id at all (the
        // service rejects a child with one).
        list_id: form.list ? form.list.id : null,
        parent_id: form.parent ? form.parent.id : null,
        patterns: bodyPatterns,
      };
      res = await fetch(`${API_BASE_URL}/api/categories`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
    }
    if (!res.ok) {
      setSaving(false);
      setFormError(await readError(res));
      return;
    }
    closeForm();
    load();
  };

  const handleDeleteCategory = async (category: Category) => {
    const confirmed = window.confirm(`Delete "${category.title}"?`);
    if (!confirmed) return;

    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/categories/${category.id}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) {
      // The backend explains 409s (living children, undeletable untracked).
      setActionError(await readError(res));
      return;
    }
    load();
  };

  // ──────────────────────────────────────────
  // Patterns editor
  // ──────────────────────────────────────────

  const addPattern = () => {
    setPatterns((prev) => [...prev, { regex: '', googleCalendarId: '' }]);
  };

  const updatePattern = (index: number, patch: Partial<PatternDraft>) => {
    setPatterns((prev) =>
      prev.map((pattern, i) => (i === index ? { ...pattern, ...patch } : pattern)),
    );
  };

  const removePattern = (index: number) => {
    setPatterns((prev) => prev.filter((_, i) => i !== index));
  };

  const dialogTitle = form
    ? form.mode === 'edit'
      ? 'Edit Category'
      : form.parent
        ? `Sub-Category under ${form.parent.title}`
        : form.list
          ? `Category in ${form.list.name}`
          : 'New Category'
    : '';

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
            onClick={openCreateList}
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
                categories={categories}
                onEditList={openEditList}
                onDeleteList={handleDeleteList}
                onAddCategory={openCreateRoot}
                onAddChild={openCreateChild}
                onEditCategory={openEditCategory}
                onDeleteCategory={handleDeleteCategory}
              />
            ))}
          </div>
        )}
      </div>

      {/* New / Edit List Dialog */}
      <Dialog open={listForm !== null} onOpenChange={(open) => !open && closeListForm()}>
        <DialogContent className="sm:max-w-[420px] p-0 gap-0 overflow-hidden bg-card border-border">
          <div className="h-2" style={{ backgroundColor: listColor }} />
          <div className="p-6">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">
                {listForm?.mode === 'edit' ? 'Edit List' : 'New List'}
              </DialogTitle>
              <DialogDescription>
                {listForm?.mode === 'edit'
                  ? 'Update the name or color of your list.'
                  : 'Create a new list to organize your tasks.'}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">Name</label>
              <input
                type="text"
                value={listName}
                onChange={(e) => setListName(e.target.value)}
                placeholder="e.g. Work"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">Color</label>
              <div className="flex items-center gap-3">
                <input
                  type="color"
                  value={listColor}
                  onChange={(e) => setListColor(e.target.value)}
                  className="h-10 w-14 rounded-lg border border-input bg-background cursor-pointer"
                  aria-label="List color"
                />
                <span className="text-sm text-muted-foreground font-mono">{listColor}</span>
              </div>
            </div>

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}

            <div className="flex justify-end gap-3">
              <Button
                variant="outline"
                onClick={closeListForm}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={handleListSubmit}
                disabled={!listName.trim() || saving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {listForm?.mode === 'edit' ? 'Save Changes' : 'Create List'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>

      {/* New / Edit Category Dialog */}
      <Dialog open={form !== null} onOpenChange={(open) => !open && closeForm()}>
        <DialogContent className="sm:max-w-[560px] p-0 gap-0 overflow-hidden bg-card border-border">
          <div className="h-2" style={{ backgroundColor: color }} />
          <div className="p-6 max-h-[80vh] overflow-y-auto">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">{dialogTitle}</DialogTitle>
              <DialogDescription>
                {form?.mode === 'edit'
                  ? 'Update the category and its title-matching patterns.'
                  : 'Categories group tasks; patterns classify titles into them.'}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">Title</label>
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="e.g. Deep Work"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">Color</label>
              <div className="flex items-center gap-3">
                <input
                  type="color"
                  value={color}
                  onChange={(e) => setColor(e.target.value)}
                  className="h-10 w-14 rounded-lg border border-input bg-background cursor-pointer"
                  aria-label="Category color"
                />
                <span className="text-sm text-muted-foreground font-mono">{color}</span>
              </div>
            </div>

            <div className="flex items-center gap-2 mb-5">
              <input
                type="checkbox"
                id="is_productive"
                checked={isProductive}
                onChange={(e) => setIsProductive(e.target.checked)}
                className="h-4 w-4 rounded border-input accent-emerald-600"
              />
              <label htmlFor="is_productive" className="text-sm font-medium text-foreground">
                Productive
              </label>
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Google Calendar id
              </label>
              <input
                type="text"
                value={googleCalendarId}
                onChange={(e) => setGoogleCalendarId(e.target.value)}
                placeholder="Optional — links this category to a calendar"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Patterns <span className="text-muted-foreground font-normal">(title → category)</span>
              </label>
              {patterns.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  No patterns yet — matching titles will never land in this category.
                </p>
              )}
              {patterns.map((pattern, index) => (
                <div key={index} className="space-y-1 rounded-xl border border-input bg-background p-3">
                  <div className="flex items-center gap-2">
                    <input
                      type="text"
                      value={pattern.regex}
                      onChange={(e) => updatePattern(index, { regex: e.target.value })}
                      placeholder="e.g. ^Deep Work$"
                      className="flex-1 min-w-0 px-3 py-2 rounded-lg border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all text-sm font-mono"
                    />
                    <button
                      onClick={() => removePattern(index)}
                      className="p-2 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors text-muted-foreground"
                      aria-label={`Remove pattern ${index + 1}`}
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  </div>
                  <input
                    type="text"
                    value={pattern.googleCalendarId}
                    onChange={(e) =>
                      updatePattern(index, { googleCalendarId: e.target.value })
                    }
                    placeholder="Google Calendar id (optional — events only)"
                    className="w-full px-3 py-2 rounded-lg border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all text-xs"
                  />
                </div>
              ))}
              <Button
                variant="outline"
                size="sm"
                onClick={addPattern}
                className="border-input text-foreground hover:bg-muted"
              >
                <Plus className="h-4 w-4 mr-1" />
                Add pattern
              </Button>
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
                onClick={handleCategorySubmit}
                disabled={!title.trim() || saving}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {form?.mode === 'edit' ? 'Save Changes' : 'Create Category'}
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
  categories: Category[];
  onEditList: (list: TaskList) => void;
  onDeleteList: (list: TaskList) => void;
  onAddCategory: (list: TaskList) => void;
  onAddChild: (category: Category) => void;
  onEditCategory: (category: Category) => void;
  onDeleteCategory: (category: Category) => void;
}

function ListCard({
  list,
  categories,
  onEditList,
  onDeleteList,
  onAddCategory,
  onAddChild,
  onEditCategory,
  onDeleteCategory,
}: ListCardProps) {
  const [menuOpen, setMenuOpen] = useState(false);

  // Roots belonging to this list — `inherited_list_id` equals the root's own
  // list_id, so one filter covers both stored and inherited membership.
  // `untracked` (list_id NULL, is_untracked) never appears here.
  const roots = categories.filter(
    (category) =>
      !category.is_untracked && !category.parent_id && category.inherited_list_id === list.id,
  );

  return (
    <div className="rounded-xl overflow-hidden" style={{ backgroundColor: list.color }}>
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
                      onEditList(list);
                    }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-sm text-foreground hover:bg-muted transition-colors"
                  >
                    <Pencil className="h-4 w-4" />
                    Edit
                  </button>
                  <button
                    onClick={() => {
                      setMenuOpen(false);
                      onDeleteList(list);
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

        <button
          onClick={() => onAddCategory(list)}
          className="flex w-full items-center justify-center gap-1 rounded-lg border border-dashed border-primary-foreground/30 py-2 text-sm text-primary-foreground/80 hover:bg-primary-foreground/10 transition-colors"
        >
          <Plus className="h-4 w-4" />
          Category
        </button>
      </div>

      {/* Categories under this list — the one-level tree (roots + children) */}
      {roots.length > 0 && (
        <div className="bg-black/20 p-3 space-y-2">
          {roots.map((root) => {
            const children = categories.filter((category) => category.parent_id === root.id);
            return (
              <div key={root.id}>
                <div className="flex items-center gap-2">
                  <span
                    className="h-2.5 w-2.5 rounded-full flex-shrink-0"
                    style={{ backgroundColor: root.color || list.color }}
                  />
                  <span className="flex-1 min-w-0 text-sm font-medium text-primary-foreground truncate">
                    {root.title}
                  </span>
                  {root.is_productive && (
                    <span className="flex-shrink-0 rounded-full bg-emerald-400/30 px-2 py-0.5 text-[10px] uppercase tracking-wide text-emerald-100">
                      productive
                    </span>
                  )}
                  <div className="flex flex-shrink-0 gap-1">
                    <button
                      onClick={() => onAddChild(root)}
                      className="p-1.5 rounded-md hover:bg-primary-foreground/10 transition-colors"
                      aria-label={`Add sub-category under ${root.title}`}
                      title="Add sub-category"
                    >
                      <Plus className="h-3.5 w-3.5 text-primary-foreground/70" />
                    </button>
                    <button
                      onClick={() => onEditCategory(root)}
                      className="p-1.5 rounded-md hover:bg-primary-foreground/10 transition-colors"
                      aria-label={`Edit ${root.title}`}
                      title="Edit"
                    >
                      <Pencil className="h-3.5 w-3.5 text-primary-foreground/70" />
                    </button>
                    <button
                      onClick={() => onDeleteCategory(root)}
                      className="p-1.5 rounded-md hover:bg-primary-foreground/10 transition-colors"
                      aria-label={`Delete ${root.title}`}
                      title="Delete"
                    >
                      <Trash2 className="h-3.5 w-3.5 text-primary-foreground/70" />
                    </button>
                  </div>
                </div>
                {children.length > 0 && (
                  <div className="ml-3.5 mt-1 pl-3 border-l border-primary-foreground/20 space-y-1">
                    {children.map((child) => (
                      <div key={child.id} className="flex items-center gap-2">
                        <span
                          className="h-2 w-2 rounded-full flex-shrink-0"
                          style={{ backgroundColor: child.color || root.color || list.color }}
                        />
                        <span className="flex-1 min-w-0 text-xs text-primary-foreground/80 truncate">
                          {child.title}
                        </span>
                        <div className="flex flex-shrink-0 gap-1">
                          <button
                            onClick={() => onEditCategory(child)}
                            className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors"
                            aria-label={`Edit ${child.title}`}
                          >
                            <Pencil className="h-3 w-3 text-primary-foreground/60" />
                          </button>
                          <button
                            onClick={() => onDeleteCategory(child)}
                            className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors"
                            aria-label={`Delete ${child.title}`}
                          >
                            <Trash2 className="h-3 w-3 text-primary-foreground/60" />
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* Tasks area — populated in slice 3 */}
      <div className="bg-black/20 p-4 border-t border-black/10">
        <p className="text-sm text-primary-foreground/70 italic">
          No tasks yet
        </p>
      </div>
    </div>
  );
}

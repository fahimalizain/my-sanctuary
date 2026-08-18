import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, Pencil, Plus, Trash2 } from 'lucide-react';
import { CalendarPicker } from '@/app/components/CalendarPicker';
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
  CalendarsResponse,
  CategoriesResponse,
  Category,
  GoogleCalendar,
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

export function CategoriesPage() {
  const [lists, setLists] = useState<TaskList[]>([]);
  const [categories, setCategories] = useState<Category[]>([]);
  // Latest `lists` for the dependency-free `load` callback below (writing a
  // ref during render is the "latest value" pattern). Reading it lets `load`
  // decide whether to show the full-page loader without closing over a stale
  // array.
  const listsRef = useRef<TaskList[]>([]);
  listsRef.current = lists;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the document tree with
  // the error+retry banner when there are no lists to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (delete 409, etc.): rendered as a banner above the
  // still-visible tree — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // Category dialog state.
  const [form, setForm] = useState<CategoryFormState | null>(null);
  const [title, setTitle] = useState('');
  const [color, setColor] = useState('#2a5c8a');
  const [isProductive, setIsProductive] = useState(false);
  const [googleCalendarId, setGoogleCalendarId] = useState('');
  const [patterns, setPatterns] = useState<PatternDraft[]>([]);

  // Google calendars for the pickers — fetched once per dialog open (never
  // per keystroke / pattern add) and shared by every CalendarPicker. The
  // API is cache-only after first import, so keeping the previous list
  // across closes is fine; refetching each open is preferred.
  const [calendars, setCalendars] = useState<GoogleCalendar[]>([]);
  const [calendarsLoading, setCalendarsLoading] = useState(false);
  const [calendarsError, setCalendarsError] = useState<string | null>(null);

  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(() => {
    // Full-page loader only while the tree is empty (first load, or a retry
    // after a hard error cleared it) — reloads fired while rows are on
    // screen never flash the spinner.
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
        return fetch(`${API_BASE_URL}/api/categories`, {
          credentials: 'include',
        });
      })
      .then(async (categoriesRes) => {
        if (!categoriesRes.ok) throw new Error(await readError(categoriesRes));
        const categoriesData =
          (await categoriesRes.json()) as CategoriesResponse;
        setCategories(categoriesData.categories ?? []);
      })
      .catch((err: unknown) => {
        const message =
          err instanceof Error ? err.message : 'Failed to load lists';
        setLoadError(message);
      })
      .finally(() => setIsLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Calendars for the pickers: one request when the dialog opens, shared by
  // every CalendarPicker instance. `form` changes identity only on
  // open/close, so edits inside the dialog never refetch.
  useEffect(() => {
    if (form === null) return;
    let cancelled = false;
    setCalendarsLoading(true);
    setCalendarsError(null);
    fetch(`${API_BASE_URL}/api/calendar/calendars`, {
      credentials: 'include',
    })
      .then(async (res) => {
        if (!res.ok) throw new Error(await readError(res));
        const data = (await res.json()) as CalendarsResponse;
        if (!cancelled) setCalendars(data.calendars ?? []);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setCalendarsError(
          err instanceof Error ? err.message : 'Failed to load calendars',
        );
      })
      .finally(() => {
        if (!cancelled) setCalendarsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [form]);

  // ──────────────────────────────────────────
  // Category actions — same dialogs and bodies as ListsPage.
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
      prev.map((pattern, i) =>
        i === index ? { ...pattern, ...patch } : pattern,
      ),
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
              Categories
            </h1>
            <p className="text-muted-foreground">
              The taxonomy that files tasks into lists
            </p>
          </div>
        </header>

        {/* Load error banner — replaces the tree only when there are no lists
            to show; with rows on screen it reads as a refresh-failure notice
            above the still-visible tree */}
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

        {/* Action error banner (e.g. a delete 409) — the tree stays mounted */}
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

        {/* Loading — only while the tree is empty (first load or a retry
            after a hard error); reloads with rows on screen never flash
            this */}
        {isLoading && lists.length === 0 && (
          <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading categories…
          </div>
        )}

        {/* Document tree — hidden only when there is nothing to show (first
            load in flight, or a load error that replaced the rows). Action
            errors never unmount the tree. */}
        {(lists.length > 0 || (!isLoading && !loadError)) && (
          <div className="space-y-6">
            {lists.map((list) => {
              // Roots belonging to this list — `inherited_list_id` equals the
              // root's own list_id, so one filter covers both stored and
              // inherited membership. `untracked` (list_id NULL,
              // is_untracked) never appears here.
              const roots = categories.filter(
                (category) =>
                  !category.is_untracked &&
                  !category.parent_id &&
                  category.inherited_list_id === list.id,
              );
              return (
                <section
                  key={list.id}
                  className="rounded-xl border border-border bg-card p-5"
                >
                  {/* Heading row: swatch + name + Add category */}
                  <div className="flex items-center justify-between gap-3 mb-4">
                    <h2 className="flex items-center gap-2 font-heading text-lg font-semibold text-foreground min-w-0">
                      <span
                        className="h-3 w-3 rounded-full flex-shrink-0"
                        style={{ backgroundColor: list.color }}
                      />
                      <span className="truncate">{list.name}</span>
                    </h2>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => openCreateRoot(list)}
                      className="flex-shrink-0 border-input text-foreground hover:bg-muted"
                    >
                      <Plus className="h-4 w-4 mr-1" />
                      Add category
                    </Button>
                  </div>

                  {roots.length === 0 ? (
                    <p className="text-sm text-muted-foreground italic">
                      No categories yet
                    </p>
                  ) : (
                    <div className="space-y-2">
                      {roots.map((root) => {
                        // One-level tree: children hang directly under the
                        // root they reference.
                        const children = categories.filter(
                          (category) => category.parent_id === root.id,
                        );
                        return (
                          <div
                            key={root.id}
                            className="rounded-lg border border-border bg-background p-3"
                          >
                            <div className="flex items-center gap-2">
                              <span
                                className="h-2.5 w-2.5 rounded-full flex-shrink-0"
                                style={{
                                  backgroundColor: root.color || list.color,
                                }}
                              />
                              <span className="flex-1 min-w-0 text-sm font-medium text-foreground truncate">
                                {root.title}
                              </span>
                              {root.is_productive && (
                                <span className="flex-shrink-0 rounded-full bg-emerald-400/30 px-2 py-0.5 text-[10px] uppercase tracking-wide text-emerald-700">
                                  productive
                                </span>
                              )}
                              <div className="flex flex-shrink-0 gap-1">
                                <button
                                  onClick={() => openCreateChild(root)}
                                  className="p-1.5 rounded-md hover:bg-muted transition-colors"
                                  aria-label={`Add sub-category under ${root.title}`}
                                  title="Add sub-category"
                                >
                                  <Plus className="h-3.5 w-3.5 text-muted-foreground" />
                                </button>
                                <button
                                  onClick={() => openEditCategory(root)}
                                  className="p-1.5 rounded-md hover:bg-muted transition-colors"
                                  aria-label={`Edit ${root.title}`}
                                  title="Edit"
                                >
                                  <Pencil className="h-3.5 w-3.5 text-muted-foreground" />
                                </button>
                                <button
                                  onClick={() => handleDeleteCategory(root)}
                                  className="p-1.5 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors"
                                  aria-label={`Delete ${root.title}`}
                                  title="Delete"
                                >
                                  <Trash2 className="h-3.5 w-3.5 text-muted-foreground" />
                                </button>
                              </div>
                            </div>
                            {children.length > 0 && (
                              <div className="ml-3.5 mt-2 pl-3 border-l border-border space-y-1">
                                {children.map((child) => (
                                  <div
                                    key={child.id}
                                    className="flex items-center gap-2"
                                  >
                                    <span
                                      className="h-2 w-2 rounded-full flex-shrink-0"
                                      style={{
                                        backgroundColor:
                                          child.color ||
                                          root.color ||
                                          list.color,
                                      }}
                                    />
                                    <span className="flex-1 min-w-0 text-xs text-muted-foreground truncate">
                                      {child.title}
                                    </span>
                                    <div className="flex flex-shrink-0 gap-1">
                                      <button
                                        onClick={() => openEditCategory(child)}
                                        className="p-1 rounded-md hover:bg-muted transition-colors"
                                        aria-label={`Edit ${child.title}`}
                                        title="Edit"
                                      >
                                        <Pencil className="h-3 w-3 text-muted-foreground" />
                                      </button>
                                      <button
                                        onClick={() =>
                                          handleDeleteCategory(child)
                                        }
                                        className="p-1 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors"
                                        aria-label={`Delete ${child.title}`}
                                        title="Delete"
                                      >
                                        <Trash2 className="h-3 w-3 text-muted-foreground" />
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
                </section>
              );
            })}
          </div>
        )}
      </div>

      {/* New / Edit Category Dialog */}
      <Dialog
        open={form !== null}
        onOpenChange={(open) => !open && closeForm()}
      >
        <DialogContent className="sm:max-w-[560px] p-0 gap-0 overflow-hidden bg-card border-border">
          <div className="h-2" style={{ backgroundColor: color }} />
          <div className="p-6 max-h-[80vh] overflow-y-auto">
            <DialogHeader className="mb-6">
              <DialogTitle className="text-foreground">
                {dialogTitle}
              </DialogTitle>
              <DialogDescription>
                {form?.mode === 'edit'
                  ? 'Update the category and its title-matching patterns.'
                  : 'Categories group tasks; patterns classify titles into them.'}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Title
              </label>
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="e.g. Deep Work"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
            </div>

            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Color
              </label>
              <div className="flex items-center gap-3">
                <input
                  type="color"
                  value={color}
                  onChange={(e) => setColor(e.target.value)}
                  className="h-10 w-14 rounded-lg border border-input bg-background cursor-pointer"
                  aria-label="Category color"
                />
                <span className="text-sm text-muted-foreground font-mono">
                  {color}
                </span>
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
              <label
                htmlFor="is_productive"
                className="text-sm font-medium text-foreground"
              >
                Productive
              </label>
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Google Calendar
              </label>
              <CalendarPicker
                value={googleCalendarId}
                onChange={setGoogleCalendarId}
                calendars={calendars}
                isLoading={calendarsLoading}
                error={calendarsError}
                placeholder="None — optional"
                aria-label="Google Calendar"
              />
            </div>

            <div className="space-y-2 mb-6">
              <label className="text-sm font-medium text-foreground">
                Patterns{' '}
                <span className="text-muted-foreground font-normal">
                  (title → category)
                </span>
              </label>
              {patterns.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  No patterns yet — matching titles will never land in this
                  category.
                </p>
              )}
              {patterns.map((pattern, index) => (
                <div
                  key={index}
                  className="space-y-1 rounded-xl border border-input bg-background p-3"
                >
                  <div className="flex items-center gap-2">
                    <input
                      type="text"
                      value={pattern.regex}
                      onChange={(e) =>
                        updatePattern(index, { regex: e.target.value })
                      }
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
                  {/* Error text lives on the category-level picker only, so
                      a failed fetch does not repeat the message once per
                      pattern row. */}
                  <CalendarPicker
                    size="sm"
                    value={pattern.googleCalendarId}
                    onChange={(id) =>
                      updatePattern(index, { googleCalendarId: id })
                    }
                    calendars={calendars}
                    isLoading={calendarsLoading}
                    error={null}
                    placeholder="None — events only"
                    aria-label={`Pattern ${index + 1} Google Calendar`}
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

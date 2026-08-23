import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Loader2, Pencil, Plus, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog';
import { useNavigate } from '@tanstack/react-router';
import { API_BASE_URL } from '@/lib/api';
import { useTitleClassification } from '@/app/hooks/useTitleClassification';
import type { ClassifyStatus } from '@/app/hooks/useTitleClassification';
import type {
  NewRoutineInput,
  RoutineRecord,
  RoutinesResponse,
  UpdateRoutineInput,
} from '@/app/types';
import {
  addCivilDays,
  civilToday,
  composeRruleBlob,
  isValidRruleBody,
  occurrenceDates,
  parseRruleBlob,
  rruleParts,
  rruleSummary,
} from './rrule-preview';

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

type WeekdayCode = 'MO' | 'TU' | 'WE' | 'TH' | 'FR' | 'SA' | 'SU';
const WEEKDAY_CODES: WeekdayCode[] = ['MO', 'TU', 'WE', 'TH', 'FR', 'SA', 'SU'];
const WEEKDAY_LABELS: Record<WeekdayCode, string> = {
  MO: 'Mo',
  TU: 'Tu',
  WE: 'We',
  TH: 'Th',
  FR: 'Fr',
  SA: 'Sa',
  SU: 'Su',
};

type Freq = 'DAILY' | 'WEEKLY';
type UntilMode = 'never' | 'until' | 'count';

const EMPTY_DAYS: Record<WeekdayCode, boolean> = {
  MO: false,
  TU: false,
  WE: false,
  TH: false,
  FR: false,
  SA: false,
  SU: false,
};

function emptyDays(): Record<WeekdayCode, boolean> {
  return { ...EMPTY_DAYS };
}

/** Weekday code (MO..SU) of a `YYYY-MM-DD` civil date, or null when
 *  malformed. Uses the same UTC-carrier trick as `rrule-preview`. */
function weekdayOf(date: string): WeekdayCode | null {
  const m = date.match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!m) return null;
  const carrier = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
  return WEEKDAY_CODES[(carrier.getUTCDay() + 6) % 7];
}

interface RoutineFormState {
  mode: 'create' | 'edit';
  routine?: RoutineRecord;
}

export function RoutinesPage() {
  const navigate = useNavigate();
  const [routines, setRoutines] = useState<RoutineRecord[]>([]);
  // Latest `routines` for the dependency-free `load` callback below (writing a
  // ref during render is the "latest value" pattern) — same as CategoriesPage.
  const routinesRef = useRef<RoutineRecord[]>([]);
  routinesRef.current = routines;
  const [isLoading, setIsLoading] = useState(true);
  // Load failures: only set from `load()`. Replaces the document tree with
  // the error+retry banner when there are no routines to show.
  const [loadError, setLoadError] = useState<string | null>(null);
  // Action failures (delete 400, etc.): rendered as a banner above the
  // still-visible list — rows are never unmounted by an action error.
  const [actionError, setActionError] = useState<string | null>(null);

  // Routine dialog state.
  const [form, setForm] = useState<RoutineFormState | null>(null);
  const [title, setTitle] = useState('');
  const [estimatedMinutes, setEstimatedMinutes] = useState('15');
  const [dtstartDate, setDtstartDate] = useState(civilToday());
  const [dtstartTime, setDtstartTime] = useState('06:30');
  const [freq, setFreq] = useState<Freq>('DAILY');
  const [intervalText, setIntervalText] = useState('1');
  const [weeklyDays, setWeeklyDays] =
    useState<Record<WeekdayCode, boolean>>(emptyDays);
  const [untilMode, setUntilMode] = useState<UntilMode>('never');
  const [untilDate, setUntilDate] = useState('');
  const [countText, setCountText] = useState('5');
  // Raw RRULE body override — wins over the builder when non-empty. Lets
  // power users type any valid body (MONTHLY, BYMONTHDAY, …) the builder
  // cannot express.
  const [rawOverride, setRawOverride] = useState('');
  // The title snapshot the classify hook fires on: set on blur and on
  // edit-open (the stored title). Title-only classify — routines have no
  // category lock; the server stays the authority.
  const [classifyTitle, setClassifyTitle] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(() => {
    // Full-page loader only while the list is empty (first load, or a retry
    // after a hard error cleared it) — reloads fired while rows are on
    // screen never flash the spinner.
    setIsLoading(routinesRef.current.length === 0);
    setLoadError(null);
    fetch(`${API_BASE_URL}/api/routines`, { credentials: 'include' })
      .then(async (res) => {
        if (!res.ok) throw new Error(await readError(res));
        const data = (await res.json()) as RoutinesResponse;
        setRoutines(data.routines ?? []);
      })
      .catch((err: unknown) => {
        const message =
          err instanceof Error ? err.message : 'Failed to load routines';
        setLoadError(message);
      })
      .finally(() => setIsLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Title classify preview (advisory — the server 400s authoritatively).
  // resetKey changes on open/close and per routine id, so the hook
  // self-resets there.
  const classifyStatus: ClassifyStatus = useTitleClassification({
    title,
    classifyTitle,
    categoryId: null,
    initialTitle:
      form?.mode === 'edit' ? form.routine!.title : undefined,
    active: form !== null,
    resetKey: `${form !== null}:${form?.mode === 'edit' ? form.routine!.id : 'new'}`,
  });

  // ──────────────────────────────────────────
  // Dialog open/close + prefill
  // ──────────────────────────────────────────

  const openCreate = () => {
    setForm({ mode: 'create' });
    setTitle('');
    setEstimatedMinutes('15');
    setDtstartDate(civilToday());
    setDtstartTime('06:30');
    setFreq('DAILY');
    setIntervalText('1');
    setWeeklyDays(emptyDays());
    setUntilMode('never');
    setUntilDate('');
    setCountText('5');
    setRawOverride('');
    setClassifyTitle(null);
    setFormError(null);
  };

  /** Prefills the builder from the stored rule when it is exactly
   *  rebuildable; otherwise the raw override carries the stored body so
   *  editing can never silently change an unrepresentable rule. */
  const prefillRule = (body: string, dtstart: string) => {
    const parts = rruleParts(body);
    const freqPart = parts.get('FREQ')?.toUpperCase();
    const until = parts.get('UNTIL');
    const count = parts.get('COUNT');
    const byday = parts.get('BYDAY');
    const supported = new Set(['FREQ', 'INTERVAL', 'BYDAY', 'UNTIL', 'COUNT']);
    let representable =
      (freqPart === 'DAILY' || freqPart === 'WEEKLY') &&
      [...parts.keys()].every((key) => supported.has(key)) &&
      !(until && count);
    if (representable && byday) {
      for (const token of byday.split(',')) {
        if (!WEEKDAY_CODES.includes(token.trim().replace(/^[+-]?\d{1,2}/, '') as WeekdayCode)) {
          representable = false;
          break;
        }
      }
    }
    if (representable && until) {
      // UNTIL must carry exactly the dtstart's time to be rebuildable
      // (the golden Z-form: `YYYYMMDDT` + dtstart time + `Z`).
      const untilTime = until.slice(9, 15);
      const dtTime = `${dtstart.slice(11, 16)}00`;
      if (untilTime !== dtTime) representable = false;
    }
    if (!representable) {
      setRawOverride(body);
      return;
    }
    setRawOverride('');
    setFreq(freqPart === 'WEEKLY' ? 'WEEKLY' : 'DAILY');
    const intervalRaw = parts.get('INTERVAL');
    setIntervalText(intervalRaw ? String(Math.max(1, Number.parseInt(intervalRaw, 10) || 1)) : '1');
    const days = emptyDays();
    if (byday) {
      for (const token of byday.split(',')) {
        const code = token.trim().replace(/^[+-]?\d{1,2}/, '') as WeekdayCode;
        if (code in days) days[code] = true;
      }
    } else if (freqPart === 'WEEKLY') {
      // No BYDAY means "the weekday of dtstart" — select it so the builder
      // emits the same semantics.
      const code = weekdayOf(dtstart.slice(0, 10));
      if (code) days[code] = true;
    }
    setWeeklyDays(days);
    if (until) {
      setUntilMode('until');
      setUntilDate(`${until.slice(0, 4)}-${until.slice(4, 6)}-${until.slice(6, 8)}`);
      setCountText('5');
    } else if (count) {
      setUntilMode('count');
      setCountText(String(Math.max(1, Number.parseInt(count, 10) || 1)));
      setUntilDate('');
    } else {
      setUntilMode('never');
      setUntilDate('');
      setCountText('5');
    }
  };

  const openEdit = (routine: RoutineRecord) => {
    setForm({ mode: 'edit', routine });
    setTitle(routine.title);
    setEstimatedMinutes(String(routine.estimated_minutes));
    // The stored recurrence is ONE blob: split its DTSTART (basic form) into
    // the builder's civil date + time fields, and prefill the rule body.
    const parsed = parseRruleBlob(routine.rrule);
    if (parsed) {
      setDtstartDate(parsed.dtstart.slice(0, 10));
      setDtstartTime(parsed.dtstart.slice(11, 16));
      prefillRule(parsed.body, parsed.dtstart);
    } else {
      // Hand-edited / unparseable blob: keep the date fields as-is and let
      // the raw override carry the stored body — editing can never silently
      // change an unrepresentable rule.
      setDtstartDate(civilToday());
      setDtstartTime('06:30');
      setRawOverride(routine.rrule);
    }
    setClassifyTitle(routine.title);
    setFormError(null);
  };

  const closeForm = () => {
    setForm(null);
    setSaving(false);
    setFormError(null);
  };

  // ──────────────────────────────────────────
  // Rule builder + validation
  // ──────────────────────────────────────────

  /** The RRULE body a save would send: the raw override when non-empty
   *  (it wins by design), otherwise the builder's output. */
  const buildRruleBody = useCallback((): string => {
    const override = rawOverride.trim();
    if (override) return override;
    let body = `FREQ=${freq}`;
    const interval = Math.max(1, Number.parseInt(intervalText, 10) || 1);
    if (interval > 1) body += `;INTERVAL=${interval}`;
    if (freq === 'WEEKLY') {
      const days = WEEKDAY_CODES.filter((code) => weeklyDays[code]);
      if (days.length > 0) body += `;BYDAY=${days.join(',')}`;
    }
    if (untilMode === 'until' && untilDate && dtstartTime) {
      // Golden Z-form: `YYYYMMDDTHHMMSSZ` with the dtstart's civil time.
      body += `;UNTIL=${untilDate.replaceAll('-', '')}T${dtstartTime.replaceAll(':', '')}00Z`;
    } else if (untilMode === 'count') {
      const count = Math.max(1, Number.parseInt(countText, 10) || 1);
      body += `;COUNT=${count}`;
    }
    return body;
  }, [
    rawOverride,
    freq,
    intervalText,
    weeklyDays,
    untilMode,
    untilDate,
    dtstartTime,
    countText,
  ]);

  const dtstart = `${dtstartDate}T${dtstartTime}:00`;
  const rruleBody = buildRruleBody();
  // The recurrence blob — DTSTART line + RRULE line — must parse together
  // (mirrors the server gate).
  const rruleBlob = composeRruleBlob(dtstart, rruleBody);
  const ruleValid = isValidRruleBody(rruleBlob);
  const estimateValid = Number(estimatedMinutes) >= 1;

  /** Live preview: the next 8 civil dates in the 30-day window from today —
   *  the same expansion the list rows use. */
  const previewDates = useMemo(() => {
    const today = civilToday();
    return occurrenceDates({
      rrule: rruleBlob,
      from: today,
      to: addCivilDays(today, 30),
    }).slice(0, 8);
  }, [rruleBlob]);

  const canSave =
    !saving &&
    title.trim().length > 0 &&
    estimateValid &&
    ruleValid &&
    classifyStatus.state !== 'nomatch' &&
    classifyStatus.state !== 'conflict';

  const switchFreq = (next: Freq) => {
    setFreq(next);
    if (next === 'WEEKLY') {
      // First switch with no days picked: default to the start date's
      // weekday so the body always carries an explicit BYDAY.
      setWeeklyDays((prev) => {
        if (Object.values(prev).some(Boolean)) return prev;
        const code = weekdayOf(dtstartDate);
        return code ? { ...prev, [code]: true } : prev;
      });
    }
  };

  // ──────────────────────────────────────────
  // Actions
  // ──────────────────────────────────────────

  const handleSubmit = async () => {
    if (!form || !canSave) return;
    setSaving(true);
    setFormError(null);
    setActionError(null);

    const payload: NewRoutineInput = {
      title: title.trim(),
      estimated_minutes: Number(estimatedMinutes),
      rrule: rruleBlob,
    };

    let res: Response;
    if (form.mode === 'edit') {
      // Every field optional — sending the full set is a no-op replace.
      res = await fetch(
        `${API_BASE_URL}/api/routines/${form.routine!.id}`,
        {
          method: 'PATCH',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload as UpdateRoutineInput),
        },
      );
    } else {
      res = await fetch(`${API_BASE_URL}/api/routines`, {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
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

  const handleDelete = async (routine: RoutineRecord) => {
    const confirmed = window.confirm(`Delete "${routine.title}"?`);
    if (!confirmed) return;

    setActionError(null);
    const res = await fetch(`${API_BASE_URL}/api/routines/${routine.id}`, {
      method: 'DELETE',
      credentials: 'include',
    });
    if (!res.ok) {
      setActionError(await readError(res));
      return;
    }
    load();
  };

  // ──────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 pt-8 pb-28">
        {/* Header */}
        <header className="flex items-center justify-between gap-4 mb-8">
          <div>
            <div className="flex items-center gap-2 mb-2">
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label="Go back to Home"
                className="-ml-2"
                onClick={() => navigate({ to: '/' })}
              >
                <ArrowLeft className="h-5 w-5" />
              </Button>
              <h1 className="font-heading text-3xl font-bold text-foreground">
                Routines
              </h1>
            </div>
            <p className="text-muted-foreground">
              Standing commitments — they land on Home, never on the Board
            </p>
          </div>
          <Button
            onClick={openCreate}
            className="flex-shrink-0"
          >
            <Plus className="h-4 w-4 mr-1" />
            Add routine
          </Button>
        </header>

        {/* Load error banner — replaces the list only when there are no rows
            to show; with rows on screen it reads as a refresh-failure notice
            above the still-visible list */}
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

        {/* Action error banner (e.g. a delete failure) — the list stays
            mounted */}
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

        {/* Loading — only while the list is empty (first load or a retry
            after a hard error); reloads with rows on screen never flash
            this */}
        {isLoading && routines.length === 0 && (
          <div className="flex items-center justify-center py-24 gap-2 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading routines…
          </div>
        )}

        {/* Empty state — only when a load actually finished with no rows */}
        {!isLoading && !loadError && routines.length === 0 && (
          <div className="rounded-xl border border-dashed border-border bg-card/50 p-10 text-center">
            <p className="font-heading text-lg font-semibold text-foreground mb-1">
              No routines yet
            </p>
            <p className="text-sm text-muted-foreground">
              Add a standing commitment — it will seed Home on its next
              occurrence, and never touch the Board.
            </p>
          </div>
        )}

        {/* Routine list — hidden only when there is nothing to show (first
            load in flight, or a load error that replaced the rows). API
            order = standing sort order. */}
        {routines.length > 0 && (
          <div className="space-y-2">
            {routines.map((routine) => {
              const nextDates = occurrenceDates({
                rrule: routine.rrule,
                from: civilToday(),
                to: addCivilDays(civilToday(), 30),
              });
              return (
                <div
                  key={routine.id}
                  className="rounded-lg border border-border bg-background p-3"
                >
                  <div className="flex items-center gap-2">
                    <span
                      className="h-2.5 w-2.5 rounded-full flex-shrink-0"
                      style={{ backgroundColor: routine.category.color }}
                    />
                    <span className="flex-1 min-w-0 text-sm font-medium text-foreground truncate">
                      {routine.title}
                    </span>
                    {/* The category pill carries the color; the name is
                        hidden for the untracked sink */}
                    {!routine.category.is_untracked && (
                      <span className="flex-shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
                        {routine.category.title}
                      </span>
                    )}
                    <div className="flex flex-shrink-0 gap-1">
                      <button
                        onClick={() => openEdit(routine)}
                        className="p-1.5 rounded-md hover:bg-muted transition-colors"
                        aria-label={`Edit ${routine.title}`}
                        title="Edit"
                      >
                        <Pencil className="h-3.5 w-3.5 text-muted-foreground" />
                      </button>
                      <button
                        onClick={() => handleDelete(routine)}
                        className="p-1.5 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors"
                        aria-label={`Delete ${routine.title}`}
                        title="Delete"
                      >
                        <Trash2 className="h-3.5 w-3.5 text-muted-foreground" />
                      </button>
                    </div>
                  </div>
                  <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                    <span>{rruleSummary(routine.rrule)}</span>
                    <span>{routine.estimated_minutes} min</span>
                    {nextDates.length > 0 && (
                      <span className="text-muted-foreground/80">
                        next: {nextDates.slice(0, 3).join(' · ')}
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* New / Edit Routine Dialog */}
      <Dialog
        open={form !== null}
        onOpenChange={(open) => !open && closeForm()}
      >
        <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden p-0 sm:max-w-[560px] bg-card border-border">
          <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
            <DialogTitle className="text-foreground">
              {form?.mode === 'edit' ? 'Edit Routine' : 'New Routine'}
            </DialogTitle>
            <DialogDescription>
              {form?.mode === 'edit'
                ? 'Update the standing definition — future occurrences follow it.'
                : 'A standing commitment that repeats — it seeds Home, never the Board.'}
            </DialogDescription>
          </DialogHeader>
          <hr className="shrink-0 border-border" />
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 pb-4">
            {/* Title + classify hint */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Title
              </label>
              <input
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                onBlur={() => setClassifyTitle(title.trim() || null)}
                placeholder="e.g. Morning Salat"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              {classifyStatus.state === 'matched' && (
                <p className="text-xs text-emerald-700">
                  Files to{' '}
                  <span
                    className="inline-block h-2 w-2 rounded-full align-middle"
                    style={{ backgroundColor: classifyStatus.category.color }}
                  />{' '}
                  {classifyStatus.category.title}
                </p>
              )}
              {classifyStatus.state === 'nomatch' && (
                <p className="text-xs text-destructive">
                  No category matches — save is disabled
                </p>
              )}
              {classifyStatus.state === 'conflict' && (
                <p className="text-xs text-destructive">
                  Title matches multiple categories — be more specific
                </p>
              )}
            </div>

            {/* Estimate */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Estimated minutes
              </label>
              <input
                type="number"
                min={1}
                value={estimatedMinutes}
                onChange={(e) => setEstimatedMinutes(e.target.value)}
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
              />
              {!estimateValid && (
                <p className="text-xs text-destructive">
                  Must be at least 1 minute
                </p>
              )}
            </div>

            {/* dtstart */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                First starts at
              </label>
              <div className="flex items-center gap-3">
                <input
                  type="date"
                  value={dtstartDate}
                  onChange={(e) => setDtstartDate(e.target.value)}
                  className="flex-1 min-w-0 px-4 py-3 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
                <input
                  type="time"
                  value={dtstartTime}
                  onChange={(e) => setDtstartTime(e.target.value)}
                  className="flex-1 min-w-0 px-4 py-3 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
              </div>
              <p className="text-xs text-muted-foreground">
                Stored as a local civil datetime ({dtstartDate}T{dtstartTime}
                :00) — no timezone involved
              </p>
            </div>

            {/* Frequency builder */}
            <div className="space-y-3 mb-5 rounded-xl border border-input bg-background p-4">
              <div>
                <label className="text-sm font-medium text-foreground">
                  Repeats
                </label>
                <div className="flex gap-2 mt-2">
                  {(['DAILY', 'WEEKLY'] as const).map((option) => (
                    <button
                      key={option}
                      type="button"
                      onClick={() => switchFreq(option)}
                      className={`flex-1 px-4 py-2 rounded-lg text-sm font-medium border transition-colors ${
                        freq === option
                          ? 'bg-primary/10 text-primary border-primary'
                          : 'bg-background text-muted-foreground border-input hover:bg-muted'
                      }`}
                    >
                      {option === 'DAILY' ? 'Daily' : 'Weekly'}
                    </button>
                  ))}
                </div>
              </div>

              <div className="flex items-center gap-3">
                <label className="text-sm text-muted-foreground">
                  Every
                </label>
                <input
                  type="number"
                  min={1}
                  value={intervalText}
                  onChange={(e) => setIntervalText(e.target.value)}
                  className="w-20 px-3 py-2 rounded-lg border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
                <span className="text-sm text-muted-foreground">
                  {freq === 'DAILY' ? 'day(s)' : 'week(s)'}
                </span>
              </div>

              {freq === 'WEEKLY' && (
                <div>
                  <label className="text-sm text-muted-foreground">
                    On days
                  </label>
                  <div className="flex gap-1.5 mt-1.5">
                    {WEEKDAY_CODES.map((code) => (
                      <button
                        key={code}
                        type="button"
                        onClick={() =>
                          setWeeklyDays((prev) => ({
                            ...prev,
                            [code]: !prev[code],
                          }))
                        }
                        aria-pressed={weeklyDays[code]}
                        className={`flex-1 px-2 py-2 rounded-lg text-xs font-medium border transition-colors ${
                          weeklyDays[code]
                            ? 'bg-primary/10 text-primary border-primary'
                            : 'bg-background text-muted-foreground border-input hover:bg-muted'
                        }`}
                      >
                        {WEEKDAY_LABELS[code]}
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>

            {/* Ends */}
            <div className="space-y-3 mb-5 rounded-xl border border-input bg-background p-4">
              <label className="text-sm font-medium text-foreground">
                Ends
              </label>
              <div className="flex gap-2">
                {(
                  [
                    ['never', 'Never'],
                    ['until', 'On date'],
                    ['count', 'After N'],
                  ] as const
                ).map(([mode, label]) => (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setUntilMode(mode)}
                    className={`flex-1 px-3 py-2 rounded-lg text-xs font-medium border transition-colors ${
                      untilMode === mode
                        ? 'bg-primary/10 text-primary border-primary'
                        : 'bg-background text-muted-foreground border-input hover:bg-muted'
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {untilMode === 'until' && (
                <input
                  type="date"
                  value={untilDate}
                  onChange={(e) => setUntilDate(e.target.value)}
                  className="w-full px-3 py-2 rounded-lg border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
              )}
              {untilMode === 'count' && (
                <div className="flex items-center gap-3">
                  <input
                    type="number"
                    min={1}
                    value={countText}
                    onChange={(e) => setCountText(e.target.value)}
                    className="w-20 px-3 py-2 rounded-lg border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                  />
                  <span className="text-sm text-muted-foreground">
                    occurrence(s)
                  </span>
                </div>
              )}
            </div>

            {/* Raw override */}
            <div className="space-y-2 mb-5">
              <label className="text-sm font-medium text-foreground">
                Raw rule{' '}
                <span className="text-muted-foreground font-normal">
                  (optional — wins over the builder)
                </span>
              </label>
              <input
                type="text"
                value={rawOverride}
                onChange={(e) => setRawOverride(e.target.value)}
                placeholder="e.g. FREQ=MONTHLY;BYMONTHDAY=1"
                className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all font-mono text-sm"
              />
              <p className="text-xs text-muted-foreground">
                The rule body only — never a{' '}
                <code className="font-mono">RRULE:</code> or{' '}
                <code className="font-mono">DTSTART:</code> prefix
              </p>
            </div>

            {/* Live preview */}
            <div className="space-y-1 mb-5 rounded-xl border border-border bg-background p-4">
              <p className="text-xs font-medium text-muted-foreground">
                Recurrence (stored as one blob)
              </p>
              <p className="text-sm font-mono text-foreground break-all">
                {rruleBlob}
              </p>
              {!ruleValid && (
                <p className="text-xs text-destructive">
                  This rule cannot be parsed with the first start — save is
                  disabled
                </p>
              )}
              <p className="text-xs font-medium text-muted-foreground mt-3">
                Next occurrences
              </p>
              {previewDates.length > 0 ? (
                <p className="text-sm text-foreground">
                  {previewDates.join(' · ')}
                </p>
              ) : (
                <p className="text-sm text-muted-foreground italic">
                  No occurrences in the next 30 days
                </p>
              )}
            </div>

            {formError && (
              <p className="mb-4 text-sm text-destructive">{formError}</p>
            )}
          </div>

          <div className="flex shrink-0 justify-end gap-3 border-t border-border px-6 py-4">
            <Button
              variant="outline"
              onClick={closeForm}
              className="border-input text-foreground hover:bg-muted"
            >
              Cancel
            </Button>
            <Button
              onClick={handleSubmit}
              disabled={!canSave}
              className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {saving && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
              {form?.mode === 'edit' ? 'Save Changes' : 'Create Routine'}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
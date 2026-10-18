// The create / edit dialog. The form is a flat `RuleFormValues` bag (see
// rules-model) so switching kinds never loses what the other kinds already
// hold; `buildRuleParams` turns it into the exact body the server accepts.
//
// On edit, `params` hydrates from `rule.params` — the revision the scoreboard
// scored at its `as_of`. Malformed params show an error inside the dialog and
// leave the board mounted.

import { useEffect, useMemo, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { useCategoriesQuery } from '@/app/queries/categories';
import type { RuleKind, RuleParams, RuleScore } from '@/app/types';
import { cn } from '@/lib/utils';
import {
  COMPARATOR_LABELS,
  KIND_HINTS,
  KIND_LABELS,
  RULE_ICONS,
  RULE_KINDS,
  SLEEP_METRIC_LABELS,
  WEEKDAY_LABELS,
  buildRuleParams,
  categoryOptions,
  emptyRuleForm,
  ruleFormValuesFromRule,
  sleepTargetLabel,
} from './rules-model';
import type { CategoryOption, RuleFormValues } from './rules-model';

const INPUT_CLASS =
  'w-full rounded-xl border border-input bg-background px-4 py-3 text-foreground placeholder:text-muted-foreground/60 focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20 transition-all';

export interface RuleSubmitPayload {
  kind: RuleKind;
  name: string;
  icon: string;
  /** `YYYY-MM-DD` or `""` for an open-ended rule. */
  endsOn: string;
  params: RuleParams;
}

export function RuleDialog({
  open,
  rule,
  pending,
  onClose,
  onSubmit,
}: {
  open: boolean;
  /** The edit target; `null` = create. */
  rule: RuleScore | null;
  pending: boolean;
  onClose: () => void;
  onSubmit: (payload: RuleSubmitPayload) => Promise<void>;
}) {
  const [values, setValues] = useState<RuleFormValues>(emptyRuleForm);
  const [error, setError] = useState<string | null>(null);
  // Same gating as the calendars picker: the categories are read only while
  // the dialog is open, and `useCategoriesQuery` works without the lists gate.
  const categoriesQuery = useCategoriesQuery({ enabled: open });
  const options = useMemo(
    () => categoryOptions(categoriesQuery.data?.categories ?? []),
    [categoriesQuery.data],
  );

  useEffect(() => {
    if (!open) return;
    if (rule === null) {
      setValues(emptyRuleForm());
      setError(null);
      return;
    }
    try {
      setValues(ruleFormValuesFromRule(rule));
      setError(null);
    } catch (err) {
      // Keep the identity fields so the user can still see which rule failed.
      setValues({
        ...emptyRuleForm(),
        kind: rule.kind,
        name: rule.name,
        icon: rule.icon,
      });
      setError(
        err instanceof Error
          ? `${err.message} — re-create the rule to change them`
          : 'Stored params are malformed',
      );
    }
  }, [open, rule]);

  const set = <K extends keyof RuleFormValues>(
    key: K,
    value: RuleFormValues[K],
  ) => setValues((previous) => ({ ...previous, [key]: value }));

  const toggleWeekday = (day: number) => {
    setValues((previous) => ({
      ...previous,
      weekdays: previous.weekdays.includes(day)
        ? previous.weekdays.filter((checked) => checked !== day)
        : [...previous.weekdays, day],
    }));
  };

  const toggleId = (key: 'categoryIds' | 'excludeIds', id: string) => {
    setValues((previous) => ({
      ...previous,
      [key]: previous[key].includes(id)
        ? previous[key].filter((current) => current !== id)
        : [...previous[key], id],
    }));
  };

  const handleSubmit = async () => {
    if (pending) return;
    const name = values.name.trim();
    if (name === '') {
      setError('Name is required');
      return;
    }
    let params: RuleParams;
    try {
      params = buildRuleParams(values);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Check the fields');
      return;
    }
    setError(null);
    try {
      await onSubmit({
        kind: values.kind,
        name,
        icon: values.icon,
        endsOn: values.endsOn,
        params,
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Save failed');
    }
  };

  const isEdit = rule !== null;

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden border-border bg-card p-0 sm:max-w-[600px]">
        <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
          <DialogTitle className="text-foreground">
            {isEdit ? 'Edit rule' : 'New rule'}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? 'Changing the params opens a new revision from today — history keeps the old one.'
              : 'A rule scores itself from your diary every day.'}
          </DialogDescription>
        </DialogHeader>
        <hr className="shrink-0 border-border" />
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 pb-4">
          {/* Kind — picked once on create, fixed after (the server rejects a
              change). */}
          <div className="mb-5 space-y-2">
            <label className="text-sm font-medium text-foreground">Kind</label>
            {isEdit ? (
              <div className="rounded-xl border border-input bg-background px-4 py-3">
                <p className="text-sm text-foreground">
                  {KIND_LABELS[values.kind]}
                </p>
                <p className="text-xs text-muted-foreground">
                  {KIND_HINTS[values.kind]} — kind is fixed once created
                </p>
              </div>
            ) : (
              <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                {RULE_KINDS.map((kind) => (
                  <button
                    key={kind}
                    type="button"
                    onClick={() => set('kind', kind)}
                    aria-pressed={values.kind === kind}
                    className={cn(
                      'rounded-lg border px-3 py-2 text-left transition-colors',
                      values.kind === kind
                        ? 'border-primary bg-primary/10'
                        : 'border-input bg-background hover:bg-muted',
                    )}
                  >
                    <span
                      className={cn(
                        'block text-xs font-medium',
                        values.kind === kind
                          ? 'text-primary'
                          : 'text-foreground',
                      )}
                    >
                      {KIND_LABELS[kind]}
                    </span>
                    <span className="block text-[10px] text-muted-foreground">
                      {KIND_HINTS[kind]}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className="mb-5 space-y-2">
            <label className="text-sm font-medium text-foreground">Name</label>
            <input
              type="text"
              value={values.name}
              onChange={(event) => set('name', event.target.value)}
              placeholder="e.g. Fitness"
              className={INPUT_CLASS}
            />
          </div>

          <div className="mb-5 grid grid-cols-2 gap-3">
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                Icon
              </label>
              <select
                value={values.icon}
                onChange={(event) => set('icon', event.target.value)}
                className={INPUT_CLASS}
              >
                <option value="">No icon</option>
                {RULE_ICONS.map((icon) => (
                  <option key={icon} value={icon}>
                    {icon}
                  </option>
                ))}
              </select>
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                Ends on
              </label>
              <input
                type="date"
                value={values.endsOn}
                onChange={(event) => set('endsOn', event.target.value)}
                className={INPUT_CLASS}
              />
            </div>
          </div>
          <p className="-mt-3 mb-5 text-xs text-muted-foreground">
            An end date makes it a challenge — the last day is judged like any
            other.
          </p>

          {/* Kind fields */}
          <div className="mb-5 rounded-xl border border-input bg-background p-4">
            {(values.kind === 'category_total' ||
              values.kind === 'category_days' ||
              values.kind === 'category_daily_avg') && (
              <>
                {values.kind === 'category_daily_avg' && (
                  <label className="mb-3 flex cursor-pointer items-center gap-2">
                    <input
                      type="checkbox"
                      checked={values.useAllProductive}
                      onChange={(event) =>
                        set('useAllProductive', event.target.checked)
                      }
                      className="h-4 w-4 rounded border-input accent-primary"
                    />
                    <span className="text-sm text-foreground">
                      All productive categories
                    </span>
                  </label>
                )}
                {!(
                  values.kind === 'category_daily_avg' &&
                  values.useAllProductive
                ) && (
                  <div className="mb-3">
                    <label className="mb-2 block text-sm font-medium text-foreground">
                      Categories
                    </label>
                    <CategoryChecklist
                      options={options}
                      selected={values.categoryIds}
                      onToggle={(id) => toggleId('categoryIds', id)}
                      loading={categoriesQuery.isLoading}
                    />
                    <label className="mt-2 flex cursor-pointer items-center gap-2">
                      <input
                        type="checkbox"
                        checked={values.includeChildren}
                        onChange={(event) =>
                          set('includeChildren', event.target.checked)
                        }
                        className="h-4 w-4 rounded border-input accent-primary"
                      />
                      <span className="text-sm text-foreground">
                        Include their sub-categories
                      </span>
                    </label>
                  </div>
                )}

                {values.kind === 'category_daily_avg' && (
                  <div className="mb-3">
                    <label className="mb-2 block text-sm font-medium text-foreground">
                      Weekdays
                    </label>
                    <div className="flex gap-1.5">
                      {WEEKDAY_LABELS.map((label, day) => (
                        <button
                          key={label}
                          type="button"
                          onClick={() => toggleWeekday(day)}
                          aria-pressed={values.weekdays.includes(day)}
                          className={cn(
                            'flex-1 rounded-lg border px-2 py-2 text-xs font-medium transition-colors',
                            values.weekdays.includes(day)
                              ? 'border-primary bg-primary/10 text-primary'
                              : 'border-input bg-background text-muted-foreground hover:bg-muted',
                          )}
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Window (days)
                    </label>
                    <input
                      type="number"
                      min={1}
                      max={366}
                      value={values.windowDays}
                      onChange={(event) =>
                        set('windowDays', event.target.value)
                      }
                      className={INPUT_CLASS}
                    />
                  </div>
                  {values.kind === 'category_total' ||
                  values.kind === 'category_daily_avg' ? (
                    <div className="space-y-2">
                      <label className="text-sm font-medium text-foreground">
                        Target hours
                      </label>
                      <input
                        type="number"
                        min={0}
                        step="0.5"
                        value={values.targetHours}
                        onChange={(event) =>
                          set('targetHours', event.target.value)
                        }
                        className={INPUT_CLASS}
                      />
                    </div>
                  ) : (
                    <div className="space-y-2">
                      <label className="text-sm font-medium text-foreground">
                        Min minutes a day
                      </label>
                      <input
                        type="number"
                        min={1}
                        value={values.minMinutes}
                        onChange={(event) =>
                          set('minMinutes', event.target.value)
                        }
                        className={INPUT_CLASS}
                      />
                    </div>
                  )}
                </div>

                {values.kind === 'category_total' && (
                  <div className="mt-3 space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Comparator
                    </label>
                    <select
                      value={values.comparator}
                      onChange={(event) =>
                        set(
                          'comparator',
                          event.target.value === 'lte' ? 'lte' : 'gte',
                        )
                      }
                      className={INPUT_CLASS}
                    >
                      {(
                        Object.keys(COMPARATOR_LABELS) as ('gte' | 'lte')[]
                      ).map((comparator) => (
                        <option key={comparator} value={comparator}>
                          {COMPARATOR_LABELS[comparator]}
                        </option>
                      ))}
                    </select>
                  </div>
                )}

                {values.kind === 'category_days' && (
                  <div className="mt-3 space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Min days in the window
                    </label>
                    <input
                      type="number"
                      min={1}
                      value={values.minDays}
                      onChange={(event) => set('minDays', event.target.value)}
                      className={INPUT_CLASS}
                    />
                  </div>
                )}

                {values.kind === 'category_daily_avg' && (
                  <div className="mt-4 border-t border-border pt-3">
                    <label className="flex cursor-pointer items-center gap-2">
                      <input
                        type="checkbox"
                        checked={values.excludeEnabled}
                        onChange={(event) =>
                          set('excludeEnabled', event.target.checked)
                        }
                        className="h-4 w-4 rounded border-input accent-primary"
                      />
                      <span className="text-sm text-foreground">
                        Exclude days
                      </span>
                      <span className="text-xs text-muted-foreground">
                        (the travel escape hatch)
                      </span>
                    </label>
                    {values.excludeEnabled && (
                      <div className="mt-3">
                        <label className="flex cursor-pointer items-center gap-2">
                          <input
                            type="checkbox"
                            checked={values.excludeAllProductive}
                            onChange={(event) =>
                              set('excludeAllProductive', event.target.checked)
                            }
                            className="h-4 w-4 rounded border-input accent-primary"
                          />
                          <span className="text-sm text-foreground">
                            All productive categories
                          </span>
                        </label>
                        {!values.excludeAllProductive && (
                          <>
                            <CategoryChecklist
                              options={options}
                              selected={values.excludeIds}
                              onToggle={(id) => toggleId('excludeIds', id)}
                              loading={categoriesQuery.isLoading}
                            />
                            <label className="mt-2 flex cursor-pointer items-center gap-2">
                              <input
                                type="checkbox"
                                checked={values.excludeIncludeChildren}
                                onChange={(event) =>
                                  set(
                                    'excludeIncludeChildren',
                                    event.target.checked,
                                  )
                                }
                                className="h-4 w-4 rounded border-input accent-primary"
                              />
                              <span className="text-sm text-foreground">
                                Include their sub-categories
                              </span>
                            </label>
                          </>
                        )}
                        <div className="mt-3 space-y-2">
                          <label className="text-sm font-medium text-foreground">
                            Exclude above (% of waking)
                          </label>
                          <input
                            type="number"
                            min={1}
                            max={100}
                            value={values.excludePercent}
                            onChange={(event) =>
                              set('excludePercent', event.target.value)
                            }
                            className={INPUT_CLASS}
                          />
                        </div>
                      </div>
                    )}
                  </div>
                )}
              </>
            )}

            {(values.kind === 'sleep_duration' ||
              values.kind === 'sleep_timing') && (
              <>
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Window (days)
                    </label>
                    <input
                      type="number"
                      min={1}
                      max={366}
                      value={values.windowDays}
                      onChange={(event) =>
                        set('windowDays', event.target.value)
                      }
                      className={INPUT_CLASS}
                    />
                  </div>
                  <div className="space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Min paired nights
                    </label>
                    <input
                      type="number"
                      min={1}
                      value={values.minNights}
                      onChange={(event) => set('minNights', event.target.value)}
                      className={INPUT_CLASS}
                    />
                  </div>
                </div>

                {values.kind === 'sleep_duration' ? (
                  <div className="mt-3 space-y-2">
                    <label className="text-sm font-medium text-foreground">
                      Target hours
                    </label>
                    <input
                      type="number"
                      min={0}
                      step="0.5"
                      value={values.targetHours}
                      onChange={(event) =>
                        set('targetHours', event.target.value)
                      }
                      className={INPUT_CLASS}
                    />
                  </div>
                ) : (
                  <div className="mt-3 grid grid-cols-2 gap-3">
                    <div className="space-y-2">
                      <label className="text-sm font-medium text-foreground">
                        Metric
                      </label>
                      <select
                        value={values.metric}
                        onChange={(event) =>
                          set(
                            'metric',
                            event.target.value as RuleFormValues['metric'],
                          )
                        }
                        className={INPUT_CLASS}
                      >
                        {(
                          Object.keys(
                            SLEEP_METRIC_LABELS,
                          ) as RuleFormValues['metric'][]
                        ).map((metric) => (
                          <option key={metric} value={metric}>
                            {SLEEP_METRIC_LABELS[metric]}
                          </option>
                        ))}
                      </select>
                    </div>
                    <div className="space-y-2">
                      <label className="text-sm font-medium text-foreground">
                        Target minutes
                      </label>
                      <input
                        type="number"
                        min={0}
                        value={values.targetMinutes}
                        onChange={(event) =>
                          set('targetMinutes', event.target.value)
                        }
                        className={INPUT_CLASS}
                      />
                    </div>
                    <div className="col-span-2 space-y-2">
                      <label className="text-sm font-medium text-foreground">
                        Comparator
                      </label>
                      <select
                        value={values.comparator}
                        onChange={(event) =>
                          set(
                            'comparator',
                            event.target.value === 'gte' ? 'gte' : 'lte',
                          )
                        }
                        className={INPUT_CLASS}
                      >
                        <option value="lte">{COMPARATOR_LABELS.lte}</option>
                        <option value="gte">{COMPARATOR_LABELS.gte}</option>
                      </select>
                    </div>
                  </div>
                )}
                <p className="mt-3 text-xs text-muted-foreground">
                  {values.kind === 'sleep_timing' &&
                  values.metric === 'bedtime_median'
                    ? 'Bedtime minutes are counted past noon — 660 = 23:00.'
                    : values.kind === 'sleep_timing' &&
                        values.metric === 'wake_median'
                      ? 'Wake minutes are counted past midnight — 300 = 05:00.'
                      : 'Only nights with both an Asleep and a Woke Up entry score.'}
                  {values.kind === 'sleep_timing' &&
                    values.targetMinutes.trim() !== '' &&
                    Number.isFinite(Number(values.targetMinutes)) && (
                      <>
                        {' '}
                        Current target:{' '}
                        {sleepTargetLabel(
                          values.metric,
                          Number(values.targetMinutes),
                        )}
                        .
                      </>
                    )}
                </p>
              </>
            )}
          </div>

          {error && (
            <p className="selectable mb-4 text-sm text-destructive">{error}</p>
          )}
        </div>

        <div className="flex shrink-0 justify-end gap-3 border-t border-border px-6 py-4">
          <Button
            variant="outline"
            onClick={onClose}
            className="border-input text-foreground hover:bg-muted"
          >
            Cancel
          </Button>
          <Button
            onClick={handleSubmit}
            disabled={pending}
            className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {pending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {isEdit ? 'Save changes' : 'Create rule'}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function CategoryChecklist({
  options,
  selected,
  onToggle,
  loading,
}: {
  options: CategoryOption[];
  selected: string[];
  onToggle: (id: string) => void;
  loading: boolean;
}) {
  if (loading && options.length === 0) {
    return <p className="text-xs text-muted-foreground">Loading categories…</p>;
  }
  if (options.length === 0) {
    return (
      <p className="text-xs text-muted-foreground">
        No categories yet — add one from Lists first.
      </p>
    );
  }
  return (
    <div className="max-h-48 overflow-y-auto overscroll-contain rounded-xl border border-input bg-background p-1">
      {options.map((option) => (
        <label
          key={option.id}
          className={cn(
            'flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 hover:bg-muted',
            option.depth === 1 && 'ml-4',
          )}
        >
          <input
            type="checkbox"
            checked={selected.includes(option.id)}
            onChange={() => onToggle(option.id)}
            className="h-4 w-4 rounded border-input accent-primary"
          />
          <span
            className="h-2 w-2 shrink-0 rounded-full"
            style={{ backgroundColor: option.color }}
          />
          <span className="truncate text-sm text-foreground">
            {option.title}
          </span>
        </label>
      ))}
    </div>
  );
}

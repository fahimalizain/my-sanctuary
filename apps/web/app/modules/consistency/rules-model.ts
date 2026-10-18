// Pure helpers behind the Consistency board: strip painting, the rule form's
// params builder / hydrator, and the reorder id list. No React, no fetch — the
// page and dialogs import these, and the node test runner exercises them
// directly.
//
// Everything here mirrors `packages/api-core/src/rules/{params,score}.rs`.
// The params builder must send exactly the keys the Rust structs accept:
// they are parsed with `deny_unknown_fields`, so an extra key (or a `null`
// where the field is optional) is a 400, not a silent ignore.

import type {
  Category,
  CategoryDailyAvgParams,
  CategoryDaysParams,
  CategorySelector,
  CategoryTotalParams,
  RuleComparator,
  RuleKind,
  RuleParams,
  RuleScore,
  SleepDurationParams,
  SleepMetric,
  SleepTimingParams,
  StripState,
} from '@/app/types';

// ──────────────────────────────────────────
// Vocabulary
// ──────────────────────────────────────────

/** The five kinds, in the order the picker shows them. */
export const RULE_KINDS: RuleKind[] = [
  'category_total',
  'category_days',
  'category_daily_avg',
  'sleep_duration',
  'sleep_timing',
];

export const KIND_LABELS: Record<RuleKind, string> = {
  category_total: 'Total hours',
  category_days: 'Days hit',
  category_daily_avg: 'Weekday average',
  sleep_duration: 'Sleep duration',
  sleep_timing: 'Sleep timing',
};

export const KIND_HINTS: Record<RuleKind, string> = {
  category_total: 'Sum the categories over a window',
  category_days: 'Count the days that clear a minimum',
  category_daily_avg: 'Mean hours on the weekdays you pick',
  sleep_duration: 'Median length of your paired nights',
  sleep_timing: 'Bedtime or wake clock, median or spread',
};

/** Icons a rule can store (`rules.icon`; `""` = none). */
export const RULE_ICONS = [
  'fitness',
  'learning',
  'productive',
  'sleep',
  'bedtime',
  'wake',
] as const;

/** Monday-first short labels (`weekdays` is `0 = Monday … 6 = Sunday`). */
export const WEEKDAY_LABELS = ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su'];

export const SLEEP_METRIC_LABELS: Record<SleepMetric, string> = {
  bedtime_median: 'Bedtime — median',
  bedtime_stdev: 'Bedtime — spread',
  wake_median: 'Wake — median',
  wake_stdev: 'Wake — spread',
};

export const COMPARATOR_LABELS: Record<RuleComparator, string> = {
  gte: 'At least',
  lte: 'At most',
};

/** The phase pill text (board shows it only for `paused`). */
export function phaseLabel(phase: RuleScore['phase']): string {
  switch (phase) {
    case 'active':
      return 'Active';
    case 'paused':
      return 'Paused';
    case 'finished':
      return 'Finished';
    case 'archived':
      return 'Archived';
  }
}

// ──────────────────────────────────────────
// Verdict chip
// ──────────────────────────────────────────

/** The board chip's one word — never a PASS/FAIL stamp.
 *
 *  An incomplete window is `Building` before anything else: the API has no
 *  verdict to show yet, whatever the phase says. */
export function boardVerdict(
  rule: Pick<RuleScore, 'phase' | 'on_track' | 'window_full'>,
): string {
  if (!rule.window_full) return 'Building';
  if (rule.phase === 'paused') return 'Paused';
  return rule.on_track ? 'On track' : 'Behind';
}

/** The archive row's second, muted word: only a finished challenge whose
 *  window is fully computed has a verdict to report. */
export function archiveVerdict(
  rule: Pick<RuleScore, 'phase' | 'on_track' | 'window_full'>,
): string | null {
  if (rule.phase !== 'finished' || !rule.window_full) return null;
  return rule.on_track ? 'On track' : 'Behind';
}

// ──────────────────────────────────────────
// Strip
// ──────────────────────────────────────────

/** The Tailwind fragment for one strip cell state. */
export function stripClass(state: StripState): string {
  switch (state) {
    case 'pass':
      return 'bg-primary';
    case 'fail':
      return 'bg-destructive/20';
    case 'pending':
      return 'bg-transparent border border-primary';
    case 'paused':
      return 'bg-muted';
    case 'excluded':
      return 'bg-border/40';
    case 'off':
      return 'bg-transparent border border-border/40';
  }
}

// ──────────────────────────────────────────
// Reorder
// ──────────────────────────────────────────

/** Swaps `index` with its neighbour in `direction`, or `null` at an end. */
export function swapNeighbors(
  ids: string[],
  index: number,
  direction: -1 | 1,
): string[] | null {
  const target = index + direction;
  if (index < 0 || index >= ids.length || target < 0 || target >= ids.length) {
    return null;
  }
  const next = [...ids];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

/**
 * The body of `POST /api/rules/reorder`: the new board order, then every
 * archive rule id that is not already listed, in the archive's own
 * `sort_order`. The server 400s unless the list carries **every** living rule
 * exactly once, so a missing archive row must never be dropped silently.
 */
export function reorderIds(
  boardIds: string[],
  archiveRules: Pick<RuleScore, 'id' | 'sort_order'>[],
): string[] {
  const listed = new Set(boardIds);
  const missing = archiveRules
    .filter((rule) => !listed.has(rule.id))
    .sort(
      (left, right) =>
        left.sort_order - right.sort_order || left.id.localeCompare(right.id),
    );
  return [...boardIds, ...missing.map((rule) => rule.id)];
}

// ──────────────────────────────────────────
// Formatting (mirrors the Rust helpers)
// ──────────────────────────────────────────

/** Minutes past noon back to a wall clock `HH:MM` (`660` → `23:00`). */
function formatBedtime(minutes: number): string {
  return clock(minutes + 12 * 60);
}

/** Minutes past midnight as a wall clock `HH:MM` (`300` → `05:00`). */
function formatWake(minutes: number): string {
  return clock(minutes);
}

function clock(totalMinutes: number): string {
  const wrapped =
    ((Math.round(totalMinutes) % (24 * 60)) + 24 * 60) % (24 * 60);
  const hours = Math.floor(wrapped / 60);
  return `${String(hours).padStart(2, '0')}:${String(wrapped % 60).padStart(2, '0')}`;
}

/** The clock/spread reading of a sleep-timing target, for the form's hint. */
export function sleepTargetLabel(metric: SleepMetric, minutes: number): string {
  switch (metric) {
    case 'bedtime_median':
      return formatBedtime(minutes);
    case 'wake_median':
      return formatWake(minutes);
    case 'bedtime_stdev':
    case 'wake_stdev':
      return `${minutes} min spread`;
  }
}

/** The target hours in a `category_total` condition (`≥4h / 7d` → `4`). */
export function targetHoursFromCondition(condition: string): number | null {
  const match = condition.match(/^[≥≤]\s*(\d+(?:\.\d+)?)\s*h/);
  if (!match) return null;
  const hours = Number(match[1]);
  return Number.isFinite(hours) ? hours : null;
}

// ──────────────────────────────────────────
// Category picker
// ──────────────────────────────────────────

export interface CategoryOption {
  id: string;
  title: string;
  color: string;
  /** 0 = root, 1 = child (indented in the picker). */
  depth: 0 | 1;
}

/**
 * The picker's flat list: every root, each followed by its one-level children.
 * The `untracked` sink is a system row — it is never a rule's subject, so it
 * (and any child of it) is left out.
 */
export function categoryOptions(categories: Category[]): CategoryOption[] {
  const usable = categories.filter((category) => !category.is_untracked);
  const roots = usable.filter((category) => !category.parent_id);
  const out: CategoryOption[] = [];
  for (const root of roots) {
    out.push({
      id: root.id,
      title: root.title,
      color: root.color,
      depth: 0,
    });
    for (const child of usable) {
      if (child.parent_id === root.id) {
        out.push({
          id: child.id,
          title: child.title,
          color: child.color || root.color,
          depth: 1,
        });
      }
    }
  }
  return out;
}

// ──────────────────────────────────────────
// Form values ↔ params
// ──────────────────────────────────────────

/** The dialog's whole state. Numeric fields stay strings (exactly what the
 *  inputs hold); `buildRuleParams` parses and validates them. */
export interface RuleFormValues {
  kind: RuleKind;
  name: string;
  /** `""` = no icon. */
  icon: string;
  /** `YYYY-MM-DD` or `""` for an open-ended rule. */
  endsOn: string;
  /** daily-avg only: count every productive category. */
  useAllProductive: boolean;
  categoryIds: string[];
  includeChildren: boolean;
  windowDays: string;
  comparator: RuleComparator;
  targetHours: string;
  minMinutes: string;
  minDays: string;
  /** Checked weekdays, `0 = Monday … 6 = Sunday`. */
  weekdays: number[];
  excludeEnabled: boolean;
  excludeAllProductive: boolean;
  excludeIds: string[];
  excludeIncludeChildren: boolean;
  excludePercent: string;
  minNights: string;
  metric: SleepMetric;
  targetMinutes: string;
}

/** A blank create form: `category_days`, include-children on, Mon–Fri. */
export function emptyRuleForm(): RuleFormValues {
  return {
    kind: 'category_days',
    name: '',
    icon: '',
    endsOn: '',
    useAllProductive: false,
    categoryIds: [],
    includeChildren: true,
    windowDays: '7',
    comparator: 'gte',
    targetHours: '4',
    minMinutes: '60',
    minDays: '5',
    weekdays: [0, 1, 2, 3, 4],
    excludeEnabled: false,
    excludeAllProductive: false,
    excludeIds: [],
    excludeIncludeChildren: true,
    excludePercent: '50',
    minNights: '14',
    metric: 'bedtime_median',
    targetMinutes: '660',
  };
}

/**
 * The `params` body for `POST` / `PATCH`, built from the form. Throws an
 * `Error` whose message is safe to show inside the dialog when a required
 * field is missing or a number is not a number.
 */
export function buildRuleParams(values: RuleFormValues): RuleParams {
  switch (values.kind) {
    case 'category_total':
      return {
        categories: idSelector(values),
        window_days: requireNumber(values.windowDays, 'Window days', 1),
        comparator: values.comparator,
        target_hours: requireNumber(values.targetHours, 'Target hours', 0),
      } satisfies CategoryTotalParams;
    case 'category_days':
      return {
        categories: idSelector(values),
        window_days: requireNumber(values.windowDays, 'Window days', 1),
        min_minutes: requireNumber(values.minMinutes, 'Min minutes', 1),
        min_days: requireNumber(values.minDays, 'Min days', 1),
      } satisfies CategoryDaysParams;
    case 'category_daily_avg': {
      const weekdays = [...new Set(values.weekdays)].sort(
        (left, right) => left - right,
      );
      if (weekdays.length === 0) {
        throw new Error('Pick at least one weekday');
      }
      const params: CategoryDailyAvgParams = {
        categories: values.useAllProductive
          ? 'all_productive'
          : idSelector(values),
        window_days: requireNumber(values.windowDays, 'Window days', 1),
        weekdays,
        target_hours: requireNumber(values.targetHours, 'Target hours', 0),
      };
      // `exclude_if` is omitted — never sent as null — when the section is off
      // or selects nothing.
      const exclude = excludeSelector(values);
      if (exclude) {
        params.exclude_if = {
          categories: exclude,
          percent_of_waking: requireNumber(
            values.excludePercent,
            'Exclude percent',
            1,
          ),
        };
      }
      return params;
    }
    case 'sleep_duration':
      return {
        window_days: requireNumber(values.windowDays, 'Window days', 1),
        min_nights: requireNumber(values.minNights, 'Min nights', 1),
        target_hours: requireNumber(values.targetHours, 'Target hours', 0),
      } satisfies SleepDurationParams;
    case 'sleep_timing':
      return {
        window_days: requireNumber(values.windowDays, 'Window days', 1),
        min_nights: requireNumber(values.minNights, 'Min nights', 1),
        metric: values.metric,
        comparator: values.comparator,
        target_minutes: requireNumber(
          values.targetMinutes,
          'Target minutes',
          0,
        ),
      } satisfies SleepTimingParams;
  }
}

function idSelector(values: RuleFormValues): CategorySelector {
  if (values.categoryIds.length === 0) {
    throw new Error('Pick at least one category');
  }
  return {
    ids: values.categoryIds,
    include_children: values.includeChildren,
  };
}

function excludeSelector(values: RuleFormValues): CategorySelector | null {
  if (!values.excludeEnabled) return null;
  if (values.excludeAllProductive) return 'all_productive';
  if (values.excludeIds.length === 0) return null;
  return {
    ids: values.excludeIds,
    include_children: values.excludeIncludeChildren,
  };
}

function requireNumber(text: string, label: string, min: number): number {
  const value = Number(text);
  if (text.trim() === '' || !Number.isFinite(value)) {
    throw new Error(`${label} must be a number`);
  }
  if (value < min) {
    throw new Error(`${label} must be at least ${min}`);
  }
  return value;
}

/**
 * Hydrates the form from a rule's stored `params` (the revision the scoreboard
 * sent). Throws a readable `Error` when the shape does not match the kind — a
 * legacy or hand-edited row must show the error, not crash the page.
 */
export function ruleFormValuesFromRule(rule: RuleScore): RuleFormValues {
  const params = parseRuleParams(rule.kind, rule.params);
  const values: RuleFormValues = {
    ...emptyRuleForm(),
    kind: rule.kind,
    name: rule.name,
    icon: rule.icon,
    endsOn: rule.ends_on ?? '',
  };
  switch (rule.kind) {
    case 'category_total': {
      const total = params as CategoryTotalParams;
      return {
        ...values,
        ...selectorFields(total.categories),
        windowDays: String(total.window_days),
        comparator: total.comparator,
        targetHours: String(total.target_hours),
      };
    }
    case 'category_days': {
      const days = params as CategoryDaysParams;
      return {
        ...values,
        ...selectorFields(days.categories),
        windowDays: String(days.window_days),
        minMinutes: String(days.min_minutes),
        minDays: String(days.min_days),
      };
    }
    case 'category_daily_avg': {
      const avg = params as CategoryDailyAvgParams;
      const exclude = avg.exclude_if;
      return {
        ...values,
        ...selectorFields(avg.categories),
        windowDays: String(avg.window_days),
        targetHours: String(avg.target_hours),
        weekdays: [...avg.weekdays].sort((left, right) => left - right),
        excludeEnabled: exclude !== undefined,
        excludeAllProductive: exclude?.categories === 'all_productive',
        excludeIds:
          exclude && exclude.categories !== 'all_productive'
            ? exclude.categories.ids
            : [],
        excludeIncludeChildren:
          exclude && exclude.categories !== 'all_productive'
            ? exclude.categories.include_children
            : true,
        excludePercent:
          exclude === undefined ? '50' : String(exclude.percent_of_waking),
      };
    }
    case 'sleep_duration': {
      const duration = params as SleepDurationParams;
      return {
        ...values,
        windowDays: String(duration.window_days),
        minNights: String(duration.min_nights),
        targetHours: String(duration.target_hours),
      };
    }
    case 'sleep_timing': {
      const timing = params as SleepTimingParams;
      return {
        ...values,
        windowDays: String(timing.window_days),
        minNights: String(timing.min_nights),
        metric: timing.metric,
        comparator: timing.comparator,
        targetMinutes: String(timing.target_minutes),
      };
    }
  }
}

/** The id-list half of the form, from a stored selector. */
function selectorFields(
  selector: CategorySelector,
): Pick<
  RuleFormValues,
  'useAllProductive' | 'categoryIds' | 'includeChildren'
> {
  if (selector === 'all_productive') {
    return { useAllProductive: true, categoryIds: [], includeChildren: false };
  }
  return {
    useAllProductive: false,
    categoryIds: selector.ids,
    includeChildren: selector.include_children,
  };
}

/**
 * Validates a stored `params` value against its kind. Throws a readable
 * `Error` — callers show the message and keep the board mounted.
 */
export function parseRuleParams(kind: RuleKind, value: unknown): RuleParams {
  const object = asObject(value, 'Stored params');
  switch (kind) {
    case 'category_total':
      return {
        categories: parseSelector(object.categories),
        window_days: numberField(object, 'window_days'),
        comparator: comparatorField(object),
        target_hours: numberField(object, 'target_hours'),
      };
    case 'category_days':
      return {
        categories: parseSelector(object.categories),
        window_days: numberField(object, 'window_days'),
        min_minutes: numberField(object, 'min_minutes'),
        min_days: numberField(object, 'min_days'),
      };
    case 'category_daily_avg': {
      const params: CategoryDailyAvgParams = {
        categories: parseSelector(object.categories),
        window_days: numberField(object, 'window_days'),
        weekdays: numberArrayField(object, 'weekdays'),
        target_hours: numberField(object, 'target_hours'),
      };
      if (object.exclude_if !== undefined && object.exclude_if !== null) {
        const exclude = asObject(object.exclude_if, 'Stored exclude_if');
        params.exclude_if = {
          categories: parseSelector(exclude.categories),
          percent_of_waking: numberField(exclude, 'percent_of_waking'),
        };
      }
      return params;
    }
    case 'sleep_duration':
      return {
        window_days: numberField(object, 'window_days'),
        min_nights: numberField(object, 'min_nights'),
        target_hours: numberField(object, 'target_hours'),
      };
    case 'sleep_timing':
      return {
        window_days: numberField(object, 'window_days'),
        min_nights: numberField(object, 'min_nights'),
        metric: metricField(object),
        comparator: comparatorField(object),
        target_minutes: numberField(object, 'target_minutes'),
      };
  }
}

function asObject(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error(`${label} are missing or malformed`);
  }
  return value as Record<string, unknown>;
}

function numberField(object: Record<string, unknown>, key: string): number {
  const value = object[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`Stored params are missing ${key}`);
  }
  return value;
}

function numberArrayField(
  object: Record<string, unknown>,
  key: string,
): number[] {
  const value = object[key];
  if (
    !Array.isArray(value) ||
    !value.every((item) => typeof item === 'number')
  ) {
    throw new Error(`Stored params are missing ${key}`);
  }
  return value;
}

function parseSelector(value: unknown): CategorySelector {
  if (value === 'all_productive') return 'all_productive';
  if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
    const object = value as Record<string, unknown>;
    const ids = object.ids;
    if (Array.isArray(ids) && ids.every((id) => typeof id === 'string')) {
      return { ids, include_children: object.include_children === true };
    }
  }
  throw new Error('Stored params have a malformed category selector');
}

function comparatorField(object: Record<string, unknown>): RuleComparator {
  const value = object.comparator;
  if (value === 'gte' || value === 'lte') return value;
  throw new Error('Stored params have a malformed comparator');
}

function metricField(object: Record<string, unknown>): SleepMetric {
  const value = object.metric;
  if (
    value === 'bedtime_median' ||
    value === 'bedtime_stdev' ||
    value === 'wake_median' ||
    value === 'wake_stdev'
  ) {
    return value;
  }
  throw new Error('Stored params have a malformed sleep metric');
}

import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { CategoryDailyAvgParams, RuleScore } from '../../types';
import {
  buildRuleParams,
  emptyRuleForm,
  formatBedtime,
  formatWake,
  parseRuleParams,
  reorderIds,
  ruleFormValuesFromRule,
  stripClass,
  swapNeighbors,
  targetHoursFromCondition,
} from './rules-model';

// ── stripClass ───────────────────────────────────────────────────────────

test('stripClass: pass paints primary', () => {
  assert.ok(stripClass('pass').includes('bg-primary'));
});

test('stripClass: fail is a faint destructive', () => {
  assert.ok(stripClass('fail').includes('bg-destructive/20'));
});

test('stripClass: pending is an outline, not a fill', () => {
  const className = stripClass('pending');
  assert.ok(className.includes('border-primary'));
  assert.ok(className.includes('bg-transparent'));
});

test('stripClass: paused is muted, excluded is faint border, off is a faint outline', () => {
  assert.equal(stripClass('paused'), 'bg-muted');
  assert.equal(stripClass('excluded'), 'bg-border/40');
  assert.ok(stripClass('off').includes('border-border/40'));
});

// ── buildRuleParams ──────────────────────────────────────────────────────

test('buildRuleParams: a fitness-style days rule', () => {
  const params = buildRuleParams({
    ...emptyRuleForm(),
    kind: 'category_days',
    categoryIds: ['cat-fitness'],
    includeChildren: true,
    windowDays: '60',
    minMinutes: '60',
    minDays: '12',
  });
  assert.deepEqual(params, {
    categories: { ids: ['cat-fitness'], include_children: true },
    window_days: 60,
    min_minutes: 60,
    min_days: 12,
  });
});

test('buildRuleParams: include_children is always sent explicitly', () => {
  const params = buildRuleParams({
    ...emptyRuleForm(),
    kind: 'category_days',
    categoryIds: ['cat-fitness'],
    includeChildren: false,
  }) as { categories: unknown };
  assert.deepEqual(params.categories, {
    ids: ['cat-fitness'],
    include_children: false,
  });
});

test('buildRuleParams: daily average with no exclude has no exclude_if key', () => {
  const params = buildRuleParams({
    ...emptyRuleForm(),
    kind: 'category_daily_avg',
    categoryIds: ['cat-work'],
    includeChildren: true,
    windowDays: '60',
    weekdays: [4, 0, 1, 0],
    targetHours: '7',
  }) as CategoryDailyAvgParams;
  assert.equal('exclude_if' in params, false);
  // Deduped and sorted — the server normalizes the same way.
  assert.deepEqual(params.weekdays, [0, 1, 4]);
  assert.deepEqual(params, {
    categories: { ids: ['cat-work'], include_children: true },
    window_days: 60,
    weekdays: [0, 1, 4],
    target_hours: 7,
  });
});

test('buildRuleParams: daily average exclude_if is sent when the section selects something', () => {
  const params = buildRuleParams({
    ...emptyRuleForm(),
    kind: 'category_daily_avg',
    useAllProductive: true,
    windowDays: '60',
    targetHours: '7',
    excludeEnabled: true,
    excludeIds: ['cat-travel'],
    excludeIncludeChildren: true,
    excludePercent: '40',
  }) as CategoryDailyAvgParams;
  assert.equal(params.categories, 'all_productive');
  assert.deepEqual(params.exclude_if, {
    categories: { ids: ['cat-travel'], include_children: true },
    percent_of_waking: 40,
  });
});

test('buildRuleParams: an enabled exclude section with nothing picked is omitted', () => {
  const params = buildRuleParams({
    ...emptyRuleForm(),
    kind: 'category_daily_avg',
    categoryIds: ['cat-work'],
    excludeEnabled: true,
    excludeIds: [],
  }) as CategoryDailyAvgParams;
  assert.equal('exclude_if' in params, false);
});

test('buildRuleParams: an empty category list is an error', () => {
  assert.throws(
    () =>
      buildRuleParams({
        ...emptyRuleForm(),
        kind: 'category_total',
        categoryIds: [],
      }),
    /category/i,
  );
});

test('buildRuleParams: a blank number is an error, not a silent 0', () => {
  assert.throws(
    () =>
      buildRuleParams({
        ...emptyRuleForm(),
        kind: 'sleep_duration',
        windowDays: '',
      }),
    /Window days/,
  );
});

// ── ruleFormValuesFromRule ───────────────────────────────────────────────

function ruleScore(overrides: Partial<RuleScore>): RuleScore {
  return {
    id: 'r-1',
    name: 'Fitness',
    icon: 'fitness',
    kind: 'category_days',
    phase: 'active',
    starts_on: '2026-10-01',
    ends_on: null,
    paused_on: null,
    sort_order: 0,
    condition: '≥12 days of ≥1h / 60d',
    on_track: true,
    window_full: true,
    actual_label: '14 days',
    target_label: '12 days / 60d',
    owed: 'On track',
    streak: 3,
    best_streak: 9,
    compliance: 0.8,
    params: {
      categories: { ids: ['cat-fitness'], include_children: true },
      window_days: 60,
      min_minutes: 60,
      min_days: 12,
    },
    strip: [],
    evidence: { type: 'category_days', days: [] },
    ...overrides,
  };
}

test('ruleFormValuesFromRule: hydrates the days fields from params', () => {
  const values = ruleFormValuesFromRule(ruleScore({}));
  assert.equal(values.kind, 'category_days');
  assert.deepEqual(values.categoryIds, ['cat-fitness']);
  assert.equal(values.includeChildren, true);
  assert.equal(values.windowDays, '60');
  assert.equal(values.minMinutes, '60');
  assert.equal(values.minDays, '12');
});

test('ruleFormValuesFromRule: hydrates a daily average with an exclude section', () => {
  const values = ruleFormValuesFromRule(
    ruleScore({
      kind: 'category_daily_avg',
      params: {
        categories: 'all_productive',
        window_days: 60,
        weekdays: [0, 1, 2],
        target_hours: 7,
        exclude_if: {
          categories: { ids: ['cat-trip'], include_children: false },
          percent_of_waking: 30,
        },
      },
    }),
  );
  assert.equal(values.useAllProductive, true);
  assert.deepEqual(values.weekdays, [0, 1, 2]);
  assert.equal(values.targetHours, '7');
  assert.equal(values.excludeEnabled, true);
  assert.deepEqual(values.excludeIds, ['cat-trip']);
  assert.equal(values.excludeIncludeChildren, false);
  assert.equal(values.excludePercent, '30');
});

test('ruleFormValuesFromRule: malformed params throw instead of crashing', () => {
  assert.throws(
    () => ruleFormValuesFromRule(ruleScore({ params: null })),
    /malformed/i,
  );
  assert.throws(
    () => ruleFormValuesFromRule(ruleScore({ params: { window_days: '60' } })),
    /malformed/i,
  );
});

test('parseRuleParams: rejects a wrong-shaped selector and accepts the canonical one', () => {
  assert.throws(
    () =>
      parseRuleParams('category_total', {
        categories: { ids: 'cat-work' },
        window_days: 7,
        comparator: 'gte',
        target_hours: 4,
      }),
    /selector/i,
  );
  assert.deepEqual(
    parseRuleParams('category_total', {
      categories: { ids: ['cat-work'], include_children: false },
      window_days: 7,
      comparator: 'gte',
      target_hours: 4,
    }),
    {
      categories: { ids: ['cat-work'], include_children: false },
      window_days: 7,
      comparator: 'gte',
      target_hours: 4,
    },
  );
});

// ── reorderIds ───────────────────────────────────────────────────────────

test('reorderIds: appends missing archive ids in sort_order', () => {
  const ids = reorderIds(
    ['b', 'a'],
    [
      { id: 'archived-late', sort_order: 9 },
      { id: 'archived-early', sort_order: 2 },
      { id: 'a', sort_order: 1 },
    ],
  );
  assert.deepEqual(ids, ['b', 'a', 'archived-early', 'archived-late']);
});

test('reorderIds: never lists an id twice', () => {
  const ids = reorderIds(['a', 'b'], [{ id: 'b', sort_order: 0 }]);
  assert.deepEqual(ids, ['a', 'b']);
});

test('swapNeighbors: swaps in place and refuses the ends', () => {
  assert.deepEqual(swapNeighbors(['a', 'b', 'c'], 0, 1), ['b', 'a', 'c']);
  assert.deepEqual(swapNeighbors(['a', 'b', 'c'], 2, -1), ['a', 'c', 'b']);
  assert.equal(swapNeighbors(['a', 'b'], 0, -1), null);
  assert.equal(swapNeighbors(['a', 'b'], 1, 1), null);
});

// ── formatting ───────────────────────────────────────────────────────────

test('formatBedtime/formatWake mirror the Rust clock helpers', () => {
  assert.equal(formatBedtime(660), '23:00');
  assert.equal(formatBedtime(0), '12:00');
  assert.equal(formatWake(300), '05:00');
  assert.equal(formatWake(0), '00:00');
});

test('targetHoursFromCondition reads the target hours, else null', () => {
  assert.equal(targetHoursFromCondition('≥4h / 7d'), 4);
  assert.equal(targetHoursFromCondition('≤6.5h / 60d'), 6.5);
  assert.equal(targetHoursFromCondition('≥12 days of ≥1h / 60d'), null);
});

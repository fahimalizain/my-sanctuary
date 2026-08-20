import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Category, TaskList } from '../../types';
import {
  buildCategoryTree,
  categoryMatchesSelection,
  categoryTriggerLabel,
  childIdsOf,
  expandCategorySelection,
  isImpliedByParent,
  toggleCategoryId,
} from './board-filters';

// ── Fixtures (inline — never import mock-data) ───────────────────────────

let seq = 0;

function list(overrides: Partial<TaskList> & { name: string }): TaskList {
  const id = overrides.id ?? `list-${++seq}`;
  const { name, ...rest } = overrides;
  return {
    id,
    user_id: 'u1',
    name,
    color: '#000000',
    sort_order: 0,
    created_at: '2026-08-20T00:00:00Z',
    updated_at: '2026-08-20T00:00:00Z',
    ...rest,
  };
}

function cat(overrides: Partial<Category> & { title: string }): Category {
  const id = overrides.id ?? `cat-${++seq}`;
  const { title, ...rest } = overrides;
  const parentId = rest.parent_id ?? null;
  const isChild = parentId !== null;
  const listId = isChild ? null : (rest.list_id ?? 'list-a');
  return {
    id,
    user_id: 'u1',
    list_id: listId,
    parent_id: parentId,
    title,
    slug: id,
    color: '#8b5cf6',
    is_productive: true,
    google_calendar_id: null,
    google_color_id: null,
    sort_order: 0,
    is_untracked: false,
    created_at: '2026-08-20T00:00:00Z',
    updated_at: '2026-08-20T00:00:00Z',
    patterns: [],
    inherited_list_id: isChild ? null : listId,
    ...rest,
  };
}

// One-level taxonomy used by every test:
//   list-a "Deep Work" (sort 0)   list-b "Life" (sort 1)   list-c "Admin" (sort 1)
//   ├─ Development (0)            └─ Life Admin (0)        └─ Errands (0)
//   │   ├─ Frontend (0)
//   │   └─ Backend (1)
//   └─ Design (1)
//       └─ UI (0)
// `untracked` is the system sink: no list, never a picker row.
const lists: TaskList[] = [
  list({ id: 'list-a', name: 'Deep Work', sort_order: 0 }),
  list({ id: 'list-b', name: 'Life', sort_order: 1 }),
  list({ id: 'list-c', name: 'Admin', sort_order: 1 }),
];

const categories: Category[] = [
  cat({
    id: 'root-dev',
    title: 'Development',
    list_id: 'list-a',
    sort_order: 0,
  }),
  cat({
    id: 'child-frontend',
    title: 'Frontend',
    parent_id: 'root-dev',
    sort_order: 0,
  }),
  cat({
    id: 'child-backend',
    title: 'Backend',
    parent_id: 'root-dev',
    sort_order: 1,
  }),
  cat({
    id: 'root-design',
    title: 'Design',
    list_id: 'list-a',
    sort_order: 1,
  }),
  cat({
    id: 'child-ui',
    title: 'UI',
    parent_id: 'root-design',
    sort_order: 0,
  }),
  cat({
    id: 'root-life',
    title: 'Life Admin',
    list_id: 'list-b',
    sort_order: 0,
  }),
  cat({
    id: 'root-errands',
    title: 'Errands',
    list_id: 'list-c',
    sort_order: 0,
  }),
  cat({
    id: 'untracked',
    title: 'Untracked',
    is_untracked: true,
    list_id: null,
    inherited_list_id: null,
  }),
];

// ── childIdsOf ───────────────────────────────────────────────────────────

test('childIdsOf: living children of a root, in input order', () => {
  assert.deepEqual(childIdsOf('root-dev', categories), [
    'child-frontend',
    'child-backend',
  ]);
});

test('childIdsOf: untracked and leaves have no children', () => {
  assert.deepEqual(childIdsOf('untracked', categories), []);
  assert.deepEqual(childIdsOf('child-frontend', categories), []);
});

// ── expandCategorySelection / categoryMatchesSelection ───────────────────

test('empty selection matches any id (no category filter)', () => {
  assert.equal(categoryMatchesSelection('root-dev', [], categories), true);
  assert.equal(
    categoryMatchesSelection('does-not-exist', [], categories),
    true,
  );
  assert.deepEqual(expandCategorySelection([], categories), new Set());
});

test('selecting a parent matches the parent and its living children, not sibling roots', () => {
  const ids = ['root-dev'];
  assert.equal(categoryMatchesSelection('root-dev', ids, categories), true);
  assert.equal(
    categoryMatchesSelection('child-frontend', ids, categories),
    true,
  );
  assert.equal(
    categoryMatchesSelection('child-backend', ids, categories),
    true,
  );
  assert.equal(categoryMatchesSelection('root-design', ids, categories), false);
  assert.equal(categoryMatchesSelection('child-ui', ids, categories), false);
  assert.equal(categoryMatchesSelection('root-life', ids, categories), false);
  assert.equal(categoryMatchesSelection('untracked', ids, categories), false);
});

test('selecting only a child matches that child, not its parent or siblings', () => {
  const ids = ['child-frontend'];
  assert.equal(
    categoryMatchesSelection('child-frontend', ids, categories),
    true,
  );
  assert.equal(categoryMatchesSelection('root-dev', ids, categories), false);
  assert.equal(
    categoryMatchesSelection('child-backend', ids, categories),
    false,
  );
  assert.equal(categoryMatchesSelection('root-design', ids, categories), false);
});

test('parent plus an implied child matches the same set as the parent alone (deduped)', () => {
  const parentOnly = expandCategorySelection(['root-dev'], categories);
  const parentPlusChild = expandCategorySelection(
    ['root-dev', 'child-frontend'],
    categories,
  );
  assert.equal(parentPlusChild.size, 3);
  assert.deepEqual([...parentPlusChild].sort(), [...parentOnly].sort());
});

test('selecting all children does NOT collapse to the parent', () => {
  const ids = ['child-frontend', 'child-backend'];
  assert.equal(
    categoryMatchesSelection('child-frontend', ids, categories),
    true,
  );
  assert.equal(
    categoryMatchesSelection('child-backend', ids, categories),
    true,
  );
  assert.equal(categoryMatchesSelection('root-dev', ids, categories), false);
});

test('untracked has no children and matches only itself', () => {
  assert.deepEqual(
    expandCategorySelection(['untracked'], categories),
    new Set(['untracked']),
  );
  assert.equal(
    categoryMatchesSelection('untracked', ['untracked'], categories),
    true,
  );
});

test('unknown selected ids contribute themselves only and never throw', () => {
  assert.deepEqual(
    expandCategorySelection(['ghost-1', 'ghost-2'], categories),
    new Set(['ghost-1', 'ghost-2']),
  );
  assert.equal(
    categoryMatchesSelection('ghost-1', ['ghost-1'], categories),
    true,
  );
  assert.equal(
    categoryMatchesSelection('root-dev', ['ghost-1'], categories),
    false,
  );
});

// ── isImpliedByParent ────────────────────────────────────────────────────

test('isImpliedByParent: true for a living child when its parent is explicitly selected', () => {
  assert.equal(
    isImpliedByParent('child-frontend', ['root-dev'], categories),
    true,
  );
  assert.equal(
    isImpliedByParent('child-backend', ['root-dev'], categories),
    true,
  );
});

test('isImpliedByParent: false when only the child itself is selected', () => {
  assert.equal(
    isImpliedByParent('child-frontend', ['child-frontend'], categories),
    false,
  );
  assert.equal(isImpliedByParent('child-frontend', [], categories), false);
});

test('isImpliedByParent: false for roots, untracked and unknown ids', () => {
  assert.equal(isImpliedByParent('root-dev', ['root-dev'], categories), false);
  assert.equal(isImpliedByParent('untracked', ['root-dev'], categories), false);
  assert.equal(isImpliedByParent('ghost', ['root-dev'], categories), false);
});

// ── toggleCategoryId ─────────────────────────────────────────────────────

test('toggleCategoryId: toggling a parent on adds only the parent id', () => {
  assert.deepEqual(toggleCategoryId([], 'root-dev', categories), ['root-dev']);
  assert.deepEqual(toggleCategoryId(['root-life'], 'root-dev', categories), [
    'root-life',
    'root-dev',
  ]);
});

test('toggleCategoryId: toggling a parent off removes it', () => {
  assert.deepEqual(
    toggleCategoryId(['root-dev', 'root-life'], 'root-dev', categories),
    ['root-life'],
  );
  assert.deepEqual(toggleCategoryId(['root-dev'], 'root-dev', categories), []);
});

test('toggleCategoryId: toggling an implied child is a no-op', () => {
  // Never adds the implied id to the URL…
  assert.deepEqual(
    toggleCategoryId(['root-dev'], 'child-frontend', categories),
    ['root-dev'],
  );
  // …and never rewrites an implied id out of the URL.
  assert.deepEqual(
    toggleCategoryId(
      ['root-dev', 'child-frontend'],
      'child-frontend',
      categories,
    ),
    ['root-dev', 'child-frontend'],
  );
});

test('toggleCategoryId: toggling a free child adds or removes just that id', () => {
  assert.deepEqual(toggleCategoryId(['root-dev'], 'child-ui', categories), [
    'root-dev',
    'child-ui',
  ]);
  assert.deepEqual(
    toggleCategoryId(['root-dev', 'child-ui'], 'child-ui', categories),
    ['root-dev'],
  );
});

// ── categoryTriggerLabel ─────────────────────────────────────────────────

test('categoryTriggerLabel: All / title / count, id fallback for missing rows', () => {
  assert.equal(categoryTriggerLabel([], categories), 'All');
  assert.equal(categoryTriggerLabel(['root-dev'], categories), 'Development');
  assert.equal(
    categoryTriggerLabel(['root-dev', 'child-ui'], categories),
    '2 categories',
  );
  assert.equal(categoryTriggerLabel(['ghost'], categories), 'ghost');
});

// ── buildCategoryTree ────────────────────────────────────────────────────

test('buildCategoryTree: empty query shows the full living tree; untracked excluded; lists sorted by sort_order then name', () => {
  const groups = buildCategoryTree(lists, categories, '');
  assert.deepEqual(
    groups.map((g) => g.list.name),
    ['Deep Work', 'Admin', 'Life'],
  );
  // Grouped under the right list.
  assert.deepEqual(
    groups[1].roots.map((r) => r.category.title),
    ['Errands'],
  );
  assert.deepEqual(
    groups[2].roots.map((r) => r.category.title),
    ['Life Admin'],
  );
  // Roots and children honor sort_order.
  assert.deepEqual(
    groups[0].roots.map((r) => r.category.title),
    ['Development', 'Design'],
  );
  assert.deepEqual(
    groups[0].roots[0].children.map((c) => c.title),
    ['Frontend', 'Backend'],
  );
  const allRoots = groups.flatMap((g) => g.roots);
  assert.ok(allRoots.every((r) => !r.category.is_untracked));
  assert.ok(allRoots.every((r) => r.children.every((c) => !c.is_untracked)));
});

test('buildCategoryTree: query on a child title keeps child + root + list, hides the rest', () => {
  const groups = buildCategoryTree(lists, categories, 'frontend');
  assert.equal(groups.length, 1);
  assert.equal(groups[0].list.name, 'Deep Work');
  assert.deepEqual(
    groups[0].roots.map((r) => r.category.title),
    ['Development'],
  );
  assert.deepEqual(
    groups[0].roots[0].children.map((c) => c.title),
    ['Frontend'],
  );
});

test('buildCategoryTree: query on a parent title keeps the parent AND its children', () => {
  const groups = buildCategoryTree(lists, categories, 'development');
  assert.equal(groups.length, 1);
  assert.deepEqual(
    groups[0].roots.map((r) => r.category.title),
    ['Development'],
  );
  assert.deepEqual(
    groups[0].roots[0].children.map((c) => c.title),
    ['Frontend', 'Backend'],
  );
});

test("buildCategoryTree: query on a list name keeps that list's whole tree", () => {
  const groups = buildCategoryTree(lists, categories, 'deep');
  assert.equal(groups.length, 1);
  assert.equal(groups[0].list.name, 'Deep Work');
  assert.equal(groups[0].roots.length, 2);
  assert.deepEqual(
    groups[0].roots[0].children.map((c) => c.title),
    ['Frontend', 'Backend'],
  );
});

test('buildCategoryTree: query is trimmed and case-insensitive', () => {
  const groups = buildCategoryTree(lists, categories, '  UI  ');
  assert.equal(groups.length, 1);
  assert.deepEqual(
    groups[0].roots.map((r) => r.category.title),
    ['Design'],
  );
  assert.deepEqual(
    groups[0].roots[0].children.map((c) => c.title),
    ['UI'],
  );
});

test('buildCategoryTree: no matches → []', () => {
  assert.deepEqual(buildCategoryTree(lists, categories, 'zzzz'), []);
});

import type { Category, TaskList } from '../../types';

// Pure category-filter helpers for the board (ADR 0002 § Filters, amended
// 2026-08-20). No React, no DOM, no fetch — unit-tested with node:test.
//
// Taxonomy contract:
// - One-level tree: roots have `parent_id: null`; children hang directly off
//   a root via `parent_id`.
// - `is_untracked` is the sink: never a picker row, and it has no children.
// - Selection is by explicit category id; the URL stores explicit ids only.

/** Living children of `parentId` (one-level; skips untracked). */
export function childIdsOf(
  parentId: string,
  categories: readonly Category[],
): string[] {
  return categories
    .filter((cat) => cat.parent_id === parentId && !cat.is_untracked)
    .map((cat) => cat.id);
}

/** Explicit ids plus living children of any selected parent. Deduped. */
export function expandCategorySelection(
  selectedIds: readonly string[],
  categories: readonly Category[],
): Set<string> {
  const expanded = new Set<string>();
  for (const id of selectedIds) {
    expanded.add(id);
    // Unknown ids contribute themselves only; known parents add their
    // living children. Untracked has no children, so it expands to itself.
    for (const childId of childIdsOf(id, categories)) {
      expanded.add(childId);
    }
  }
  return expanded;
}

/**
 * Empty `selectedIds` → true (no category filter).
 * Otherwise true iff `taskCategoryId` is in the expanded set.
 */
export function categoryMatchesSelection(
  taskCategoryId: string,
  selectedIds: readonly string[],
  categories: readonly Category[],
): boolean {
  if (selectedIds.length === 0) return true;
  return expandCategorySelection(selectedIds, categories).has(taskCategoryId);
}

/** True when this category is a living child of an explicitly selected parent. */
export function isImpliedByParent(
  categoryId: string,
  selectedIds: readonly string[],
  categories: readonly Category[],
): boolean {
  const cat = categories.find((entry) => entry.id === categoryId);
  if (!cat || cat.parent_id === null || cat.is_untracked) return false;
  return selectedIds.includes(cat.parent_id);
}

/**
 * Toggle an explicit id. If the id is implied by a selected parent, return
 * the same ids unchanged (click is a no-op). Otherwise add or remove `id`.
 * Never inserts children when adding a parent.
 */
export function toggleCategoryId(
  selectedIds: readonly string[],
  id: string,
  categories: readonly Category[],
): string[] {
  if (isImpliedByParent(id, selectedIds, categories)) {
    return [...selectedIds];
  }
  if (selectedIds.includes(id)) {
    return selectedIds.filter((selected) => selected !== id);
  }
  return [...selectedIds, id];
}

/**
 * 0 explicit ids → "All"
 * 1 → that category's title (fallback to the raw id if the row is missing)
 * N → `${n} categories`
 * Count is explicit ids only.
 */
export function categoryTriggerLabel(
  selectedIds: readonly string[],
  categories: readonly Category[],
): string {
  if (selectedIds.length === 0) return 'All';
  if (selectedIds.length === 1) {
    const cat = categories.find((entry) => entry.id === selectedIds[0]);
    return cat ? cat.title : selectedIds[0];
  }
  return `${selectedIds.length} categories`;
}

export type CategoryTreeRoot = {
  category: Category;
  children: Category[];
};

export type CategoryTreeGroup = {
  list: TaskList;
  roots: CategoryTreeRoot[];
};

/**
 * Picker tree. Drops `is_untracked`.
 * Groups by `inherited_list_id === list.id`.
 * Sort lists by `sort_order` then `name`; roots by `sort_order` then `title`;
 * children by `sort_order` then `title`.
 * Hide lists that have no visible roots after filtering.
 *
 * `query` (trimmed, case-insensitive):
 *   - empty → full tree
 *   - otherwise a node is kept if it matches OR is an ancestor of a match
 *   - a category matches if its title, its parent's title, or its list name
 *     contains the query
 */
export function buildCategoryTree(
  lists: readonly TaskList[],
  categories: readonly Category[],
  query: string,
): CategoryTreeGroup[] {
  const q = query.trim().toLowerCase();
  const byId = new Map(categories.map((cat) => [cat.id, cat]));

  const byListId = new Map<string, Category[]>();
  for (const cat of categories) {
    // Roots only: children hang off their root, and untracked is never
    // offered as a picker row.
    if (cat.parent_id !== null || cat.is_untracked) continue;
    if (cat.inherited_list_id === null) continue;
    const bucket = byListId.get(cat.inherited_list_id);
    if (bucket) {
      bucket.push(cat);
    } else {
      byListId.set(cat.inherited_list_id, [cat]);
    }
  }

  const childrenOf = (rootId: string): Category[] =>
    categories.filter((cat) => cat.parent_id === rootId && !cat.is_untracked);

  const matchesQuery = (cat: Category, list: TaskList): boolean => {
    if (q.length === 0) return true;
    if (cat.title.toLowerCase().includes(q)) return true;
    const parent = cat.parent_id !== null ? byId.get(cat.parent_id) : undefined;
    if (parent && parent.title.toLowerCase().includes(q)) return true;
    return list.name.toLowerCase().includes(q);
  };

  const groups: CategoryTreeGroup[] = [];
  for (const list of [...lists].sort(
    (a, b) => a.sort_order - b.sort_order || a.name.localeCompare(b.name),
  )) {
    const roots = (byListId.get(list.id) ?? []).sort(
      (a, b) => a.sort_order - b.sort_order || a.title.localeCompare(b.title),
    );
    const tree: CategoryTreeRoot[] = [];
    for (const root of roots) {
      const children = childrenOf(root.id)
        .sort(
          (a, b) =>
            a.sort_order - b.sort_order || a.title.localeCompare(b.title),
        )
        .filter((child) => matchesQuery(child, list));
      if (matchesQuery(root, list) || children.length > 0) {
        tree.push({ category: root, children });
      }
    }
    if (tree.length > 0) {
      groups.push({ list, roots: tree });
    }
  }
  return groups;
}

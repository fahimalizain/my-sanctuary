import { test } from 'node:test';
import assert from 'node:assert/strict';
import { NAV_LABEL_MIN_WIDTH_PX, navPillDestination } from './nav-pill';

// ── NAV_LABEL_MIN_WIDTH_PX ──────────────────────────────────────────────

test('NAV_LABEL_MIN_WIDTH_PX matches the Tailwind md breakpoint', () => {
  assert.equal(NAV_LABEL_MIN_WIDTH_PX, 768);
});

// ── desktop (showLabel: true) ────────────────────────────────────────────

test('desktop: width includes the active label width', () => {
  assert.deepEqual(
    navPillDestination({
      activeIndex: 2,
      inactiveWidth: 40,
      gap: 4,
      labelWidth: 60,
      showLabel: true,
    }),
    { left: 88, width: 100 },
  );
});

test('desktop: first item sits at left 0', () => {
  assert.deepEqual(
    navPillDestination({
      activeIndex: 0,
      inactiveWidth: 40,
      gap: 4,
      labelWidth: 60,
      showLabel: true,
    }),
    { left: 0, width: 100 },
  );
});

test('desktop: left is index * (inactiveWidth + gap) for a mid index', () => {
  assert.equal(
    navPillDestination({
      activeIndex: 3,
      inactiveWidth: 40,
      gap: 4,
      labelWidth: 60,
      showLabel: true,
    }).left,
    132,
  );
});

// ── mobile (showLabel: false) ────────────────────────────────────────────

test('mobile: width is inactiveWidth only; labelWidth is ignored', () => {
  assert.deepEqual(
    navPillDestination({
      activeIndex: 3,
      inactiveWidth: 40,
      gap: 4,
      labelWidth: 999,
      showLabel: false,
    }),
    { left: 132, width: 40 },
  );
});

test('mobile: first item sits at left 0', () => {
  assert.deepEqual(
    navPillDestination({
      activeIndex: 0,
      inactiveWidth: 40,
      gap: 4,
      labelWidth: 60,
      showLabel: false,
    }),
    { left: 0, width: 40 },
  );
});

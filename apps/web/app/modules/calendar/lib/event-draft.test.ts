// Unit tests for draft persist eligibility.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isPersistableDraftTitle } from './event-draft';

test('isPersistableDraftTitle: empty → false', () => {
  assert.equal(isPersistableDraftTitle(''), false);
});

test('isPersistableDraftTitle: whitespace → false', () => {
  assert.equal(isPersistableDraftTitle('   '), false);
  assert.equal(isPersistableDraftTitle('\t\n'), false);
});

test('isPersistableDraftTitle: non-empty → true', () => {
  assert.equal(isPersistableDraftTitle('Standup'), true);
});

test('isPersistableDraftTitle: trimmed non-empty → true', () => {
  assert.equal(isPersistableDraftTitle('  Hi  '), true);
});

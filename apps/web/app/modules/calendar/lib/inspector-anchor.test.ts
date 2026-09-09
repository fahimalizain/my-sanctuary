// Unit tests for event-chip anchor selectors used by EventInspector.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { escapeAttrValue, eventChipSelector } from './inspector-anchor';

test('eventChipSelector: normal id', () => {
  assert.equal(eventChipSelector('abc-123'), '[data-event-id="abc-123"]');
});

test('eventChipSelector: temp uuid id', () => {
  const id = 'tmp_550e8400-e29b-41d4-a716-446655440000';
  assert.equal(eventChipSelector(id), `[data-event-id="${id}"]`);
});

test('eventChipSelector: id containing double quote', () => {
  assert.equal(eventChipSelector('foo"bar'), '[data-event-id="foo\\"bar"]');
});

test('eventChipSelector: id containing backslash', () => {
  assert.equal(eventChipSelector('foo\\bar'), '[data-event-id="foo\\\\bar"]');
});

test('eventChipSelector: id containing ] is fine inside quotes', () => {
  assert.equal(eventChipSelector('foo]bar'), '[data-event-id="foo]bar"]');
});

test('escapeAttrValue: empty string', () => {
  assert.equal(escapeAttrValue(''), '');
});

// Unit tests for temp → server id aliases: a create that flips its id must
// still answer for the id a drag session or a queued callback is holding.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventIdAliases } from './event-id-aliases';

test('resolve: server ids pass through untouched', () => {
  const aliases = new EventIdAliases();
  assert.equal(aliases.resolve('evt-1'), 'evt-1');
  assert.equal(aliases.resolve(''), '');
});

test('resolve: an unknown temp id is still itself', () => {
  const aliases = new EventIdAliases();
  assert.equal(aliases.resolve('tmp_pending'), 'tmp_pending');
});

test('record then resolve: a temp id follows its create to the server row', () => {
  const aliases = new EventIdAliases();
  aliases.record('tmp_a', 'srv-1');
  assert.equal(aliases.resolve('tmp_a'), 'srv-1');
  // Other ids are not dragged along.
  assert.equal(aliases.resolve('tmp_b'), 'tmp_b');
  assert.equal(aliases.resolve('srv-1'), 'srv-1');
});

test('record: ignores a non-temp source or a temp destination', () => {
  const aliases = new EventIdAliases();
  aliases.record('srv-1', 'srv-2');
  assert.equal(aliases.resolve('srv-1'), 'srv-1');
  aliases.record('tmp_a', 'tmp_b');
  assert.equal(aliases.resolve('tmp_a'), 'tmp_a');
  // A self-record is not an alias either.
  aliases.record('tmp_same', 'tmp_same');
  assert.equal(aliases.resolve('tmp_same'), 'tmp_same');
});

test('last record wins: a retried create cannot point at the dead row', () => {
  const aliases = new EventIdAliases();
  aliases.record('tmp_a', 'srv-1');
  aliases.record('tmp_a', 'srv-2');
  assert.equal(aliases.resolve('tmp_a'), 'srv-2');
});

test('reset: drops every alias (account switch)', () => {
  const aliases = new EventIdAliases();
  aliases.record('tmp_a', 'srv-1');
  aliases.reset();
  assert.equal(aliases.resolve('tmp_a'), 'tmp_a');
});

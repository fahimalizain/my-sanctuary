import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildClassifyUrl } from './classify-url';

test('buildClassifyUrl: no lock appends only the title', () => {
  assert.equal(
    buildClassifyUrl('Review Q3', null),
    '/api/tasks/classify?title=Review%20Q3',
  );
});

test('buildClassifyUrl: a lock appends category_id', () => {
  assert.equal(
    buildClassifyUrl('Review Q3', 'cat-1'),
    '/api/tasks/classify?title=Review%20Q3&category_id=cat-1',
  );
});

test('buildClassifyUrl: an empty string lock behaves like no lock', () => {
  assert.equal(
    buildClassifyUrl('Review Q3', ''),
    '/api/tasks/classify?title=Review%20Q3',
  );
});

test('buildClassifyUrl: empty title with a lock keeps the empty title param', () => {
  assert.equal(
    buildClassifyUrl('', 'cat-1'),
    '/api/tasks/classify?title=&category_id=cat-1',
  );
});

test('buildClassifyUrl: special characters are percent-encoded', () => {
  assert.equal(
    buildClassifyUrl('hello & world?', null),
    '/api/tasks/classify?title=hello%20%26%20world%3F',
  );
});

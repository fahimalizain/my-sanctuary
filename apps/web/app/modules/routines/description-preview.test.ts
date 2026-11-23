import { test } from 'node:test';
import assert from 'node:assert/strict';
import { markdownPreview } from './description-preview';

test('plain text is left alone', () => {
  assert.equal(
    markdownPreview('Bring water and a mat'),
    'Bring water and a mat',
  );
});

test('markdown markers are stripped and whitespace collapses', () => {
  assert.equal(
    markdownPreview('# Warm up\n\n**Run** two laps\n\n- stretch\n- hydrate'),
    'Warm up Run two laps stretch hydrate',
  );
});

test('links keep their label, code keeps its text', () => {
  assert.equal(
    markdownPreview('See [the plan](https://example.com) and `npm test`'),
    'See the plan and npm test',
  );
});

test('long previews are clamped with an ellipsis', () => {
  const preview = markdownPreview('word '.repeat(100), 40);
  assert.equal(preview.length, 40);
  assert.ok(preview.endsWith('…'));
});

test('empty markdown stays empty', () => {
  assert.equal(markdownPreview('   '), '');
});

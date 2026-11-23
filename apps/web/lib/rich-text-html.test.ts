import { test } from 'node:test';
import assert from 'node:assert/strict';
import { isEmptyEditorHtml, normalizeEditorHtml } from './rich-text-html';

test('empty and whitespace-only HTML maps to empty', () => {
  assert.equal(normalizeEditorHtml(''), '');
  assert.equal(normalizeEditorHtml('   '), '');
  assert.equal(normalizeEditorHtml('<p></p>'), '');
  assert.equal(normalizeEditorHtml('<p><br></p>'), '');
  assert.equal(normalizeEditorHtml('<p> &nbsp; </p>'), '');
});

test('visible HTML is kept and only trailing whitespace is trimmed', () => {
  assert.equal(normalizeEditorHtml('<p>Hello</p>\n'), '<p>Hello</p>');
  assert.equal(
    normalizeEditorHtml('<p><strong>Hello</strong></p>'),
    '<p><strong>Hello</strong></p>',
  );
});

test('a horizontal rule keeps an otherwise empty document', () => {
  assert.equal(isEmptyEditorHtml('<hr>'), false);
  assert.equal(isEmptyEditorHtml('<p><br></p><hr>'), false);
});

test('comments do not count as content', () => {
  assert.equal(isEmptyEditorHtml('<!-- note -->'), true);
  assert.equal(isEmptyEditorHtml('<p><!-- note --></p>'), true);
});

import { before, test } from 'node:test';
import assert from 'node:assert/strict';
import { installDom } from '../components/install-dom';

installDom();

type Helpers = typeof import('./event-description');
let helpers: Helpers;

before(async () => {
  helpers = await import('./event-description');
});

test('plain text becomes escaped HTML with preserved line breaks', () => {
  assert.equal(
    helpers.plainTextToEditorHtml('5 < 10\nnext & last'),
    '<p>5 &lt; 10<br>next &amp; last</p>',
  );
});

test('HTML detection does not mistake ordinary prose for markup', () => {
  assert.equal(helpers.isHtmlDescription('Bring snacks'), false);
  assert.equal(helpers.isHtmlDescription('5 < 10'), false);
  assert.equal(helpers.isHtmlDescription('<b>Bring snacks</b>'), true);
});

test('sanitizer keeps the event-description allowlist and drops scripts/handlers', () => {
  const clean = helpers.sanitizeEventDescriptionHtml(
    '<p>Hello <strong>world</strong><script>alert(1)</script>' +
      '<img src=x onerror=alert(1)><a href="javascript:alert(1)">bad</a></p>',
  );
  assert.match(clean, /<strong>world<\/strong>/);
  assert.doesNotMatch(clean, /script/i);
  assert.doesNotMatch(clean, /onerror/i);
  assert.doesNotMatch(clean, /javascript:/i);
});

test('sanitized links open safely in a new tab', () => {
  const clean = helpers.sanitizeEventDescriptionHtml(
    '<a href="https://example.com">Example</a>',
  );
  assert.match(clean, /href="https:\/\/example\.com"/);
  assert.match(clean, /target="_blank"/);
  assert.match(clean, /rel="noopener noreferrer nofollow"/);
});

test('stored plain text is escaped before it reaches the editor', () => {
  assert.equal(
    helpers.eventDescriptionToEditorHtml('Tom & Jerry\n5 < 10'),
    '<p>Tom &amp; Jerry<br>5 &lt; 10</p>',
  );
});

test('stored HTML is sanitized before it reaches the editor', () => {
  assert.equal(
    helpers.eventDescriptionToEditorHtml(
      '<p>Keep <b>this</b></p><script>alert(1)</script>',
    ),
    '<p>Keep <b>this</b></p>',
  );
});

test('blank descriptions map to empty', () => {
  assert.equal(helpers.eventDescriptionToEditorHtml('   '), '');
  assert.equal(helpers.eventDescriptionToEditorHtml('<p>&nbsp;</p>'), '');
});

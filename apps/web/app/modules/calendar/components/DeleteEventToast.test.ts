// Mount tests for the deferred-delete toast: message wording for titled and
// blank events, the Undo affordance, and that it never takes focus. DOM must
// exist before the component module is imported — installDom() then dynamic
// import (see EventInspector.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import { installDom } from './install-dom';

installDom();

let DeleteEventToast: typeof import('./DeleteEventToast').DeleteEventToast;
let cleanup: () => void;
let fireEvent: typeof import('@testing-library/react').fireEvent;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  fireEvent = rtl.fireEvent;
  render = rtl.render;
  screen = rtl.screen;
  const mod = await import('./DeleteEventToast');
  DeleteEventToast = mod.DeleteEventToast;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

test('titled event: the message quotes the trimmed title', () => {
  render(
    createElement(DeleteEventToast, {
      title: '  Deep work  ',
      onUndo: () => {},
    }),
  );

  const toast = screen.getByRole('status');
  assert.equal(toast.textContent, 'Deleted “Deep work”Undo');
  assert.equal(toast.getAttribute('aria-live'), 'polite');
});

test('blank title: the message says the event was untitled', () => {
  render(createElement(DeleteEventToast, { title: '   ', onUndo: () => {} }));

  const toast = screen.getByRole('status');
  assert.ok(toast.textContent?.startsWith('Deleted untitled event'));
});

test('Undo is a button labelled Undo and calls onUndo exactly once', () => {
  let undos = 0;
  render(
    createElement(DeleteEventToast, {
      title: 'Deep work',
      onUndo: () => {
        undos += 1;
      },
    }),
  );

  const button = screen.getByRole('button', { name: 'Undo' });
  assert.equal(button.textContent, 'Undo');
  assert.equal(button.getAttribute('type'), 'button');

  fireEvent.click(button);
  assert.equal(undos, 1);
});

test('rendering the toast does not steal focus', () => {
  render(
    createElement(DeleteEventToast, { title: 'Deep work', onUndo: () => {} }),
  );

  assert.equal(document.activeElement, document.body);
});

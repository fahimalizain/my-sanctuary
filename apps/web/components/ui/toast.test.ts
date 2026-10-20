// Mount tests for the shared action toast: live-region politeness, message and
// action rendering, action ordering and click isolation, variant pass-through,
// and that mounting never takes focus. DOM must exist before the component
// module is imported — installDom() then dynamic import (see
// DeleteEventToast.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import { installDom } from '@/app/modules/calendar/components/install-dom';

installDom();

let Toast: typeof import('@/components/ui/toast').Toast;
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
  const mod = await import('@/components/ui/toast');
  Toast = mod.Toast;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

test('message: rendered inside a polite status region', () => {
  render(createElement(Toast, { children: 'Saved' }));

  const toast = screen.getByRole('status');
  assert.equal(toast.textContent, 'Saved');
  assert.equal(toast.getAttribute('aria-live'), 'polite');
});

test('no actions: renders no button', () => {
  render(createElement(Toast, { children: 'Saved' }));

  assert.equal(screen.queryAllByRole('button').length, 0);
});

test('empty actions: renders no button', () => {
  render(createElement(Toast, { actions: [], children: 'Saved' }));

  assert.equal(screen.queryAllByRole('button').length, 0);
});

test('actions: render in order and each click calls only its own action', () => {
  const calls: string[] = [];
  render(
    createElement(Toast, {
      actions: [
        { label: 'Undo', onClick: () => calls.push('undo') },
        {
          label: 'Dismiss',
          onClick: () => calls.push('dismiss'),
          variant: 'outline',
        },
      ],
      children: 'Deleted “Deep work”',
    }),
  );

  const buttons = screen.getAllByRole('button');
  assert.deepEqual(
    buttons.map((button) => button.textContent),
    ['Undo', 'Dismiss'],
  );
  for (const button of buttons) {
    assert.equal(button.getAttribute('type'), 'button');
  }

  fireEvent.click(buttons[0]);
  assert.deepEqual(calls, ['undo']);

  fireEvent.click(buttons[1]);
  assert.deepEqual(calls, ['undo', 'dismiss']);
});

test('variant: outline is accepted and the default stays primary', () => {
  render(
    createElement(Toast, {
      actions: [
        { label: 'Later', onClick: () => {}, variant: 'outline' },
        { label: 'Keep', onClick: () => {} },
      ],
      children: 'Event moved',
    }),
  );

  const outline = screen.getByRole('button', { name: 'Later' });
  assert.ok(outline.className.includes('border-input'));
  assert.ok(!outline.className.includes('bg-primary'));

  const primary = screen.getByRole('button', { name: 'Keep' });
  assert.ok(primary.className.includes('bg-primary'));
});

test('politeness: assertive sets an assertive live region', () => {
  render(
    createElement(Toast, {
      children: 'Could not save',
      politeness: 'assertive',
    }),
  );

  assert.equal(
    screen.getByRole('status').getAttribute('aria-live'),
    'assertive',
  );
});

test('rendering the toast does not steal focus', () => {
  render(
    createElement(Toast, {
      actions: [{ label: 'Undo', onClick: () => {} }],
      children: 'Deleted “Deep work”',
    }),
  );

  assert.equal(document.activeElement, document.body);
});

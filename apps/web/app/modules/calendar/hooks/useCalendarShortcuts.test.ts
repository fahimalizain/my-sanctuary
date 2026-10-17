// Feature tests for the page-level calendar shortcut listener: one keydown
// resolves to one action and routes it to the strip callbacks. DOM must exist
// before the hook module is imported.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ReactElement } from 'react';
import { installDom } from '../components/install-dom';

installDom();

let useCalendarShortcuts: typeof import('./useCalendarShortcuts').useCalendarShortcuts;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  const mod = await import('./useCalendarShortcuts');
  useCalendarShortcuts = mod.useCalendarShortcuts;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

interface ShortcutHarnessProps {
  suspended: boolean;
  /** Callback log, in dispatch order. */
  calls: string[];
}

/** Records callbacks instead of moving a real strip. */
function ShortcutHarness({
  suspended,
  calls,
}: ShortcutHarnessProps): ReactElement {
  useCalendarShortcuts({
    suspended,
    onPeriodChange: (n) => calls.push(`period:${n}`),
    onToday: () => calls.push('today'),
    onShiftPeriod: (delta) => calls.push(`shift:${delta}`),
  });
  return createElement('div', null, 'harness');
}

interface Harness {
  calls: string[];
  setSuspended: (next: boolean) => void;
  unmount: () => void;
}

function mountHarness(suspended = false): Harness {
  const calls: string[] = [];
  const view = render(createElement(ShortcutHarness, { suspended, calls }));
  return {
    calls,
    setSuspended: (next) => {
      view.rerender(createElement(ShortcutHarness, { suspended: next, calls }));
    },
    unmount: () => {
      view.unmount();
    },
  };
}

/** Dispatch a cancelable keydown; window is the default listener target. */
function press(
  key: string,
  init: KeyboardEventInit = {},
  target: EventTarget = window,
): KeyboardEvent {
  const event = new window.KeyboardEvent('keydown', {
    key,
    bubbles: true,
    cancelable: true,
    ...init,
  });
  target.dispatchEvent(event);
  return event;
}

test('useCalendarShortcuts: t goes to today and consumes the key', () => {
  const { calls } = mountHarness();

  const event = press('t');

  assert.deepEqual(calls, ['today']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: [ and ] step one period back and forward', () => {
  const { calls } = mountHarness();

  const next = press(']');
  const prev = press('[');

  assert.deepEqual(calls, ['shift:1', 'shift:-1']);
  assert.equal(next.defaultPrevented, true);
  assert.equal(prev.defaultPrevented, true);
});

test('useCalendarShortcuts: 4 selects a four-day period', () => {
  const { calls } = mountHarness();

  const event = press('4');

  assert.deepEqual(calls, ['period:4']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: meta+t is left to the browser', () => {
  const { calls } = mountHarness();

  const event = press('t', { metaKey: true });

  assert.deepEqual(calls, [], 'Cmd+T must stay a new tab');
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: a suspended page ignores shortcuts', () => {
  const { calls } = mountHarness(true);

  const event = press('t');

  assert.deepEqual(calls, []);
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: shortcuts resume when the dialog closes', () => {
  const { calls, setSuspended } = mountHarness(true);

  press('t');
  assert.deepEqual(calls, [], 'the delete dialog owns the keyboard');

  setSuspended(false);
  press('t');
  assert.deepEqual(calls, ['today']);
});

test('useCalendarShortcuts: keys typed in an input do not navigate', () => {
  const { calls } = mountHarness();
  const input = document.createElement('input');
  document.body.appendChild(input);

  try {
    // Dispatch on the input so the window listener sees it as e.target.
    const event = press('t', {}, input);
    assert.equal(event.target, input);
    assert.deepEqual(calls, []);
    assert.equal(event.defaultPrevented, false);
  } finally {
    input.remove();
  }
});

test('useCalendarShortcuts: unmount removes the listener', () => {
  const { calls, unmount } = mountHarness();

  unmount();
  press('t');

  assert.deepEqual(calls, []);
});

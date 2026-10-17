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

type ClipboardAction = 'copy' | 'cut' | 'paste' | 'duplicate';

interface ShortcutHarnessProps {
  suspended: boolean;
  /** Callback log, in dispatch order. */
  calls: string[];
  /** False models no selection / a read-only calendar. */
  deleteAllowed?: boolean;
  /**
   * Which clipboard callbacks claim the key. Default: none do (nothing is
   * selected / nothing was copied), so the browser chord stays untouched and
   * nothing is logged — the log only records handled keys.
   */
  clipboardHandled?: Partial<Record<ClipboardAction, boolean>>;
}

/** Records callbacks instead of moving a real strip. */
function ShortcutHarness({
  suspended,
  calls,
  deleteAllowed = true,
  clipboardHandled,
}: ShortcutHarnessProps): ReactElement {
  const clipboard = (action: ClipboardAction) => (): boolean => {
    if (!clipboardHandled?.[action]) return false;
    calls.push(action);
    return true;
  };

  useCalendarShortcuts({
    suspended,
    onPeriodChange: (n) => calls.push(`period:${n}`),
    onToday: () => calls.push('today'),
    onShiftPeriod: (delta) => calls.push(`shift:${delta}`),
    onCreate: () => calls.push('create'),
    onRequestDelete: deleteAllowed ? () => calls.push('delete') : null,
    onCopy: clipboard('copy'),
    onCut: clipboard('cut'),
    onPaste: clipboard('paste'),
    onDuplicate: clipboard('duplicate'),
  });
  return createElement('div', null, 'harness');
}

interface Harness {
  calls: string[];
  setSuspended: (next: boolean) => void;
  unmount: () => void;
}

function mountHarness(
  suspended = false,
  deleteAllowed = true,
  clipboardHandled?: Partial<Record<ClipboardAction, boolean>>,
): Harness {
  const calls: string[] = [];
  const view = render(
    createElement(ShortcutHarness, {
      suspended,
      calls,
      deleteAllowed,
      clipboardHandled,
    }),
  );
  return {
    calls,
    setSuspended: (next) => {
      view.rerender(
        createElement(ShortcutHarness, {
          suspended: next,
          calls,
          deleteAllowed,
          clipboardHandled,
        }),
      );
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

test('useCalendarShortcuts: c creates a draft and consumes the key', () => {
  const { calls } = mountHarness();

  const event = press('c');

  assert.deepEqual(calls, ['create']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: Backspace requests delete and consumes the key', () => {
  const { calls } = mountHarness();

  const event = press('Backspace');

  assert.deepEqual(calls, ['delete']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: Backspace is left alone when delete is not allowed', () => {
  const { calls } = mountHarness(false, false);

  const event = press('Backspace');

  assert.deepEqual(calls, [], 'no selection / read-only calendar');
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: holding c does not create a stack of drafts', () => {
  const { calls } = mountHarness();

  const event = press('c', { repeat: true });

  assert.deepEqual(calls, []);
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: meta+c stays a copy, not a create', () => {
  const { calls } = mountHarness();

  const event = press('c', { metaKey: true });

  assert.deepEqual(calls, [], 'Cmd+C must stay free for the clipboard');
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: meta+c copies and consumes the key when handled', () => {
  const { calls } = mountHarness(false, true, { copy: true });

  const event = press('c', { metaKey: true });

  assert.deepEqual(calls, ['copy']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: meta+c leaves the chord alone when nothing is copied', () => {
  const { calls } = mountHarness(false, true, { copy: false });

  const event = press('c', { metaKey: true });

  assert.deepEqual(calls, []);
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: meta+d duplicates and consumes the key', () => {
  const { calls } = mountHarness(false, true, { duplicate: true });

  const event = press('d', { metaKey: true });

  assert.deepEqual(calls, ['duplicate']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: an unhandled meta+d stays a bookmark', () => {
  const { calls } = mountHarness();

  const event = press('d', { metaKey: true });

  assert.deepEqual(calls, []);
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: ctrl+v pastes when the clipboard has an item', () => {
  const { calls } = mountHarness(false, true, { paste: true });

  const event = press('v', { ctrlKey: true });

  assert.deepEqual(calls, ['paste']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: meta+x cuts when something is selected', () => {
  const { calls } = mountHarness(false, true, { cut: true });

  const event = press('x', { metaKey: true });

  assert.deepEqual(calls, ['cut']);
  assert.equal(event.defaultPrevented, true);
});

test('useCalendarShortcuts: meta+c typed in an input copies text, not the event', () => {
  const { calls } = mountHarness(false, true, { copy: true });
  const input = document.createElement('input');
  document.body.appendChild(input);

  try {
    const event = press('c', { metaKey: true }, input);
    assert.equal(event.target, input);
    assert.deepEqual(calls, [], 'the field keeps its own clipboard chord');
    assert.equal(event.defaultPrevented, false);
  } finally {
    input.remove();
  }
});

test('useCalendarShortcuts: meta+t does not go to today', () => {
  const { calls } = mountHarness();

  const event = press('t', { metaKey: true });

  assert.deepEqual(calls, []);
  assert.equal(event.defaultPrevented, false);
});

test('useCalendarShortcuts: c typed in an input does not create', () => {
  const { calls } = mountHarness();
  const input = document.createElement('input');
  document.body.appendChild(input);

  try {
    const event = press('c', {}, input);
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

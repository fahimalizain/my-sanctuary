// Unit tests for the calendar keymap. The resolver itself is DOM-free — it
// takes plain key fields, not a KeyboardEvent — but `isTextEditingTarget`
// reads real elements, so a JSDOM environment is installed for those cases.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { installDom } from '../components/install-dom';
import {
  isTextEditingTarget,
  resolveCalendarShortcut,
  type CalendarShortcutInput,
} from './calendar-shortcuts';

installDom();

/** No modifier held, no repeat: the state a bare key arrives in. */
const IDLE_INPUT: CalendarShortcutInput = {
  key: '',
  metaKey: false,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  repeat: false,
};

const resolve = (
  key: string,
  overrides: Partial<CalendarShortcutInput> = {},
): ReturnType<typeof resolveCalendarShortcut> =>
  resolveCalendarShortcut({ ...IDLE_INPUT, key, ...overrides });

// ── Period length ───────────────────────────────────────────────────────

test('resolveCalendarShortcut: 1, d, D select the one-day view', () => {
  for (const key of ['1', 'd', 'D']) {
    assert.deepEqual(
      resolve(key),
      { type: 'setPeriod', length: 1 },
      `key ${key}`,
    );
  }
});

test('resolveCalendarShortcut: 2–6 select that many days', () => {
  for (const key of ['2', '3', '4', '5', '6']) {
    assert.deepEqual(
      resolve(key),
      { type: 'setPeriod', length: Number(key) },
      `key ${key}`,
    );
  }
});

test('resolveCalendarShortcut: w, W, 0 select the week view', () => {
  for (const key of ['w', 'W', '0']) {
    assert.deepEqual(
      resolve(key),
      { type: 'setPeriod', length: 7 },
      `key ${key}`,
    );
  }
});

// ── Today ───────────────────────────────────────────────────────────────

test('resolveCalendarShortcut: t goes to today', () => {
  assert.deepEqual(resolve('t'), { type: 'today' });
});

test('resolveCalendarShortcut: Shift is not a blocker, so T still goes to today', () => {
  // Shift+t reports key "T" with shiftKey true. The shifted character is
  // already resolved, so shiftKey must not veto the shortcut.
  assert.deepEqual(resolve('T', { shiftKey: true }), { type: 'today' });
});

// ── Period stepping ─────────────────────────────────────────────────────

test('resolveCalendarShortcut: [ steps back one period', () => {
  assert.deepEqual(resolve('['), { type: 'shiftPeriod', delta: -1 });
});

test('resolveCalendarShortcut: ] steps forward one period', () => {
  assert.deepEqual(resolve(']'), { type: 'shiftPeriod', delta: 1 });
});

test('resolveCalendarShortcut: auto-repeat still steps the period', () => {
  // Holding ] keeps paging; `repeat` only says the key is held down.
  assert.deepEqual(resolve(']', { repeat: true }), {
    type: 'shiftPeriod',
    delta: 1,
  });
});

// ── Create / delete ─────────────────────────────────────────────────────

test('resolveCalendarShortcut: c and Shift+c create a draft', () => {
  assert.deepEqual(resolve('c'), { type: 'create' });
  // Shift+c reports key "C" with shiftKey true.
  assert.deepEqual(resolve('C', { shiftKey: true }), { type: 'create' });
});

test('resolveCalendarShortcut: Backspace and Delete request the delete dialog', () => {
  for (const key of ['Backspace', 'Delete']) {
    assert.deepEqual(resolve(key), { type: 'requestDelete' }, `key ${key}`);
  }
});

test('resolveCalendarShortcut: holding create or delete does not repeat', () => {
  // A held key must not open a stack of drafts or dialogs.
  for (const key of ['c', 'C', 'Backspace', 'Delete']) {
    assert.equal(resolve(key, { repeat: true }), null, `key ${key}`);
  }
});

test('resolveCalendarShortcut: chords never create or delete', () => {
  // Cmd+C must stay free for the clipboard slice.
  assert.equal(resolve('c', { metaKey: true }), null);
  assert.equal(resolve('c', { ctrlKey: true }), null);
  assert.equal(resolve('Backspace', { altKey: true }), null);
});

test('resolveCalendarShortcut: d is the one-day view, not delete', () => {
  assert.deepEqual(resolve('d'), { type: 'setPeriod', length: 1 });
  assert.deepEqual(resolve('D'), { type: 'setPeriod', length: 1 });
});

// ── Chords and unrelated keys ───────────────────────────────────────────

test('resolveCalendarShortcut: meta/ctrl/alt chords are left to the browser', () => {
  // Cmd+T is a new tab and Cmd+[ is Back; the page must never claim them.
  const chords: Partial<CalendarShortcutInput>[] = [
    { metaKey: true },
    { ctrlKey: true },
    { altKey: true },
  ];
  for (const chord of chords) {
    for (const key of ['t', '1', '[']) {
      assert.equal(
        resolve(key, chord),
        null,
        `${JSON.stringify(chord)} + ${key}`,
      );
    }
  }
});

test('resolveCalendarShortcut: navigation and unrelated keys are not ours', () => {
  for (const key of ['ArrowLeft', 'Escape', '?']) {
    assert.equal(resolve(key), null, `key ${key}`);
  }
});

// ── Editing targets ─────────────────────────────────────────────────────

test('isTextEditingTarget: null and non-elements are not editing targets', () => {
  assert.equal(isTextEditingTarget(null), false);
  assert.equal(isTextEditingTarget(new EventTarget()), false);
  assert.equal(
    isTextEditingTarget({ tagName: 'INPUT' } as unknown as EventTarget),
    false,
    'a duck-typed object is not an element',
  );
});

test('isTextEditingTarget: inputs, textareas, and selects are editing targets', () => {
  for (const tag of ['input', 'textarea', 'select']) {
    assert.equal(
      isTextEditingTarget(document.createElement(tag)),
      true,
      `element ${tag}`,
    );
  }
});

test('isTextEditingTarget: a contentEditable region is an editing target', () => {
  const region = document.createElement('div');
  // jsdom has no isContentEditable implementation; the hook reads this flag.
  Object.defineProperty(region, 'isContentEditable', { value: true });
  assert.equal(isTextEditingTarget(region), true);
});

test('isTextEditingTarget: buttons and plain divs are not editing targets', () => {
  assert.equal(isTextEditingTarget(document.createElement('button')), false);
  assert.equal(isTextEditingTarget(document.createElement('div')), false);
});

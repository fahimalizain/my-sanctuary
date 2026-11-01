// Mount tests for the task row's finished states. A parked `DISCARDED` task
// lives in the Completed dump (ADR 0004), so it has to READ as finished —
// strikethrough title, dimmed card — while its control offers "reopen", not
// "complete": a red X instead of the filled check. `COMPLETED` keeps the
// check, a living row keeps the empty circle.
// The second half covers the row's `pending` slot: the control that owns the
// in-flight request swaps its glyph for the same-size spinner (and refuses a
// second click), while the rest of the row's mutating controls go disabled
// without a second spinner.
// DOM must exist before the component module is imported — installDom() then
// dynamic import (see EventChip.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import type { AgendaItemRecord, TaskRecord } from '../../types';
import { installDom } from '../calendar/components/install-dom';

installDom();

let AgendaItemRow: typeof import('./AgendaItemRow').AgendaItemRow;
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
  const mod = await import('./AgendaItemRow');
  AgendaItemRow = mod.AgendaItemRow;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

const TODAY = '2026-08-23';

/** The task embed of one agenda row (the field set agenda-helpers.test.ts
 *  builds — kept local, those helpers are not exported). */
function task(status: TaskRecord['status'], title: string): TaskRecord {
  return {
    id: 'task-1',
    user_id: 'u-1',
    title,
    display_title: title,
    description: '',
    duration_minutes: 15,
    priority: 'medium',
    difficulty: 'easy',
    sort_order: 0,
    status,
    created_at: '2026-08-23T00:00:00Z',
    updated_at: '2026-08-23T00:00:00Z',
    focused: false,
    category: {
      id: 'cat-1',
      title: 'Work',
      slug: 'work',
      list_id: 'list-1',
      inherited_list_id: null,
      is_untracked: false,
      color: '#2a5c8a',
    },
  };
}

function taskItem(
  status: TaskRecord['status'],
  title: string,
): AgendaItemRecord {
  return {
    id: 'row-1',
    user_id: 'u-1',
    local_date: TODAY,
    kind: 'task',
    ref_id: 'task-1',
    sort_order: 0,
    task: task(status, title),
    occurrence: null,
  };
}

/** Render one row and return its root. */
function mount(
  item: AgendaItemRecord,
  {
    onCompleteTask = () => {},
    onPauseTask = () => {},
    pending = null,
    showHandle = false,
  }: {
    onCompleteTask?: (row: AgendaItemRecord) => void;
    onPauseTask?: (row: AgendaItemRecord) => void;
    pending?: 'check' | 'play' | 'calendar' | 'grip' | null;
    showHandle?: boolean;
  } = {},
): HTMLElement {
  render(
    createElement(AgendaItemRow, {
      item,
      isDragging: false,
      showHandle,
      pending,
      serverToday: TODAY,
      onReschedule: () => {},
      onMoveToBacklog: () => {},
      onUnschedule: () => {},
      onCompleteTask,
      onStartTask: () => {},
      onPauseTask,
      onOpenTask: () => {},
      onCompleteOccurrence: () => {},
      onSkipOccurrence: () => {},
      onStartOccurrence: () => {},
      onPauseOccurrence: () => {},
      onRenameOccurrence: () => {},
    }),
  );
  const root = document.getElementById(`agenda-item-${item.id}`);
  assert.ok(root, `row #agenda-item-${item.id} rendered`);
  return root;
}

/** The control's glyph is the in-flight spinner (lucide's Loader2). */
function isSpinning(el: Element): boolean {
  const svg = el.querySelector('svg');
  return !!svg && (svg.getAttribute('class') ?? '').includes('animate-spin');
}

// ── DISCARDED ───────────────────────────────────────────────────────────

test('discarded row is crossed out and reopens through a red X', () => {
  const item = taskItem('DISCARDED', 'Buy Chicken');
  const calls: AgendaItemRecord[] = [];
  const root = mount(item, { onCompleteTask: (row) => calls.push(row) });

  assert.ok(
    screen.getByText('Buy Chicken').className.includes('line-through'),
    'the title is struck through',
  );
  assert.ok(
    root.className.includes('opacity-60'),
    'the card is muted like a done row',
  );

  const undo = screen.getByRole('button', {
    name: 'Undo discard — Buy Chicken',
  });
  const circle = undo.querySelector('span');
  assert.ok(circle, 'the X sits on a circle');
  assert.ok(circle.className.includes('border-rose-500'), 'rose border');
  assert.ok(circle.className.includes('bg-rose-500/15'), 'faint rose disc');
  assert.ok(circle.className.includes('text-rose-500'), 'red mark');
  assert.ok(circle.querySelector('svg'), 'the mark is an icon');

  assert.equal(
    screen.queryByRole('button', { name: 'Complete Buy Chicken' }),
    null,
    'a discarded row is never completed from the row',
  );
  assert.equal(screen.queryByText('Done'), null, 'no Done chip');

  fireEvent.click(undo);
  assert.equal(calls.length, 1, 'one click, one reopen');
  assert.equal(calls[0], item, 'HomePage gets the row it owns');
});

// ── COMPLETED ───────────────────────────────────────────────────────────

test('completed row keeps the filled check and the Done chip', () => {
  const item = taskItem('COMPLETED', 'Buy Chicken');
  const root = mount(item);

  assert.ok(
    screen.getByText('Buy Chicken').className.includes('line-through'),
    'the title is struck through',
  );
  assert.ok(root.className.includes('opacity-60'), 'the card is muted');

  assert.ok(
    screen.getByRole('button', { name: 'Undo done — Buy Chicken' }),
    'uncheck is offered',
  );
  assert.ok(screen.getByText('Done'), 'done rows still carry their chip');
  assert.equal(
    screen.queryByRole('button', { name: 'Undo discard — Buy Chicken' }),
    null,
    'no discard control on a done row',
  );
});

// ── living rows ─────────────────────────────────────────────────────────

test('a planned row is open-looking and completes', () => {
  const item = taskItem('PLANNED', 'Buy Chicken');
  const root = mount(item);

  assert.equal(
    screen.getByText('Buy Chicken').className.includes('line-through'),
    false,
    'a living title is not struck through',
  );
  assert.equal(
    root.className.includes('opacity-60'),
    false,
    'a living card is not muted',
  );
  assert.ok(
    screen.getByRole('button', { name: 'Complete Buy Chicken' }),
    'a living row completes',
  );
});

// ── a mutation in flight (the row's `pending` slot) ─────────────────────

test('a planned row spins its check and ignores a second click', () => {
  const item = taskItem('PLANNED', 'Buy Chicken');
  const calls: AgendaItemRecord[] = [];
  mount(item, { pending: 'check', onCompleteTask: (row) => calls.push(row) });

  const complete = screen.getByRole('button', { name: 'Complete Buy Chicken' });
  assert.equal(
    (complete as HTMLButtonElement).disabled,
    true,
    'the circle is disabled while it saves',
  );
  assert.equal(
    complete.getAttribute('aria-busy'),
    'true',
    'the circle announces the save',
  );
  assert.ok(isSpinning(complete), 'the mark is the in-flight spinner');

  const circle = complete.querySelector('span');
  assert.ok(circle, 'spinny mark still sits in the 20px circle');
  assert.equal(
    circle.className.includes('bg-primary'),
    false,
    'a saving circle is neutral, never a filled check',
  );

  fireEvent.click(complete);
  assert.equal(calls.length, 0, 'a busy row fires no second mutation');
});

test('a discarded row spins its undo-discard and drops the rose mark', () => {
  const item = taskItem('DISCARDED', 'Buy Chicken');
  mount(item, { pending: 'check' });

  const undo = screen.getByRole('button', {
    name: 'Undo discard — Buy Chicken',
  });
  assert.equal((undo as HTMLButtonElement).disabled, true);
  assert.equal(undo.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(undo), 'the X is the in-flight spinner');

  const circle = undo.querySelector('span');
  assert.ok(circle, 'the spinner keeps the circle box');
  assert.equal(
    circle.className.includes('bg-rose-500'),
    false,
    'no rose disc while saving',
  );
  assert.equal(
    circle.className.includes('border-rose-500'),
    false,
    'no rose ring while saving',
  );
});

test('a completed row spins its undo-done and keeps the Done chip', () => {
  const item = taskItem('COMPLETED', 'Buy Chicken');
  mount(item, { pending: 'check' });

  const undo = screen.getByRole('button', { name: 'Undo done — Buy Chicken' });
  assert.equal((undo as HTMLButtonElement).disabled, true);
  assert.equal(undo.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(undo), 'the check is the in-flight spinner');
  assert.ok(screen.getByText('Done'), 'the row is still a done row');

  const circle = undo.querySelector('span');
  assert.ok(circle);
  assert.equal(
    circle.className.includes('bg-primary'),
    false,
    'no filled check while saving',
  );
});

test('an in-progress row spins Pause and ignores a second click', () => {
  const item = taskItem('IN_PROGRESS', 'Buy Chicken');
  const calls: AgendaItemRecord[] = [];
  mount(item, { pending: 'play', onPauseTask: (row) => calls.push(row) });

  const pause = screen.getByRole('button', { name: 'Pause Buy Chicken' });
  assert.equal((pause as HTMLButtonElement).disabled, true);
  assert.equal(pause.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(pause), 'Pause becomes the spinner');

  fireEvent.click(pause);
  assert.equal(calls.length, 0, 'a busy row fires no second pause');
});

test('a planned row spins its calendar trigger while the move runs', () => {
  const item = taskItem('PLANNED', 'Buy Chicken');
  mount(item, { pending: 'calendar' });

  const move = screen.getByRole('button', {
    name: 'Move Buy Chicken to another day',
  });
  assert.equal((move as HTMLButtonElement).disabled, true);
  assert.equal(move.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(move), 'the calendar icon becomes the spinner');
});

test('a busy calendar still renders on a row that cannot reschedule', () => {
  // `IN_PROGRESS` tasks are never reschedulable, but the move that IS running
  // still owns the slot — the trigger must exist to show its spinner.
  const item = taskItem('IN_PROGRESS', 'Buy Chicken');
  mount(item, { pending: 'calendar' });

  const move = screen.getByRole('button', {
    name: 'Move Buy Chicken to another day',
  });
  assert.equal((move as HTMLButtonElement).disabled, true);
  assert.equal(move.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(move), 'the calendar icon becomes the spinner');
});

test('a dragged row spins its grip', () => {
  const item = taskItem('PLANNED', 'Buy Chicken');
  mount(item, { pending: 'grip', showHandle: true });

  const grip = screen.getByRole('button', { name: 'Reorder Buy Chicken' });
  assert.equal((grip as HTMLButtonElement).disabled, true);
  assert.equal(grip.getAttribute('aria-busy'), 'true');
  assert.ok(isSpinning(grip), 'the grip becomes the spinner');
});

test('a row saving its check quiets the other controls without spinning them', () => {
  const item = taskItem('PLANNED', 'Buy Chicken');
  mount(item, { pending: 'check', showHandle: true });

  const others: Array<[string, HTMLElement]> = [
    ['play', screen.getByRole('button', { name: 'Start Buy Chicken' })],
    [
      'calendar',
      screen.getByRole('button', { name: 'Move Buy Chicken to another day' }),
    ],
    ['grip', screen.getByRole('button', { name: 'Reorder Buy Chicken' })],
  ];
  for (const [name, control] of others) {
    assert.equal(
      (control as HTMLButtonElement).disabled,
      true,
      `${name} cannot race the check`,
    );
    assert.equal(
      control.getAttribute('aria-busy'),
      null,
      `${name} keeps its own glyph — no second spinner`,
    );
    assert.equal(isSpinning(control), false, `${name} is not the spinner slot`);
  }
});

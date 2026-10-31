// Mount tests for the task row's finished states. A parked `DISCARDED` task
// lives in the Completed dump (ADR 0004), so it has to READ as finished —
// strikethrough title, dimmed card — while its control offers "reopen", not
// "complete": a red X instead of the filled check. `COMPLETED` keeps the
// check, a living row keeps the empty circle.
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

/** Render one row (no sortable wrapper, no grip) and return its root. */
function mount(
  item: AgendaItemRecord,
  onCompleteTask: (row: AgendaItemRecord) => void = () => {},
): HTMLElement {
  render(
    createElement(AgendaItemRow, {
      item,
      isDragging: false,
      showHandle: false,
      serverToday: TODAY,
      onReschedule: () => {},
      onMoveToBacklog: () => {},
      onUnschedule: () => {},
      onCompleteTask,
      onStartTask: () => {},
      onPauseTask: () => {},
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

// ── DISCARDED ───────────────────────────────────────────────────────────

test('discarded row is crossed out and reopens through a red X', () => {
  const item = taskItem('DISCARDED', 'Buy Chicken');
  const calls: AgendaItemRecord[] = [];
  const root = mount(item, (row) => calls.push(row));

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

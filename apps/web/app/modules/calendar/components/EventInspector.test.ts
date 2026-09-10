// Mount tests for EventInspector (Notion-density body). DOM must exist
// before the component module is imported — installDom() then dynamic import.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ComponentType } from 'react';
import type { CalendarEvent, GoogleCalendar } from '@/app/types';
import { installDom, setMatchMediaDesktop } from './install-dom';

installDom();

type EventInspectorProps = {
  event: CalendarEvent;
  calendar?: GoogleCalendar;
  focusTitle?: boolean;
  onClose: () => void;
  onSaveTitle: (summary: string) => void | Promise<void>;
  onSaveDescription: (description: string) => void | Promise<void>;
  onDelete: () => void | Promise<void>;
  isSaving?: boolean;
  isDeleting?: boolean;
  isDragging?: boolean;
};

let EventInspector: ComponentType<EventInspectorProps>;
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
  const mod = await import('./EventInspector');
  EventInspector = mod.EventInspector;
});

afterEach(() => {
  cleanup();
  // Reset to mobile default between cases.
  setMatchMediaDesktop(false);
});

after(() => {
  cleanup();
});

function makeEvent(
  overrides: Partial<CalendarEvent> & Pick<CalendarEvent, 'id'>,
): CalendarEvent {
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);
  return {
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title: 'Morning standup',
    description: '',
    start_time: start.toISOString(),
    end_time: end.toISOString(),
    last_synced_at: '2026-09-08T00:00:00.000Z',
    ...overrides,
  };
}

function makeCalendar(overrides?: Partial<GoogleCalendar>): GoogleCalendar {
  return {
    id: 'cal-1',
    google_calendar_id: 'primary',
    summary: 'Personal Goals',
    time_zone: 'America/New_York',
    is_primary: true,
    access_role: 'owner',
    sync_enabled: true,
    ...overrides,
  };
}

type Spies = {
  onClose: (...args: unknown[]) => void;
  onSaveTitle: (...args: unknown[]) => void;
  onSaveDescription: (...args: unknown[]) => void;
  onDelete: (...args: unknown[]) => void;
  closeCalls: unknown[][];
  saveCalls: unknown[][];
  saveDescriptionCalls: unknown[][];
  deleteCalls: unknown[][];
};

function makeSpies(): Spies {
  const closeCalls: unknown[][] = [];
  const saveCalls: unknown[][] = [];
  const saveDescriptionCalls: unknown[][] = [];
  const deleteCalls: unknown[][] = [];
  return {
    closeCalls,
    saveCalls,
    saveDescriptionCalls,
    deleteCalls,
    onClose: (...args: unknown[]) => {
      closeCalls.push(args);
    },
    onSaveTitle: (...args: unknown[]) => {
      saveCalls.push(args);
    },
    onSaveDescription: (...args: unknown[]) => {
      saveDescriptionCalls.push(args);
    },
    onDelete: (...args: unknown[]) => {
      deleteCalls.push(args);
    },
  };
}

function mount(
  props: {
    event?: CalendarEvent;
    calendar?: GoogleCalendar | undefined;
    focusTitle?: boolean;
  } = {},
  spies = makeSpies(),
) {
  const event = props.event ?? makeEvent({ id: 'e1' });
  const calendar = 'calendar' in props ? props.calendar : makeCalendar();
  render(
    createElement(EventInspector, {
      event,
      calendar,
      focusTitle: props.focusTitle,
      onClose: spies.onClose,
      onSaveTitle: spies.onSaveTitle,
      onSaveDescription: spies.onSaveDescription,
      onDelete: spies.onDelete,
    }),
  );
  return { spies, event };
}

// ── 1. Timed event when-block ───────────────────────────────────────────

test('timed event when-block shows Notion-style range and date line', () => {
  mount();
  assert.ok(screen.getByText('5:15 AM → 5:30 AM 15min'));
  assert.ok(screen.getByText('Sun Sep 13'));
});

// ── 2. Title commit ─────────────────────────────────────────────────────

test('title commit: trim on blur; unchanged and blank are no-ops', () => {
  const spies = makeSpies();
  mount({ event: makeEvent({ id: 'e1', title: 'Morning standup' }) }, spies);

  const input = screen.getByLabelText('Title') as HTMLInputElement;
  assert.equal(input.value, 'Morning standup');

  // Change to trimmed new title → onSaveTitle once.
  fireEvent.change(input, { target: { value: '  New title  ' } });
  fireEvent.blur(input);
  assert.equal(spies.saveCalls.length, 1);
  assert.equal(spies.saveCalls[0][0], 'New title');

  // Same as current saved → no-op.
  fireEvent.change(input, { target: { value: 'New title' } });
  fireEvent.blur(input);
  assert.equal(spies.saveCalls.length, 1);

  // Clear to blank → no-op, input restored to last saved.
  fireEvent.change(input, { target: { value: '' } });
  fireEvent.blur(input);
  assert.equal(spies.saveCalls.length, 1);
  assert.equal(input.value, 'New title');
});

// ── 3. Description ──────────────────────────────────────────────────────

test('description: writable textarea with placeholder when empty', () => {
  mount({ event: makeEvent({ id: 'e1', description: '' }) });
  const ta = screen.getByLabelText('Description') as HTMLTextAreaElement;
  assert.equal(ta.tagName, 'TEXTAREA');
  assert.equal(ta.value, '');
  assert.equal(ta.placeholder, 'Description');
});

test('description: writable textarea shows non-empty value', () => {
  mount({
    event: makeEvent({ id: 'e2', description: 'Bring snacks' }),
  });
  const ta = screen.getByLabelText('Description') as HTMLTextAreaElement;
  assert.equal(ta.value, 'Bring snacks');
});

test('description commit: trim on blur; unchanged no-op; clear commits empty', () => {
  const spies = makeSpies();
  mount(
    { event: makeEvent({ id: 'e3', description: 'Bring snacks' }) },
    spies,
  );

  const ta = screen.getByLabelText('Description') as HTMLTextAreaElement;
  assert.equal(ta.value, 'Bring snacks');

  // Changed + trimmed → onSaveDescription once.
  fireEvent.change(ta, { target: { value: '  Pack lunch  ' } });
  fireEvent.blur(ta);
  assert.equal(spies.saveDescriptionCalls.length, 1);
  assert.equal(spies.saveDescriptionCalls[0][0], 'Pack lunch');

  // Same after trim → no-op.
  fireEvent.change(ta, { target: { value: 'Pack lunch' } });
  fireEvent.blur(ta);
  assert.equal(spies.saveDescriptionCalls.length, 1);

  // Clear to empty → commits "" (unlike title).
  fireEvent.change(ta, { target: { value: '' } });
  fireEvent.blur(ta);
  assert.equal(spies.saveDescriptionCalls.length, 2);
  assert.equal(spies.saveDescriptionCalls[1][0], '');

  // Whitespace-only → treated as empty; already saved "" → no-op.
  fireEvent.change(ta, { target: { value: '   ' } });
  fireEvent.blur(ta);
  assert.equal(spies.saveDescriptionCalls.length, 2);
  assert.equal(ta.value, '');
});

// ── 4. Calendar row ─────────────────────────────────────────────────────

test('calendar row shows calendar.summary', () => {
  mount({ calendar: makeCalendar({ summary: 'Personal Goals' }) });
  assert.ok(screen.getByText('Personal Goals'));
});

// ── 5. Chips ────────────────────────────────────────────────────────────

test('chips: all-day + zone + recurrence show; plain one-shot shows none', () => {
  mount({
    event: makeEvent({
      id: 'e-chips',
      is_all_day: true,
      start_time_zone: 'America/New_York',
      recurrence: '["RRULE:FREQ=DAILY"]',
    }),
  });
  assert.ok(screen.getByText('All-day'));
  assert.ok(screen.getByText('America/New_York'));
  assert.ok(screen.getByText('Repeat'));
  cleanup();

  mount({
    event: makeEvent({
      id: 'e-plain',
      // Explicit one-shot: no all-day, zone, recurrence, or recurring id.
      is_all_day: false,
      start_time_zone: '',
      recurrence: '',
      recurring_event_id: '',
    }),
  });
  assert.equal(screen.queryByText('All-day'), null);
  assert.equal(screen.queryByText('America/New_York'), null);
  assert.equal(screen.queryByText('Repeat'), null);
});

// ── 6. Repeat via recurring_event_id ────────────────────────────────────

test('Repeat chip via recurring_event_id alone', () => {
  mount({
    event: makeEvent({
      id: 'e-instance',
      recurring_event_id: 'master-1',
    }),
  });
  assert.ok(screen.getByText('Repeat'));
});

// ── 7. Overflow Delete ──────────────────────────────────────────────────

test('overflow Delete calls onDelete; available for owner/writer/omitted', () => {
  const spies = makeSpies();
  mount({ calendar: makeCalendar({ access_role: 'owner' }) }, spies);

  const more = screen.getByLabelText('More actions');
  fireEvent.click(more);
  const del = screen.getByLabelText('Delete event');
  fireEvent.click(del);
  assert.equal(spies.deleteCalls.length, 1);
  cleanup();

  // Writer
  mount({ calendar: makeCalendar({ access_role: 'writer' }) });
  assert.ok(screen.getByLabelText('More actions'));
  cleanup();

  // Omitted calendar → still writable
  mount({ calendar: undefined });
  assert.ok(screen.getByLabelText('More actions'));
});

// ── 8. Read-only ────────────────────────────────────────────────────────

test('read-only reader and freeBusyReader: static title/description, no Delete', () => {
  for (const role of ['reader', 'freeBusyReader'] as const) {
    cleanup();
    mount({
      event: makeEvent({
        id: `ro-${role}`,
        title: 'Locked event',
        description: 'Secret notes',
      }),
      calendar: makeCalendar({ access_role: role }),
    });
    // Title text visible, no Title input.
    assert.ok(screen.getByText('Locked event'));
    assert.equal(screen.queryByLabelText('Title'), null);
    // Description is static text — no textarea.
    assert.ok(screen.getByText('Secret notes'));
    assert.equal(screen.queryByLabelText('Description'), null);
    assert.equal(document.querySelector('textarea'), null);
    assert.equal(screen.queryByLabelText('More actions'), null);
    assert.equal(screen.queryByLabelText('Delete event'), null);
  }
});

test('read-only empty description shows static placeholder', () => {
  mount({
    event: makeEvent({ id: 'ro-empty-desc', description: '   ' }),
    calendar: makeCalendar({ access_role: 'reader' }),
  });
  assert.ok(screen.getByText('Description'));
  assert.equal(screen.queryByLabelText('Description'), null);
  assert.equal(document.querySelector('textarea'), null);
});

// ── 9. data-event-inspector on both shells ──────────────────────────────

test('data-event-inspector present on mobile and desktop shells', () => {
  // Mobile (default matchMedia)
  setMatchMediaDesktop(false);
  mount({ event: makeEvent({ id: 'mobile-shell' }) });
  assert.ok(document.querySelector('[data-event-inspector]'));
  cleanup();

  // Desktop
  setMatchMediaDesktop(true);
  mount({ event: makeEvent({ id: 'desktop-shell' }) });
  assert.ok(document.querySelector('[data-event-inspector]'));
});

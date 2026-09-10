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
  calendars?: GoogleCalendar[];
  focusTitle?: boolean;
  onClose: () => void;
  onSaveTitle: (summary: string) => void | Promise<void>;
  onSaveDescription: (description: string) => void | Promise<void>;
  onSaveTimes: (startIso: string, endIso: string) => void | Promise<void>;
  onSaveAllDay: (next: {
    isAllDay: boolean;
    startIso: string;
    endIso: string;
  }) => void | Promise<void>;
  onSaveTimeZone: (
    timeZone: string,
    startIso: string,
    endIso: string,
  ) => void | Promise<void>;
  onSaveCalendar: (calendarId: string) => void | Promise<void>;
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
  onSaveTimes: (...args: unknown[]) => void;
  onSaveAllDay: (...args: unknown[]) => void;
  onSaveTimeZone: (...args: unknown[]) => void;
  onSaveCalendar: (...args: unknown[]) => void;
  onDelete: (...args: unknown[]) => void;
  closeCalls: unknown[][];
  saveCalls: unknown[][];
  saveDescriptionCalls: unknown[][];
  saveTimesCalls: unknown[][];
  saveAllDayCalls: unknown[][];
  saveTimeZoneCalls: unknown[][];
  saveCalendarCalls: unknown[][];
  deleteCalls: unknown[][];
};

function makeSpies(): Spies {
  const closeCalls: unknown[][] = [];
  const saveCalls: unknown[][] = [];
  const saveDescriptionCalls: unknown[][] = [];
  const saveTimesCalls: unknown[][] = [];
  const saveAllDayCalls: unknown[][] = [];
  const saveTimeZoneCalls: unknown[][] = [];
  const saveCalendarCalls: unknown[][] = [];
  const deleteCalls: unknown[][] = [];
  return {
    closeCalls,
    saveCalls,
    saveDescriptionCalls,
    saveTimesCalls,
    saveAllDayCalls,
    saveTimeZoneCalls,
    saveCalendarCalls,
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
    onSaveTimes: (...args: unknown[]) => {
      saveTimesCalls.push(args);
    },
    onSaveAllDay: (...args: unknown[]) => {
      saveAllDayCalls.push(args);
    },
    onSaveTimeZone: (...args: unknown[]) => {
      saveTimeZoneCalls.push(args);
    },
    onSaveCalendar: (...args: unknown[]) => {
      saveCalendarCalls.push(args);
    },
    onDelete: (...args: unknown[]) => {
      deleteCalls.push(args);
    },
  };
}

function defaultCalendars(): GoogleCalendar[] {
  return [
    makeCalendar({ id: 'cal-1', summary: 'Personal Goals' }),
    makeCalendar({
      id: 'cal-2',
      google_calendar_id: 'work@example.com',
      summary: 'Work',
      is_primary: false,
      access_role: 'writer',
    }),
    makeCalendar({
      id: 'cal-reader',
      google_calendar_id: 'shared@example.com',
      summary: 'Shared (read)',
      is_primary: false,
      access_role: 'reader',
    }),
  ];
}

function mount(
  props: {
    event?: CalendarEvent;
    calendar?: GoogleCalendar | undefined;
    calendars?: GoogleCalendar[];
    focusTitle?: boolean;
  } = {},
  spies = makeSpies(),
) {
  const event = props.event ?? makeEvent({ id: 'e1' });
  const calendar = 'calendar' in props ? props.calendar : makeCalendar();
  const calendars = props.calendars ?? defaultCalendars();
  render(
    createElement(EventInspector, {
      event,
      calendar,
      calendars,
      focusTitle: props.focusTitle,
      onClose: spies.onClose,
      onSaveTitle: spies.onSaveTitle,
      onSaveDescription: spies.onSaveDescription,
      onSaveTimes: spies.onSaveTimes,
      onSaveAllDay: spies.onSaveAllDay,
      onSaveTimeZone: spies.onSaveTimeZone,
      onSaveCalendar: spies.onSaveCalendar,
      onDelete: spies.onDelete,
    }),
  );
  return { spies, event };
}

// ── 1. Timed event when-block ───────────────────────────────────────────

test('timed writable when-block shows time/date inputs and duration', () => {
  mount();
  const startInput = screen.getByLabelText('Start time') as HTMLInputElement;
  const endInput = screen.getByLabelText('End time') as HTMLInputElement;
  const dateInput = screen.getByLabelText('Date') as HTMLInputElement;
  assert.equal(startInput.type, 'time');
  assert.equal(endInput.type, 'time');
  assert.equal(dateInput.type, 'date');
  assert.equal(startInput.value, '05:15');
  assert.equal(endInput.value, '05:30');
  assert.equal(dateInput.value, '2026-09-13');
  assert.ok(screen.getByText('15min'));
  // Static AM range text is replaced by inputs.
  assert.equal(screen.queryByText('5:15 AM → 5:30 AM 15min'), null);
});

test('timed when-block: start time change preserves duration', () => {
  const spies = makeSpies();
  mount({}, spies);
  const startInput = screen.getByLabelText('Start time') as HTMLInputElement;
  fireEvent.change(startInput, { target: { value: '06:00' } });
  assert.equal(spies.saveTimesCalls.length, 1);
  const expectedStart = new Date(2026, 8, 13, 6, 0);
  const expectedEnd = new Date(2026, 8, 13, 6, 15);
  assert.equal(spies.saveTimesCalls[0][0], expectedStart.toISOString());
  assert.equal(spies.saveTimesCalls[0][1], expectedEnd.toISOString());
});

test('timed when-block: end time change keeps start', () => {
  const spies = makeSpies();
  mount({}, spies);
  const endInput = screen.getByLabelText('End time') as HTMLInputElement;
  fireEvent.change(endInput, { target: { value: '07:00' } });
  assert.equal(spies.saveTimesCalls.length, 1);
  const expectedStart = new Date(2026, 8, 13, 5, 15);
  const expectedEnd = new Date(2026, 8, 13, 7, 0);
  assert.equal(spies.saveTimesCalls[0][0], expectedStart.toISOString());
  assert.equal(spies.saveTimesCalls[0][1], expectedEnd.toISOString());
});

test('timed when-block: end before start clamps to start+15min', () => {
  const spies = makeSpies();
  // Longer initial end so clamp (start+15) is a real change vs current.
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 7, 0);
  mount(
    {
      event: makeEvent({
        id: 'e-clamp',
        start_time: start.toISOString(),
        end_time: end.toISOString(),
      }),
    },
    spies,
  );
  const endInput = screen.getByLabelText('End time') as HTMLInputElement;
  fireEvent.change(endInput, { target: { value: '04:00' } });
  assert.equal(spies.saveTimesCalls.length, 1);
  const expectedStart = new Date(2026, 8, 13, 5, 15);
  const expectedEnd = new Date(2026, 8, 13, 5, 30);
  assert.equal(spies.saveTimesCalls[0][0], expectedStart.toISOString());
  assert.equal(spies.saveTimesCalls[0][1], expectedEnd.toISOString());
});

test('timed when-block: date change shifts both instants by one day', () => {
  const spies = makeSpies();
  mount({}, spies);
  const dateInput = screen.getByLabelText('Date') as HTMLInputElement;
  fireEvent.change(dateInput, { target: { value: '2026-09-14' } });
  assert.equal(spies.saveTimesCalls.length, 1);
  const expectedStart = new Date(2026, 8, 14, 5, 15);
  const expectedEnd = new Date(2026, 8, 14, 5, 30);
  assert.equal(spies.saveTimesCalls[0][0], expectedStart.toISOString());
  assert.equal(spies.saveTimesCalls[0][1], expectedEnd.toISOString());
});

test('timed when-block: unchanged change/blur does not call onSaveTimes', () => {
  const spies = makeSpies();
  mount({}, spies);
  const startInput = screen.getByLabelText('Start time') as HTMLInputElement;
  const endInput = screen.getByLabelText('End time') as HTMLInputElement;
  const dateInput = screen.getByLabelText('Date') as HTMLInputElement;

  fireEvent.change(startInput, { target: { value: '05:15' } });
  fireEvent.blur(startInput);
  fireEvent.change(endInput, { target: { value: '05:30' } });
  fireEvent.blur(endInput);
  fireEvent.change(dateInput, { target: { value: '2026-09-13' } });
  fireEvent.blur(dateInput);
  assert.equal(spies.saveTimesCalls.length, 0);
});

test('timed when-block: unchanged blur with Google-shaped times (no ms) is no-op', () => {
  // Google / replica often omits milliseconds; toISOString() always emits .000Z.
  // Instant compare must treat these as equal so blur does not no-op-PATCH.
  const spies = makeSpies();
  const start = new Date(2026, 8, 13, 5, 15);
  const end = new Date(2026, 8, 13, 5, 30);
  const stripMs = (iso: string) => iso.replace('.000Z', 'Z');
  mount(
    {
      event: makeEvent({
        id: 'e-noms',
        start_time: stripMs(start.toISOString()),
        end_time: stripMs(end.toISOString()),
      }),
    },
    spies,
  );
  const startInput = screen.getByLabelText('Start time') as HTMLInputElement;
  fireEvent.change(startInput, { target: { value: '05:15' } });
  fireEvent.blur(startInput);
  assert.equal(spies.saveTimesCalls.length, 0);
});

test('timed when-block: empty/invalid values do not call onSaveTimes', () => {
  const spies = makeSpies();
  mount({}, spies);
  const startInput = screen.getByLabelText('Start time') as HTMLInputElement;
  const dateInput = screen.getByLabelText('Date') as HTMLInputElement;
  fireEvent.change(startInput, { target: { value: '' } });
  fireEvent.change(dateInput, { target: { value: '' } });
  assert.equal(spies.saveTimesCalls.length, 0);
});

test('all-day writable: date input only, no time or timezone', () => {
  mount({
    event: makeEvent({
      id: 'e-allday',
      is_all_day: true,
      start_time: '2026-09-13T00:00:00Z',
      end_time: '2026-09-14T00:00:00Z',
    }),
  });
  assert.equal(screen.queryByLabelText('Start time'), null);
  assert.equal(screen.queryByLabelText('End time'), null);
  assert.equal(screen.queryByLabelText('Time zone'), null);
  const dateInput = screen.getByLabelText('Date') as HTMLInputElement;
  assert.equal(dateInput.type, 'date');
  assert.equal(dateInput.value, '2026-09-13');
  const allDay = screen.getByLabelText('All-day') as HTMLButtonElement;
  assert.equal(allDay.getAttribute('aria-pressed'), 'true');
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
  mount({ event: makeEvent({ id: 'e3', description: 'Bring snacks' }) }, spies);

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

test('writable calendar row is a select of writable calendars', () => {
  const spies = makeSpies();
  mount(
    {
      calendar: makeCalendar({ summary: 'Personal Goals' }),
      calendars: defaultCalendars(),
    },
    spies,
  );
  const select = screen.getByLabelText('Calendar') as HTMLSelectElement;
  assert.equal(select.value, 'cal-1');
  const optionLabels = Array.from(select.options).map((o) => o.textContent);
  assert.deepEqual(optionLabels, ['Personal Goals', 'Work']);
  // Reader calendar is not an option.
  assert.ok(!optionLabels.includes('Shared (read)'));

  fireEvent.change(select, { target: { value: 'cal-2' } });
  assert.equal(spies.saveCalendarCalls.length, 1);
  assert.equal(spies.saveCalendarCalls[0][0], 'cal-2');
});

test('calendar select same value is a no-op', () => {
  const spies = makeSpies();
  mount({}, spies);
  const select = screen.getByLabelText('Calendar') as HTMLSelectElement;
  fireEvent.change(select, { target: { value: 'cal-1' } });
  assert.equal(spies.saveCalendarCalls.length, 0);
});

test('read-only calendar row has no select; static summary remains', () => {
  mount({
    calendar: makeCalendar({
      access_role: 'reader',
      summary: 'Personal Goals',
    }),
    calendars: defaultCalendars(),
  });
  assert.equal(screen.queryByLabelText('Calendar'), null);
  assert.ok(screen.getByText('Personal Goals'));
});

// ── 5. Chips / all-day + time zone ──────────────────────────────────────

test('writable timed: All-day toggle + Time zone select present', () => {
  const spies = makeSpies();
  mount(
    {
      event: makeEvent({
        id: 'e-timed',
        is_all_day: false,
        start_time_zone: '',
      }),
    },
    spies,
  );
  const allDay = screen.getByLabelText('All-day') as HTMLButtonElement;
  assert.equal(allDay.tagName, 'BUTTON');
  assert.equal(allDay.getAttribute('aria-pressed'), 'false');
  assert.ok(screen.getByLabelText('Time zone'));

  fireEvent.click(allDay);
  assert.equal(spies.saveAllDayCalls.length, 1);
  const payload = spies.saveAllDayCalls[0][0] as {
    isAllDay: boolean;
    startIso: string;
    endIso: string;
  };
  assert.equal(payload.isAllDay, true);
  assert.equal(payload.startIso, '2026-09-13T00:00:00Z');
  // Exclusive end = day after last occupied (same civil day → next midnight).
  assert.equal(payload.endIso, '2026-09-14T00:00:00Z');
});

test('writable timed: Time zone change calls onSaveTimeZone', () => {
  const spies = makeSpies();
  const event = makeEvent({
    id: 'e-tz',
    is_all_day: false,
    start_time_zone: 'UTC',
  });
  mount({ event }, spies);
  const select = screen.getByLabelText('Time zone') as HTMLSelectElement;
  fireEvent.change(select, { target: { value: 'America/New_York' } });
  assert.equal(spies.saveTimeZoneCalls.length, 1);
  assert.equal(spies.saveTimeZoneCalls[0][0], 'America/New_York');
  assert.equal(spies.saveTimeZoneCalls[0][1], event.start_time);
  assert.equal(spies.saveTimeZoneCalls[0][2], event.end_time);
});

test('read-only: All-day is a span; no timezone select; Repeat display-only', () => {
  mount({
    calendar: makeCalendar({ access_role: 'reader' }),
    event: makeEvent({
      id: 'e-ro-chips',
      is_all_day: true,
      start_time: '2026-09-13T00:00:00Z',
      end_time: '2026-09-14T00:00:00Z',
      start_time_zone: 'America/New_York',
      recurrence: '["RRULE:FREQ=DAILY"]',
    }),
  });
  assert.equal(screen.queryByLabelText('All-day'), null);
  assert.ok(screen.getByText('All-day'));
  assert.equal(screen.queryByLabelText('Time zone'), null);
  // Zone still shown as static chip when set on read-only all-day.
  assert.ok(screen.getByText('America/New_York'));
  assert.ok(screen.getByText('Repeat'));
  // Repeat is not a button.
  assert.equal(screen.getByText('Repeat').tagName, 'SPAN');
});

test('writable plain one-shot still has All-day toggle (not only when true)', () => {
  mount({
    event: makeEvent({
      id: 'e-plain',
      is_all_day: false,
      start_time_zone: '',
      recurrence: '',
      recurring_event_id: '',
    }),
  });
  assert.ok(screen.getByLabelText('All-day'));
  assert.ok(screen.getByLabelText('Time zone'));
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

test('read-only reader and freeBusyReader: static title/description/when, no Delete', () => {
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
    // When-block stays static — no time/date inputs.
    assert.equal(screen.queryByLabelText('Start time'), null);
    assert.equal(screen.queryByLabelText('End time'), null);
    assert.equal(screen.queryByLabelText('Date'), null);
    assert.ok(screen.getByText('5:15 AM → 5:30 AM 15min'));
    assert.ok(screen.getByText('Sun Sep 13'));
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

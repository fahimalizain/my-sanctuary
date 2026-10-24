// Draft → create lifecycle. A draft is local-only until its title blur sends
// the POST; from that moment it is a real event and no discard path (empty-cell
// click, Escape, another chip, a new draft) may empty its overlay — an empty
// overlay makes create onSuccess DELETE the row it just made.
//
// These drive the real session hook inside the app-wide overlay provider, with
// `fetch` stubbed, so the discard branches, the queue and the create callbacks
// run together. DOM + API base must exist before the API chain is imported:
// static ESM imports hoist, so install them at module top and dynamic-import
// everything else in `before` (same pattern as BackgroundSync.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createElement,
  type ComponentType,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
  type RefObject,
} from 'react';
import type { QueryClient } from '@tanstack/react-query';
import type {
  CalendarEvent,
  NewCalendarEventInput,
  PatchCalendarEventInput,
} from '@/app/types';
import type { CalendarClipboardItem } from '../lib/calendar-clipboard';
import { installDom } from '../components/install-dom';

installDom();
(globalThis as { __API_BASE_URL__?: string }).__API_BASE_URL__ = '';

type SessionModule = typeof import('./useCalendarSession');
type Session = ReturnType<SessionModule['useCalendarSession']>;

let useCalendarSession: SessionModule['useCalendarSession'];
let CalendarEventsProvider: ComponentType<{ children?: ReactNode }>;
let QueryClientProvider: ComponentType<{
  client: QueryClient;
  children?: ReactNode;
}>;
let queryClient: QueryClient;
let findCachedCalendarEvent: typeof import('@/app/queries/calendar').findCachedCalendarEvent;
let render: typeof import('@testing-library/react').render;
let act: typeof import('@testing-library/react').act;
let waitFor: typeof import('@testing-library/react').waitFor;
let cleanup: () => void;

const CALENDARS = [
  {
    id: 'cal-1',
    google_calendar_id: 'google-1',
    summary: 'Primary',
    time_zone: 'UTC',
    is_primary: true,
    access_role: 'owner',
    sync_enabled: true,
  },
  {
    id: 'cal-2',
    google_calendar_id: 'google-2',
    summary: 'Work',
    time_zone: 'UTC',
    is_primary: false,
    access_role: 'writer',
    sync_enabled: true,
  },
];

const EMPTY_SYNC = { status: 'ok', calendars: [] };

interface Request {
  method: string;
  path: string;
  body?: unknown;
}

interface PendingPost {
  body: NewCalendarEventInput;
  respondWith: (event: CalendarEvent) => void;
}

let requests: Request[] = [];
let pendingPosts: PendingPost[] = [];
/** Server rows the events GET returns; tests that need a chip seed one here. */
let seededEvents: CalendarEvent[] = [];
/** Rows the fake API has stored: creates land here, PATCHes merge into them. */
let serverRows = new Map<string, CalendarEvent>();

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** The row the API would return for a create request. */
function createdEvent(
  body: NewCalendarEventInput,
  id: string,
  calendarId = body.calendar_id,
): CalendarEvent {
  const zone = body.start_time_zone?.trim();
  return {
    id,
    calendar_id: calendarId,
    google_event_id: `google-${id}`,
    title: body.summary,
    description: body.description ?? '',
    start_time: body.start,
    end_time: body.end,
    last_synced_at: '2026-10-18T00:00:00.000Z',
    ...(body.is_all_day ? { is_all_day: true } : {}),
    ...(!body.is_all_day && zone
      ? { start_time_zone: zone, end_time_zone: zone }
      : {}),
  };
}

/** Fold a PATCH body into the stored row, as the API would. */
function patchedEvent(
  id: string,
  patch: PatchCalendarEventInput,
): CalendarEvent {
  const base = serverRows.get(id) ??
    seededEvents.find((e) => e.id === id) ?? {
      id,
      calendar_id: patch.calendar_id ?? 'cal-1',
      google_event_id: `google-${id}`,
      title: '',
      description: '',
      start_time: '2026-10-18T09:00:00.000Z',
      end_time: '2026-10-18T10:00:00.000Z',
      last_synced_at: '2026-10-18T00:00:00.000Z',
    };
  const next: CalendarEvent = {
    ...base,
    ...(patch.summary !== undefined ? { title: patch.summary } : {}),
    ...(patch.description !== undefined
      ? { description: patch.description }
      : {}),
    ...(patch.start !== undefined ? { start_time: patch.start } : {}),
    ...(patch.end !== undefined ? { end_time: patch.end } : {}),
    ...(patch.is_all_day !== undefined ? { is_all_day: patch.is_all_day } : {}),
    ...(patch.start_time_zone !== undefined
      ? {
          start_time_zone: patch.start_time_zone,
          end_time_zone: patch.start_time_zone,
        }
      : {}),
    ...(patch.calendar_id !== undefined
      ? { calendar_id: patch.calendar_id }
      : {}),
  };
  serverRows.set(id, next);
  return next;
}

/** The create POST stays unresolved until the test answers it. */
function postCreate(body: NewCalendarEventInput): Promise<Response> {
  return new Promise<Response>((resolve) => {
    pendingPosts.push({
      body,
      respondWith: (event) => {
        serverRows.set(event.id, event);
        resolve(json({ event, source: 'google' }));
      },
    });
  });
}

function respond(
  url: string,
  init?: RequestInit,
): Response | Promise<Response> {
  const path = url.split('?')[0];
  const method = (init?.method ?? 'GET').toUpperCase();
  const body =
    typeof init?.body === 'string'
      ? (JSON.parse(init.body) as unknown)
      : undefined;
  requests.push({ method, path, body });

  if (method === 'GET' && path === '/api/calendar/events') {
    return json({ events: seededEvents, source: 'window', sync: EMPTY_SYNC });
  }
  if (method === 'GET' && path === '/api/calendar/calendars') {
    return json({ calendars: CALENDARS });
  }
  if (method === 'POST' && path === '/api/calendar/events' && body) {
    return postCreate(body as NewCalendarEventInput);
  }
  if (method === 'PATCH') {
    const id = path.split('/').pop() ?? 'patched';
    return json({
      event: patchedEvent(id, (body ?? {}) as PatchCalendarEventInput),
      source: 'google',
    });
  }
  if (method === 'DELETE') return json({ success: true });
  return json({ error: 'unexpected' }, 404);
}

function deleteRequests(): Request[] {
  return requests.filter((r) => r.method === 'DELETE');
}

function patchRequests(): Request[] {
  return requests.filter((r) => r.method === 'PATCH');
}

/** Let the query/mutation pipeline drain (a few macrotasks). */
async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

interface HarnessProps {
  api: { current: Session | null };
  days: Date[];
  scrollerRef: RefObject<HTMLElement | null>;
}

function Harness({ api, days, scrollerRef }: HarnessProps): null {
  const session = useCalendarSession({
    timeMin: days[0].toISOString(),
    timeMax: days[days.length - 1].toISOString(),
    days,
    dayCount: days.length,
    scrollerHeight: 0,
    scrollerRef,
    lockStrip: () => {},
    unlockStrip: () => {},
    scrollStripBy: () => {},
  });
  api.current = session;
  return null;
}

const DAY = new Date(2026, 9, 18);

/** A timed draft range on the test day. */
function hourRange(
  startHour: number,
  endHour: number,
): {
  start: Date;
  end: Date;
} {
  return {
    start: new Date(2026, 9, 18, startHour, 0, 0, 0),
    end: new Date(2026, 9, 18, endHour, 0, 0, 0),
  };
}

/** Live session ref: every render writes the newest API into it. */
type SessionRef = { current: Session | null };

async function mountSession(): Promise<SessionRef> {
  const api: SessionRef = { current: null };
  const scrollerRef: RefObject<HTMLElement | null> = { current: null };

  await act(async () => {
    render(
      createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          CalendarEventsProvider,
          null,
          createElement(Harness, { api, days: [DAY], scrollerRef }),
        ),
      ),
    );
  });

  await waitFor(() => {
    assert.ok(api.current);
    assert.equal(api.current.isLoading, false);
    assert.equal(api.current.calendars.length, CALENDARS.length);
  });

  return api;
}

/** Chips painted in the grid, in day order. */
function paintedIds(session: Session): string[] {
  return Array.from(session.eventsByDay.values())
    .flat()
    .map((positioned) => positioned.event.id);
}

/**
 * A desktop empty-cell click: pointerdown on the day column (the session's
 * create gesture) then a pointerup with no movement, which runs the drag hook's
 * `onEmptyClick` — the repro's discard path.
 */
async function clickEmptyCell(session: Session): Promise<void> {
  const col = document.createElement('div');
  col.getBoundingClientRect = () =>
    ({
      top: 0,
      left: 0,
      right: 100,
      bottom: 1000,
      width: 100,
      height: 1000,
      x: 0,
      y: 0,
      toJSON() {
        return {};
      },
    }) as DOMRect;

  const down = {
    button: 0,
    clientX: 10,
    clientY: 480,
    pointerId: 1,
    pointerType: 'mouse',
    metaKey: false,
    ctrlKey: false,
    timeStamp: 0,
    target: col,
    currentTarget: col,
    preventDefault() {},
    stopPropagation() {},
  } as unknown as ReactPointerEvent<HTMLElement>;

  await act(async () => {
    session.onColumnPointerDown(down, DAY);
  });
  await act(async () => {
    window.dispatchEvent(
      new window.PointerEvent('pointerup', {
        pointerId: 1,
        pointerType: 'mouse',
        clientX: 10,
        clientY: 480,
        bubbles: true,
      }),
    );
  });
}

// ── Chip drag helper: real window pointer events, real hit-testing stubs ──
//
// The drag hook lives on window listeners, so a session started on a chip
// survives that chip's React key flipping tmp → server id. jsdom does no
// hit-testing and no layout, so the day column / chip geometry is stubbed and
// `elementFromPoint` is pinned to the column while mounted.

const COL_W = 100;

function dayColumnRect(hourH: number): DOMRect {
  return {
    top: 0,
    left: 0,
    right: COL_W,
    bottom: hourH * 24,
    width: COL_W,
    height: hourH * 24,
    x: 0,
    y: 0,
    toJSON() {
      return {};
    },
  } as DOMRect;
}

function chipRect(startMin: number, endMin: number, hourH: number): DOMRect {
  const top = (startMin / 60) * hourH;
  const height = Math.max(1, ((endMin - startMin) / 60) * hourH);
  return {
    top,
    left: 0,
    right: COL_W,
    bottom: top + height,
    width: COL_W,
    height,
    x: 0,
    y: top,
    toJSON() {
      return {};
    },
  } as DOMRect;
}

function minutesOf(iso: string): number {
  const d = new Date(iso);
  return d.getHours() * 60 + d.getMinutes();
}

interface ChipDom {
  chipEl: HTMLElement;
  remove: () => void;
}

/**
 * A day column + chip element the drag hook can measure. `elementFromPoint`
 * points at the column so timed hit-testing resolves, exactly like a browser
 * over that day.
 */
function mountChipDom(
  chipId: string,
  startMin: number,
  endMin: number,
  hourH: number,
): ChipDom {
  const col = document.createElement('div');
  col.setAttribute('data-day-col', DAY.toISOString());
  col.getBoundingClientRect = () => dayColumnRect(hourH);

  const chipEl = document.createElement('div');
  chipEl.className = 'event-chip';
  chipEl.dataset.eventId = chipId;
  chipEl.dataset.startMin = String(startMin);
  chipEl.dataset.endMin = String(endMin);
  chipEl.getBoundingClientRect = () => chipRect(startMin, endMin, hourH);
  col.append(chipEl);
  document.body.append(col);

  const prevHit = document.elementFromPoint;
  document.elementFromPoint = () => col;

  return {
    chipEl,
    remove: () => {
      col.remove();
      if (typeof prevHit === 'function') document.elementFromPoint = prevHit;
      else Reflect.deleteProperty(document, 'elementFromPoint');
    },
  };
}

function chipPointerDown(
  session: Session,
  event: CalendarEvent,
  chipEl: HTMLElement,
  clientY: number,
): void {
  const e = {
    button: 0,
    clientX: 10,
    clientY,
    pointerId: 1,
    pointerType: 'mouse',
    metaKey: false,
    ctrlKey: false,
    timeStamp: 0,
    target: chipEl,
    currentTarget: chipEl,
    preventDefault() {},
    stopPropagation() {},
  } as unknown as ReactPointerEvent<HTMLElement>;
  session.onChipPointerDown(e, event, chipEl);
}

async function movePointer(clientY: number): Promise<void> {
  await act(async () => {
    window.dispatchEvent(
      new window.PointerEvent('pointermove', {
        pointerId: 1,
        pointerType: 'mouse',
        clientX: 10,
        clientY,
        bubbles: true,
        cancelable: true,
      }),
    );
  });
}

async function dropPointer(clientY: number): Promise<void> {
  await act(async () => {
    window.dispatchEvent(
      new window.PointerEvent('pointerup', {
        pointerId: 1,
        pointerType: 'mouse',
        clientX: 10,
        clientY,
        bubbles: true,
      }),
    );
  });
}

/** Client Y for a minute-of-day on the stubbed column. */
function yForMinutes(minutes: number, hourH: number): number {
  return (minutes / 60) * hourH;
}

/** The painted event for `id`, wherever the packing put it. */
function paintedEvent(session: Session, id: string): CalendarEvent | undefined {
  for (const day of session.eventsByDay.values()) {
    const hit = day.find((positioned) => positioned.event.id === id);
    if (hit) return hit.event;
  }
  return undefined;
}

before(async () => {
  (globalThis as { fetch: typeof fetch }).fetch = ((
    url: string,
    init?: RequestInit,
  ) => Promise.resolve(respond(url, init))) as unknown as typeof fetch;

  const rtl = await import('@testing-library/react');
  render = rtl.render;
  act = rtl.act;
  waitFor = rtl.waitFor;
  cleanup = rtl.cleanup;

  const rq = await import('@tanstack/react-query');
  QueryClientProvider = rq.QueryClientProvider;

  const qc = await import('@/lib/queryClient');
  queryClient = qc.queryClient;

  const provider = await import('../events-context');
  CalendarEventsProvider = provider.CalendarEventsProvider as ComponentType<{
    children?: ReactNode;
  }>;

  const queries = await import('@/app/queries/calendar');
  findCachedCalendarEvent = queries.findCachedCalendarEvent;

  useCalendarSession = (await import('./useCalendarSession'))
    .useCalendarSession;
});

/**
 * Drop every timer the client owns. Every create/delete is a mutation, and
 * `MutationCache.clear()` forgets them without destroying them, so their
 * five-minute gc timers would keep the node test runner alive.
 */
async function resetQueryClient(): Promise<void> {
  // Unmount queues a microtask (a real unmount commits a pending delete); let
  // it settle before dropping the client.
  await settle();
  for (const mutation of queryClient.getMutationCache().getAll()) {
    mutation.destroy();
  }
  queryClient.clear();
}

afterEach(async () => {
  cleanup();
  await resetQueryClient();
  requests = [];
  pendingPosts = [];
  seededEvents = [];
  serverRows = new Map();
});

after(async () => {
  cleanup();
  await resetQueryClient();
});

// ── The repro: title blur, then a click on the grid ──────────────────────

test('a draft whose create was sent survives an empty-cell click', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId;
  assert.ok(tempId, 'a temp chip is painted and selected');
  assert.deepEqual(paintedIds(session.current!), [tempId]);

  // Title blur: the POST is sent now.
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  assert.equal(pendingPosts[0].body.summary, 'Dentist');

  // Click the empty grid while the POST is still in flight.
  await clickEmptyCell(session.current!);
  assert.deepEqual(
    paintedIds(session.current!),
    [tempId],
    'the chip stays on the grid',
  );

  // The create lands: it must be kept, never ghost-deleted.
  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  assert.deepEqual(deleteRequests(), [], 'no DELETE for a sent create');
  assert.ok(
    findCachedCalendarEvent(queryClient, 'srv-1'),
    'the created row is in the cache',
  );
  assert.deepEqual(
    paintedIds(session.current!),
    ['srv-1'],
    'the chip carries its id',
  );
});

// ── Edits while the create flies are flushed, never a second POST ────────

test('a second title edit flushes as a patch after create returns', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Edit again while the POST is in flight: overlay only, no second create.
  await act(async () => {
    session.current!.handleSaveTitle('Dentist 2');
  });
  await settle();
  assert.equal(pendingPosts.length, 1, 'no second POST for the same draft');
  assert.equal(
    session.current!.selectedEvent?.title,
    'Dentist 2',
    'the edit paints immediately',
  );

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  assert.deepEqual(deleteRequests(), []);
  const patches = patchRequests();
  assert.equal(patches.length, 1, 'the edit is flushed under the server id');
  assert.deepEqual(patches[0].body, { summary: 'Dentist 2' });
  assert.equal(
    session.current!.selectedEvent?.id,
    'srv-1',
    'the selection follows the row',
  );
});

// ── Escape / another chip: same discard paths, same rule ─────────────────

const OTHER_EVENT: CalendarEvent = {
  id: 'evt-other',
  calendar_id: 'cal-1',
  google_event_id: 'google-evt-other',
  title: 'Lunch',
  description: '',
  start_time: new Date(2026, 9, 18, 13, 0, 0, 0).toISOString(),
  end_time: new Date(2026, 9, 18, 14, 0, 0, 0).toISOString(),
  last_synced_at: '2026-10-18T00:00:00.000Z',
};

test('a draft whose create was sent survives Escape and another chip', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  await act(async () => {
    session.current!.closeInspector();
  });
  assert.deepEqual(
    paintedIds(session.current!),
    [tempId, OTHER_EVENT.id],
    'Escape keeps the chip',
  );

  // Selecting the other chip is the discard path that used to clear it.
  await act(async () => {
    session.current!.selectEvent(OTHER_EVENT.id);
  });
  assert.deepEqual(
    paintedIds(session.current!),
    [tempId, OTHER_EVENT.id],
    'another chip keeps it',
  );

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  assert.deepEqual(deleteRequests(), []);
  assert.ok(findCachedCalendarEvent(queryClient, 'srv-1'));
  assert.deepEqual(
    paintedIds(session.current!),
    ['srv-1', OTHER_EVENT.id],
    'the created row joins the seeded chip',
  );
});

// ── A new draft replaces the open one, never an in-flight create ─────────

test('starting a second draft keeps the previous in-flight create', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const firstTempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('First');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Drag out a second draft while the first POST is still in flight.
  await act(async () => {
    session.current!.beginDraft(hourRange(13, 14));
  });
  const secondTempId = session.current!.selectedEventId!;
  assert.notEqual(secondTempId, firstTempId);
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    [firstTempId, secondTempId].sort(),
    'both chips are painted',
  );

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  assert.deepEqual(
    deleteRequests(),
    [],
    'the first create is not ghost-deleted',
  );
  assert.ok(findCachedCalendarEvent(queryClient, 'srv-1'));
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-1', secondTempId].sort(),
    'the first chip carries its server id',
  );
});

// ── Calendar picks are per create, not one shared flag ───────────────────

test("a newer draft's calendar pick does not move an older create", async () => {
  const session = await mountSession();

  // Draft A keeps the default calendar: the server may reroute it by title.
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  await act(async () => {
    session.current!.handleSaveTitle('Standup');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  assert.equal(
    pendingPosts[0].body.calendar_id_explicit,
    undefined,
    'an untouched draft lets the server route by category',
  );

  // Draft B picks another calendar while A's POST is still in flight.
  await act(async () => {
    session.current!.beginDraft(hourRange(13, 14));
  });
  await act(async () => {
    session.current!.handleSaveCalendar('cal-2');
  });

  // A lands in cal-2 by category routing, not because the user picked it.
  pendingPosts[0].respondWith(
    createdEvent(pendingPosts[0].body, 'srv-1', 'cal-2'),
  );
  await settle();

  assert.deepEqual(
    patchRequests(),
    [],
    "B's pick must not move A away from the server's calendar",
  );
  assert.ok(findCachedCalendarEvent(queryClient, 'srv-1'));
});

// ── Explicit delete still works on a sent create ─────────────────────────

test('deleting a sent create parks the row behind the undo toast', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Explicit delete: hide the chip; the DELETE waits out the undo window.
  await act(async () => {
    session.current!.handleDeleteEvent();
  });
  assert.notEqual(
    session.current!.pendingDeleteTitle,
    null,
    'the undo toast is up',
  );

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  assert.deepEqual(
    deleteRequests(),
    [],
    'the parked row is not deleted inside the undo window',
  );
  assert.ok(
    findCachedCalendarEvent(queryClient, 'srv-1'),
    'the parked row is cached behind the toast',
  );
});

// ── A duplicate is a real event while its create flies ───────────────────
//
// Cmd+D paints a temp copy and POSTs it. The user can drag / rename it before
// the POST returns: those edits are local to the temp id, and every one must
// survive the tmp → server remap and land as a PATCH on the server row.

/** Select the seeded row, then Cmd+D: paints a temp copy and starts its POST. */
async function duplicateChip(ref: SessionRef): Promise<string> {
  await act(async () => {
    ref.current!.selectEvent(OTHER_EVENT.id);
  });
  await act(async () => {
    assert.equal(ref.current!.duplicateSelected(), true);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  const tempId = ref.current!.selectedEventId;
  assert.ok(tempId, 'the copy is selected');
  assert.ok(tempId!.startsWith('tmp_'), 'the copy paints under a temp id');
  return tempId!;
}

test('a duplicate moved before create returns is flushed onto the server row', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  // Grab the temp chip in the middle and move it two hours down.
  const tempEvent = session.current!.selectedEvent!;
  assert.equal(tempEvent.id, tempId);
  const hourH = session.current!.hourH;
  const startMin = minutesOf(tempEvent.start_time);
  const endMin = minutesOf(tempEvent.end_time);
  // Mid-chip: the top / bottom bands are resize handles, not a move grab.
  const grabY = yForMinutes((startMin + endMin) / 2, hourH);
  const dom = mountChipDom(tempId, startMin, endMin, hourH);
  try {
    await act(async () => {
      chipPointerDown(session.current!, tempEvent, dom.chipEl, grabY);
    });
    // The grab offset is 30 min into the chip; +120 minutes of travel lands
    // the start on 15:00 (two hours after the source).
    await movePointer(yForMinutes((startMin + endMin) / 2 + 120, hourH));
    assert.equal(session.current!.isDragging, true);

    // The create lands while the pointer is still down: the drag must survive
    // the chip's key flip and the ghost must keep painting the row.
    pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
    await settle();
    assert.equal(
      session.current!.isDragging,
      true,
      'the drag session survives',
    );
    assert.equal(
      session.current!.previewTitle,
      'Lunch',
      'the ghost keeps its title',
    );
    assert.equal(session.current!.draggingEventId, 'srv-1');
    assert.deepEqual(
      patchRequests(),
      [],
      'the move is not sent before the drop',
    );

    // The React key flips tmp → server, so the captured chip element is gone.
    // The gesture lives on window listeners, so the drag must survive it.
    dom.chipEl.remove();

    // Drop: the session still holds the temp id, so this is the repro. The
    // alias must aim the PATCH at the server row.
    await dropPointer(yForMinutes((startMin + endMin) / 2 + 120, hourH));
    await settle();
  } finally {
    dom.remove();
  }

  const moved = hourRange(15, 16);
  const patches = patchRequests();
  assert.equal(patches.length, 1);
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, {
    start: moved.start.toISOString(),
    end: moved.end.toISOString(),
  });
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-1', OTHER_EVENT.id].sort(),
  );
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.start_time,
    moved.start.toISOString(),
    'the chip stays where it was dropped',
  );
  assert.deepEqual(deleteRequests(), []);
});

test('a duplicate dropped before create returns is flushed onto the server row', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  // The repro: drag the new chip and let go while its POST is still in
  // flight. The drop paints the temp overlay; the POST returns afterwards.
  const tempEvent = session.current!.selectedEvent!;
  const hourH = session.current!.hourH;
  const startMin = minutesOf(tempEvent.start_time);
  const endMin = minutesOf(tempEvent.end_time);
  const grabY = yForMinutes((startMin + endMin) / 2, hourH);
  const dom = mountChipDom(tempId, startMin, endMin, hourH);
  try {
    await act(async () => {
      chipPointerDown(session.current!, tempEvent, dom.chipEl, grabY);
    });
    const dropY = yForMinutes((startMin + endMin) / 2 + 120, hourH);
    await movePointer(dropY);
    await dropPointer(dropY);
    await settle();
  } finally {
    dom.remove();
  }

  const moved = hourRange(15, 16);
  assert.deepEqual(patchRequests(), [], 'no PATCH without a server id');
  assert.equal(
    paintedEvent(session.current!, tempId)?.start_time,
    moved.start.toISOString(),
    'the drop paints where the user let go',
  );

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  const patches = patchRequests();
  assert.equal(patches.length, 1, 'the drop is flushed, never dropped');
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, {
    start: moved.start.toISOString(),
    end: moved.end.toISOString(),
  });
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.start_time,
    moved.start.toISOString(),
    'the chip does not snap back to the posted slot',
  );
  assert.deepEqual(deleteRequests(), []);
});

test('a stale temp id from a drag still patches the server row', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);
  const staleEvent = session.current!.selectedEvent!;

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-1', OTHER_EVENT.id].sort(),
    'the copy carries its server id',
  );

  // A stale selection resolves through the alias instead of closing.
  await act(async () => {
    session.current!.selectEvent(tempId);
  });
  assert.equal(session.current!.selectedEvent?.id, 'srv-1');
  assert.equal(session.current!.selectedEventId, 'srv-1');

  // A chip / drag session holding the pre-create event object moves again.
  const hourH = session.current!.hourH;
  const startMin = minutesOf(staleEvent.start_time);
  const endMin = minutesOf(staleEvent.end_time);
  const grabY = yForMinutes((startMin + endMin) / 2, hourH);
  const dom = mountChipDom(tempId, startMin, endMin, hourH);
  try {
    await act(async () => {
      chipPointerDown(session.current!, staleEvent, dom.chipEl, grabY);
    });
    // +4h of travel lands the start on 17:00.
    await movePointer(yForMinutes((startMin + endMin) / 2 + 4 * 60, hourH));
    await dropPointer(yForMinutes((startMin + endMin) / 2 + 4 * 60, hourH));
    await settle();
  } finally {
    dom.remove();
  }

  const moved = hourRange(17, 18);
  const patches = patchRequests();
  assert.equal(patches.length, 1);
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, {
    start: moved.start.toISOString(),
    end: moved.end.toISOString(),
  });
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.start_time,
    moved.start.toISOString(),
  );
});

test('a paste renamed before create returns is flushed, not lost', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  // Rename the copy while its POST is in flight: overlay only.
  await act(async () => {
    session.current!.handleSaveTitle('Lunch 2');
  });
  await settle();
  assert.deepEqual(patchRequests(), [], 'no PATCH without a server id');
  assert.equal(paintedEvent(session.current!, tempId)?.title, 'Lunch 2');

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  const patches = patchRequests();
  assert.equal(patches.length, 1, 'the rename is flushed under the server id');
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, { summary: 'Lunch 2' });
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.title,
    'Lunch 2',
    'the chip keeps the rename',
  );
});

// ── The draft path keeps flushing exactly as before ──────────────────────

test('a draft moved while its create flies is flushed onto the server id', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  const moved = hourRange(11, 12);
  await act(async () => {
    session.current!.handleSaveTimes(
      moved.start.toISOString(),
      moved.end.toISOString(),
    );
  });
  assert.deepEqual(patchRequests(), [], 'the temp id defers its PATCH');

  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  const patches = patchRequests();
  assert.equal(patches.length, 1);
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, {
    start: moved.start.toISOString(),
    end: moved.end.toISOString(),
  });
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.start_time,
    moved.start.toISOString(),
  );
  assert.ok(tempId.startsWith('tmp_'));
});

test('a calendar pick made while the create flies moves the row', async () => {
  const session = await mountSession();

  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  await act(async () => {
    session.current!.handleSaveTitle('Standup');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  assert.equal(pendingPosts[0].body.calendar_id_explicit, undefined);

  // Pick another calendar while the POST is in flight: the server already
  // routed the create, so the pick must come back as a MOVE.
  await act(async () => {
    session.current!.handleSaveCalendar('cal-2');
  });
  assert.equal(session.current!.selectedEvent?.calendar_id, 'cal-2');

  pendingPosts[0].respondWith(
    createdEvent(pendingPosts[0].body, 'srv-1', 'cal-1'),
  );
  await settle();

  const patches = patchRequests();
  assert.equal(patches.length, 1);
  assert.equal(patches[0].path, '/api/calendar/events/srv-1');
  assert.deepEqual(patches[0].body, { calendar_id: 'cal-2' });
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.calendar_id,
    'cal-2',
    'the chip does not snap back to the routed calendar',
  );
});

// ── Cutting a create still in flight is settled on paste ─────────────────

test('a cut taken on an in-flight create deletes the source on paste', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  let item: CalendarClipboardItem | null = null;
  await act(async () => {
    item = session.current!.cutForClipboard();
  });
  // The assignment happens inside the act callback, which control-flow
  // analysis does not follow; re-assert the declared union.
  const cut = item as CalendarClipboardItem | null;
  assert.ok(cut);
  assert.equal(
    cut.cutSourceId,
    tempId,
    'the cut names the temp source until it resolves',
  );
  assert.ok(
    paintedIds(session.current!).includes(tempId),
    'the source stays on the grid',
  );

  // The source create returns before the paste: the alias can name its row.
  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();

  let paste: Promise<boolean> | null = null;
  await act(async () => {
    paste = session.current!.pasteFromClipboard(cut);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-2'));
  let removed = false;
  await act(async () => {
    removed = await paste!;
  });
  await settle();

  assert.equal(removed, true, 'the paste reports the source it consumed');
  assert.deepEqual(
    deleteRequests().map((r) => r.path),
    ['/api/calendar/events/srv-1'],
  );
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-2', OTHER_EVENT.id].sort(),
  );
});

test('a cut whose source create has not returned is left for a later paste', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  let item: CalendarClipboardItem | null = null;
  await act(async () => {
    item = session.current!.cutForClipboard();
  });
  // Same act-callback narrowing caveat as above.
  const cut = item as CalendarClipboardItem | null;
  assert.ok(cut);
  assert.equal(cut.cutSourceId, tempId);

  // Paste while the source's own create is still in flight.
  let paste: Promise<boolean> | null = null;
  await act(async () => {
    paste = session.current!.pasteFromClipboard(cut);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-2'));
  let removed = true;
  await act(async () => {
    removed = await paste!;
  });
  await settle();

  assert.equal(removed, false, 'there is no source row to delete yet');
  assert.deepEqual(deleteRequests(), []);
  assert.ok(
    paintedIds(session.current!).includes(tempId),
    'the source create is untouched',
  );

  // The source lands later: it stays, because nothing claimed its removal.
  pendingPosts[0].respondWith(createdEvent(pendingPosts[0].body, 'srv-1'));
  await settle();
  assert.deepEqual(deleteRequests(), []);
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-1', 'srv-2', OTHER_EVENT.id].sort(),
  );
});

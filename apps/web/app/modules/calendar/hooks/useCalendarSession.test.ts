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
  return {
    id,
    calendar_id: calendarId,
    google_event_id: `google-${id}`,
    title: body.summary,
    description: body.description ?? '',
    start_time: body.start,
    end_time: body.end,
    last_synced_at: '2026-10-18T00:00:00.000Z',
  };
}

/** The create POST stays unresolved until the test answers it. */
function postCreate(body: NewCalendarEventInput): Promise<Response> {
  return new Promise<Response>((resolve) => {
    pendingPosts.push({
      body,
      respondWith: (event) => resolve(json({ event, source: 'google' })),
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
    const patch = (body ?? {}) as PatchCalendarEventInput;
    return json({
      event: {
        id,
        calendar_id: patch.calendar_id ?? 'cal-1',
        google_event_id: `google-${id}`,
        title: patch.summary ?? '',
        description: patch.description ?? '',
        start_time: patch.start ?? '2026-10-18T09:00:00.000Z',
        end_time: patch.end ?? '2026-10-18T10:00:00.000Z',
        last_synced_at: '2026-10-18T00:00:00.000Z',
      },
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

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
  useState,
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
type QueueModule = typeof import('../events-context');
type Queue = ReturnType<QueueModule['useCalendarEventsQueue']>;

let useCalendarSession: SessionModule['useCalendarSession'];
let CalendarEventsProvider: ComponentType<{ children?: ReactNode }>;
let useCalendarEventsQueue: QueueModule['useCalendarEventsQueue'];
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
  /** Answer the POST with an API error response, as a failed create does. */
  fail: (message?: string) => void;
}

interface PendingPatch {
  id: string;
  body: PatchCalendarEventInput;
  /** Answer with the merged server row (or an explicit one). */
  respondWith: (event?: CalendarEvent) => void;
  fail: (message?: string) => void;
}

interface PendingGet {
  /** Resolve this events GET with `events`. */
  respondWith: (events: CalendarEvent[]) => void;
}

let requests: Request[] = [];
let pendingPosts: PendingPost[] = [];
/** Keep PATCH responses unresolved so a test can order them against GETs. */
let holdPatches = false;
let pendingPatches: PendingPatch[] = [];
/** Keep events GET responses unresolved for the stale-read race. */
let holdGets = false;
let pendingGets: PendingGet[] = [];
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
      fail: (message = 'create failed') => {
        resolve(json({ error: message }, 500));
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
    if (holdGets) {
      return new Promise<Response>((resolve) => {
        pendingGets.push({
          respondWith: (events) => {
            resolve(json({ events, source: 'window', sync: EMPTY_SYNC }));
          },
        });
      });
    }
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
    const input = (body ?? {}) as PatchCalendarEventInput;
    if (holdPatches) {
      return new Promise<Response>((resolve) => {
        pendingPatches.push({
          id,
          body: input,
          respondWith: (event) => {
            resolve(
              json({
                event: event ?? patchedEvent(id, input),
                source: 'google',
              }),
            );
          },
          fail: (message = 'patch failed') => {
            resolve(json({ error: message }, 500));
          },
        });
      });
    }
    return json({ event: patchedEvent(id, input), source: 'google' });
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

/**
 * Delete the selection with `window.setTimeout` stubbed, so the deferred
 * delete's undo timer is captured instead of waiting DELETE_UNDO_MS. Returns
 * the callback that commits the delete.
 */
async function armDeleteCommit(session: Session): Promise<() => void> {
  const realSetTimeout = window.setTimeout;
  const captured: { fire: (() => void) | null } = { fire: null };
  window.setTimeout = ((cb: TimerHandler) => {
    if (typeof cb === 'function') captured.fire = cb as () => void;
    return 0;
  }) as typeof window.setTimeout;
  try {
    await act(async () => {
      session.handleDeleteEvent();
    });
  } finally {
    window.setTimeout = realSetTimeout;
  }
  await settle();
  assert.ok(captured.fire, 'the undo timer was armed');
  return captured.fire;
}

interface HarnessProps {
  api: SessionRef;
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

/** Reads the provider's queue without being part of the page. */
function QueueProbe({ target }: { target: { current: Queue | null } }): null {
  target.current = useCalendarEventsQueue();
  return null;
}

/**
 * Mounts/unmounts only the page, so the app-wide provider (and everything it
 * owns: local change layer, aliases, unsaved creates) survives like it does
 * when the calendar route is left and entered again.
 */
interface PageControl {
  setMounted: ((mounted: boolean) => void) | null;
}

function PageHost({
  api,
  days,
  scrollerRef,
  control,
}: HarnessProps & { control: PageControl }): ReactNode {
  const [mounted, setMounted] = useState(true);
  control.setMounted = setMounted;
  if (!mounted) return null;
  return createElement(Harness, { api, days, scrollerRef });
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

/**
 * Live session ref: every render writes the newest API into it. `queue` is the
 * provider's queue (the layer's public seam), and the page can be unmounted
 * and remounted while the provider stays up.
 */
interface SessionRef {
  current: Session | null;
  queue: { current: Queue | null };
  unmountPage: () => Promise<void>;
  remountPage: () => Promise<void>;
}

async function mountSession(): Promise<SessionRef> {
  const api = {
    current: null,
    queue: { current: null },
    unmountPage: async () => {},
    remountPage: async () => {},
  } as SessionRef;
  const scrollerRef: RefObject<HTMLElement | null> = { current: null };
  const control: PageControl = { setMounted: null };

  await act(async () => {
    render(
      createElement(
        QueryClientProvider,
        { client: queryClient },
        createElement(
          CalendarEventsProvider,
          null,
          createElement(QueueProbe, { target: api.queue }),
          createElement(PageHost, {
            api,
            days: [DAY],
            scrollerRef,
            control,
          }),
        ),
      ),
    );
  });

  await waitFor(() => {
    assert.ok(api.current);
    assert.equal(api.current.isLoading, false);
    assert.equal(api.current.calendars.length, CALENDARS.length);
    assert.ok(api.queue.current);
  });

  api.unmountPage = async () => {
    await act(async () => {
      control.setMounted?.(false);
    });
    api.current = null;
    await settle();
  };

  api.remountPage = async () => {
    await act(async () => {
      control.setMounted?.(true);
    });
    await waitFor(() => {
      assert.ok(api.current);
      assert.equal(api.current.isLoading, false);
    });
  };

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
  useCalendarEventsQueue = provider.useCalendarEventsQueue;

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
  holdPatches = false;
  pendingPatches = [];
  holdGets = false;
  pendingGets = [];
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

// ── A failed create is an unsaved event, never a loss ────────────────────
//
// The POST is the only thing that failed: no server row exists, so the temp
// chip must keep every field it holds (including edits made while the POST was
// in flight), stay selected, and survive the discard paths that belong to
// drafts. Its next save — or the banner / inspector Retry — re-POSTs its
// current local state through the same create path.

test('a failed create keeps the chip, its fields and the selection', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Edits made while the POST flies are local to the temp chip.
  await act(async () => {
    session.current!.handleSaveDescription('Bring the form');
  });
  const moved = hourRange(11, 12);
  await act(async () => {
    session.current!.handleSaveTimes(
      moved.start.toISOString(),
      moved.end.toISOString(),
    );
  });
  assert.deepEqual(patchRequests(), [], 'no PATCH without a server id');

  pendingPosts[0].fail();
  await settle();

  assert.deepEqual(deleteRequests(), [], 'a failed create sends no DELETE');
  assert.deepEqual(patchRequests(), [], 'nothing PATCHes a temp id');
  assert.deepEqual(
    paintedIds(session.current!),
    [tempId],
    'the chip stays on the grid',
  );
  assert.equal(
    session.current!.selectedEventId,
    tempId,
    'the inspector stays on it',
  );
  assert.ok(
    session.current!.unsavedEventIds.has(tempId),
    'the chip is marked unsaved',
  );

  const painted = paintedEvent(session.current!, tempId);
  assert.ok(painted);
  assert.equal(painted.title, 'Dentist');
  assert.equal(painted.description, 'Bring the form');
  assert.equal(painted.start_time, moved.start.toISOString());
  assert.equal(painted.end_time, moved.end.toISOString());

  assert.match(
    session.current!.writeError ?? '',
    /Couldn't create event/,
    'the failure is reported',
  );
  assert.match(session.current!.writeError ?? '', /create failed/);
  assert.equal(session.current!.canRetryWrites, true);
});

test('a failed create survives Escape, an empty click, another chip and a new draft', async () => {
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
  pendingPosts[0].fail();
  await settle();

  // Escape (inspector close) only discards an open draft.
  await act(async () => {
    session.current!.closeInspector();
  });
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    [tempId, OTHER_EVENT.id].sort(),
    'Escape keeps it',
  );
  assert.equal(session.current!.selectedEventId, null);

  // Desktop empty-cell click.
  await clickEmptyCell(session.current!);
  assert.ok(
    paintedIds(session.current!).includes(tempId),
    'an empty click keeps it',
  );

  // Selecting another chip.
  await act(async () => {
    session.current!.selectEvent(OTHER_EVENT.id);
  });
  assert.ok(
    paintedIds(session.current!).includes(tempId),
    'another chip keeps it',
  );

  // A new draft replaces the previous *draft*, never an unsaved event.
  await act(async () => {
    session.current!.beginDraft(hourRange(13, 14));
  });
  const draftId = session.current!.selectedEventId!;
  assert.notEqual(draftId, tempId);
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    [tempId, OTHER_EVENT.id, draftId].sort(),
    'both chips are painted',
  );

  // Discarding the new draft leaves the unsaved event untouched.
  await clickEmptyCell(session.current!);
  assert.deepEqual(
    paintedIds(session.current!).sort(),
    [tempId, OTHER_EVENT.id].sort(),
  );
  assert.ok(session.current!.unsavedEventIds.has(tempId));
  assert.equal(paintedEvent(session.current!, tempId)?.title, 'Dentist');
  assert.deepEqual(patchRequests(), []);
});

test('saving a failed draft retries with its current fields and remaps', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Description typed while the POST was in flight: it survives the failure
  // and the retry carries it.
  await act(async () => {
    session.current!.handleSaveDescription('Bring the form');
  });
  pendingPosts[0].fail();
  await settle();
  assert.equal(
    paintedEvent(session.current!, tempId)?.description,
    'Bring the form',
  );

  // The next title save is the retry trigger.
  await act(async () => {
    session.current!.handleSaveTitle('Dentist 2');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));

  const posted = pendingPosts[1].body;
  assert.deepEqual(posted, {
    calendar_id: 'cal-1',
    summary: 'Dentist 2',
    description: 'Bring the form',
    start: paintedEvent(session.current!, tempId)!.start_time,
    end: paintedEvent(session.current!, tempId)!.end_time,
  });

  pendingPosts[1].respondWith(createdEvent(posted, 'srv-1'));
  await settle();

  assert.deepEqual(
    patchRequests(),
    [],
    'the retry body already carried every field',
  );
  assert.deepEqual(paintedIds(session.current!), ['srv-1']);
  assert.equal(
    session.current!.selectedEventId,
    'srv-1',
    'the selection follows the row',
  );
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.title,
    'Dentist 2',
    'the chip keeps the retried fields',
  );
  assert.equal(session.current!.unsavedEventIds.size, 0, 'no marker is left');
  assert.equal(session.current!.writeError, null, 'the failure banner clears');
  assert.deepEqual(deleteRequests(), []);
});

test('a failed paste keeps its copy and an explicit retry lands it', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();
  const tempId = await duplicateChip(session);

  pendingPosts[0].fail('duplicate failed');
  await settle();

  assert.ok(
    paintedEvent(session.current!, tempId),
    'the copy stays on the grid',
  );
  assert.equal(session.current!.selectedEvent?.id, tempId);
  assert.ok(session.current!.unsavedEventIds.has(tempId));
  assert.equal(session.current!.canRetryWrites, true);
  assert.deepEqual(patchRequests(), []);

  // The inspector's Retry re-POSTs the copy as it stands now.
  let retry: Promise<boolean> | null = null;
  await act(async () => {
    retry = session.current!.retryUnsavedCreate(tempId);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  assert.deepEqual(
    pendingPosts[1].body,
    pendingPosts[0].body,
    'an untouched copy retries the same body, explicit calendar included',
  );

  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-1'));
  let retried = false;
  await act(async () => {
    retried = await retry!;
  });
  await settle();

  assert.equal(retried, true, 'the retry reports that it landed');

  assert.deepEqual(
    paintedIds(session.current!).sort(),
    ['srv-1', OTHER_EVENT.id].sort(),
  );
  assert.equal(session.current!.selectedEventId, 'srv-1');
  assert.equal(session.current!.unsavedEventIds.size, 0);
  assert.equal(session.current!.writeError, null);
  assert.deepEqual(deleteRequests(), []);
  assert.deepEqual(patchRequests(), []);
});

test('a retried paste leaves the cut source alone', async () => {
  seededEvents = [OTHER_EVENT];
  const session = await mountSession();

  await act(async () => {
    session.current!.selectEvent(OTHER_EVENT.id);
  });
  let item: CalendarClipboardItem | null = null;
  await act(async () => {
    item = session.current!.cutForClipboard();
  });
  const cut = item as CalendarClipboardItem | null;
  assert.ok(cut);
  assert.equal(cut.cutSourceId, OTHER_EVENT.id);

  // Paste, then fail its create: the source was never touched.
  let paste: Promise<boolean> | null = null;
  await act(async () => {
    paste = session.current!.pasteFromClipboard(cut);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  pendingPosts[0].fail();
  let removed = true;
  await act(async () => {
    removed = await paste!;
  });
  await settle();
  assert.equal(removed, false, 'a failed paste costs nothing');
  assert.deepEqual(deleteRequests(), []);
  assert.ok(paintedIds(session.current!).includes(OTHER_EVENT.id));

  const copyId = session.current!.selectedEventId!;
  assert.ok(copyId.startsWith('tmp_'));

  // The retry creates the copy but deliberately does not consume the cut:
  // the clipboard still holds that source, so deleting it here would be a
  // removal the page could not even reflect.
  await act(async () => {
    session.current!.retryUnsavedCreates();
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-2'));
  await settle();

  assert.deepEqual(deleteRequests(), [], 'the cut source stays');
  assert.ok(
    paintedIds(session.current!).includes(OTHER_EVENT.id),
    'the source chip is still painted',
  );
  assert.ok(paintedIds(session.current!).includes('srv-2'));
});

test('deleting a failed create commits locally and undo restores it', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  pendingPosts[0].fail();
  await settle();

  // Delete hides the chip behind the undo toast; nothing is sent.
  await act(async () => {
    session.current!.handleDeleteEvent();
  });
  assert.deepEqual(paintedIds(session.current!), [], 'the chip is hidden');
  assert.notEqual(session.current!.pendingDeleteTitle, null);
  assert.deepEqual(deleteRequests(), []);

  // Undo restores the chip as the same unsaved event.
  await act(async () => {
    await session.current!.undoDelete();
  });
  assert.deepEqual(paintedIds(session.current!), [tempId]);
  assert.equal(session.current!.selectedEventId, tempId);
  assert.ok(
    session.current!.unsavedEventIds.has(tempId),
    'it comes back retryable',
  );

  // Delete again and let the grace window commit: a local clear, no request.
  const commit = await armDeleteCommit(session.current!);
  await act(async () => {
    commit();
  });
  await settle();

  assert.deepEqual(paintedIds(session.current!), [], 'the local event is gone');
  assert.equal(session.current!.unsavedEventIds.size, 0);
  assert.equal(session.current!.pendingDeleteTitle, null);
  assert.deepEqual(deleteRequests(), [], 'no server row to DELETE');
  assert.deepEqual(patchRequests(), []);
  assert.equal(pendingPosts.length, 1, 'no retry was sent');
});

test('a create that fails behind an undo toast still comes back as unsaved', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Delete before the POST returns: the temp chip is hidden by suppressing
  // paint and the undo toast owns it.
  await act(async () => {
    session.current!.handleDeleteEvent();
  });
  assert.deepEqual(paintedIds(session.current!), []);

  // The POST fails while the toast is still up: nothing is deleted, and the
  // undo record is still valid (there was never a server row).
  pendingPosts[0].fail();
  await settle();
  assert.notEqual(session.current!.pendingDeleteTitle, null);
  assert.deepEqual(deleteRequests(), []);

  // Undo brings the chip back as an unsaved event, not as a draft.
  await act(async () => {
    await session.current!.undoDelete();
  });
  assert.deepEqual(paintedIds(session.current!), [tempId]);
  assert.equal(session.current!.selectedEventId, tempId);
  assert.ok(session.current!.unsavedEventIds.has(tempId));

  // A retry lands it like any other create.
  await act(async () => {
    session.current!.retryUnsavedCreates();
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-1'));
  await settle();

  assert.deepEqual(paintedIds(session.current!), ['srv-1']);
  assert.equal(session.current!.unsavedEventIds.size, 0);
  assert.deepEqual(deleteRequests(), []);
});

test('cutting a failed create drops it locally; the payload is a plain copy', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  assert.ok(session.current!.selectedEventId, 'the draft chip is selected');
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  pendingPosts[0].fail();
  await settle();

  let item: CalendarClipboardItem | null = null;
  await act(async () => {
    item = session.current!.cutForClipboard();
  });
  const cut = item as CalendarClipboardItem | null;
  assert.ok(cut);
  assert.equal(
    cut.cutSourceId,
    null,
    'a local-only event has no row to cut on paste',
  );
  assert.deepEqual(paintedIds(session.current!), [], 'the chip is gone');
  assert.equal(session.current!.selectedEventId, null);
  assert.equal(session.current!.unsavedEventIds.size, 0);
  assert.deepEqual(deleteRequests(), []);
  assert.deepEqual(patchRequests(), []);
});

// ── The local change layer: a server read may not undo local intent ──────
//
// A PATCH is acknowledged by its own response, but only a *server read* that
// started after that ack may retire the intent. A GET that started before the
// ack can resolve after it with the pre-write row (gap-8): it must neither
// retire the intent nor repaint the old value.

/** A seeded server row for the layer tests. */
function seededEvent(
  id: string,
  overrides: Partial<CalendarEvent> = {},
): CalendarEvent {
  return {
    id,
    calendar_id: 'cal-1',
    google_event_id: `google-${id}`,
    title: 'Event',
    description: '',
    start_time: new Date(2026, 9, 18, 9, 0, 0, 0).toISOString(),
    end_time: new Date(2026, 9, 18, 10, 0, 0, 0).toISOString(),
    last_synced_at: '2026-10-18T00:00:00.000Z',
    ...overrides,
  };
}

/** Start an events refetch and return its held response. */
async function startHeldGet(session: Session): Promise<PendingGet> {
  holdGets = true;
  await act(async () => {
    session.retry();
  });
  await waitFor(() => assert.equal(pendingGets.length, 1));
  return pendingGets[0];
}

test('a stale GET resolving after the PATCH ack cannot repaint the old value', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  // The PATCH is sent and held, so the GET below starts before its ack.
  holdPatches = true;
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await waitFor(() => assert.equal(pendingPatches.length, 1));
  const staleGet = await startHeldGet(session.current!);

  // The PATCH lands: acknowledged, cached, still painted.
  await act(async () => {
    pendingPatches[0].respondWith();
  });
  await settle();
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'New');
  assert.ok(session.queue.current!.getOverlay('e1'), 'the intent is kept');

  // The GET that started first now resolves with the pre-write row.
  await act(async () => {
    staleGet.respondWith([seededEvent('e1', { title: 'Old' })]);
  });
  await settle();

  assert.equal(
    findCachedCalendarEvent(queryClient, 'e1')?.title,
    'Old',
    'the stale read really did overwrite the cache',
  );
  assert.equal(
    paintedEvent(session.current!, 'e1')?.title,
    'New',
    'the local intent still paints over the stale row',
  );
  assert.equal(
    session.current!.selectedEvent?.title,
    'New',
    'the inspector follows the paint',
  );
});

test('a GET that started after the ack and agrees retires the intent', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await settle();
  assert.equal(patchRequests().length, 1);
  assert.ok(session.queue.current!.getOverlay('e1'));

  // The server now holds the write; a read started after the ack confirms it.
  const row = serverRows.get('e1');
  assert.equal(row?.title, 'New');
  const agreedGet = await startHeldGet(session.current!);
  await act(async () => {
    agreedGet.respondWith([row]);
  });
  await settle();

  assert.equal(
    paintedEvent(session.current!, 'e1')?.title,
    'New',
    'the display is unchanged',
  );
  assert.equal(
    session.queue.current!.getOverlay('e1'),
    undefined,
    'agreement retires the field: no local intent is left',
  );
});

test('a GET that started after the ack and disagrees lets the server win', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await settle();
  assert.equal(patchRequests().length, 1);

  // A read after the ack that does not carry our value: a remote edit, or a
  // server that refused to keep it. Retire so the mismatch cannot mask every
  // later change forever.
  const remoteGet = await startHeldGet(session.current!);
  await act(async () => {
    remoteGet.respondWith([seededEvent('e1', { title: 'Remote' })]);
  });
  await settle();

  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'Remote');
  assert.equal(session.queue.current!.getOverlay('e1'), undefined);
});

test('an edit whose value already equals the server row sends no PATCH', async () => {
  seededEvents = [seededEvent('e1', { title: 'Lunch' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  await act(async () => {
    session.current!.handleSaveTitle('Lunch');
  });
  await settle();

  assert.deepEqual(patchRequests(), [], 'a no-op edit is never sent');
  assert.equal(
    paintedEvent(session.current!, 'e1')?.title,
    'Lunch',
    'the display still shows the value',
  );
  assert.equal(
    session.queue.current!.getOverlay('e1'),
    undefined,
    'the no-op intent is settled without a request',
  );
});

test('rapid edits while a PATCH is in flight squash into one request', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old', description: '' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  holdPatches = true;
  await act(async () => {
    session.current!.handleSaveTitle('A');
  });
  await waitFor(() => assert.equal(pendingPatches.length, 1));
  assert.deepEqual(pendingPatches[0].body, { summary: 'A' });

  // Two more edits while the first request is in flight: both coalesce.
  await act(async () => {
    session.current!.handleSaveDescription('note');
  });
  await act(async () => {
    session.current!.handleSaveTitle('B');
  });
  assert.equal(pendingPatches.length, 1, 'still one request in flight');

  await act(async () => {
    pendingPatches[0].respondWith();
  });
  await waitFor(() => assert.equal(pendingPatches.length, 2));
  assert.deepEqual(pendingPatches[1].body, {
    summary: 'B',
    description: 'note',
  });

  await act(async () => {
    pendingPatches[1].respondWith();
  });
  await settle();
  assert.equal(pendingPatches.length, 2, 'no third request');
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'B');
  assert.equal(paintedEvent(session.current!, 'e1')?.description, 'note');
});

test('a failed PATCH keeps the edit painted and the banner Retry re-sends it', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  holdPatches = true;
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await waitFor(() => assert.equal(pendingPatches.length, 1));
  await act(async () => {
    pendingPatches[0].fail('boom');
  });
  await settle();

  assert.equal(
    paintedEvent(session.current!, 'e1')?.title,
    'New',
    'a failed edit is kept, never reverted',
  );
  assert.match(session.current!.writeError ?? '', /Couldn't save event/);
  assert.match(session.current!.writeError ?? '', /boom/);
  assert.equal(session.current!.canRetryWrites, true);

  // The banner Retry re-sends exactly the kept intent.
  await act(async () => {
    session.current!.retryWrites();
  });
  await waitFor(() => assert.equal(pendingPatches.length, 2));
  assert.deepEqual(pendingPatches[1].body, { summary: 'New' });
  assert.equal(session.current!.writeError, null, 'the banner clears');

  await act(async () => {
    pendingPatches[1].respondWith();
  });
  await settle();
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'New');
  assert.equal(session.current!.canRetryWrites, false);
  assert.equal(
    session.queue.current!.getOverlay('e1')?.op,
    'upsert',
    'the acknowledged intent stays painted until a read retires it',
  );
});

test('an unsaved create survives unmounting and remounting the page', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));

  // Edit while the POST flies, then fail it: the local row keeps everything.
  await act(async () => {
    session.current!.handleSaveDescription('Bring the form');
  });
  pendingPosts[0].fail();
  await settle();
  assert.ok(session.current!.unsavedEventIds.has(tempId));

  // Leave the calendar route and come back. The provider stays mounted; the
  // page is a new session with new state.
  await session.unmountPage();
  await session.remountPage();

  assert.deepEqual(
    paintedIds(session.current!),
    [tempId],
    'the unsaved chip is still on the grid',
  );
  assert.ok(
    session.current!.unsavedEventIds.has(tempId),
    'the unsaved marker survives the page',
  );
  assert.equal(
    paintedEvent(session.current!, tempId)?.title,
    'Dentist',
    'the typed title survives too',
  );
  assert.equal(
    paintedEvent(session.current!, tempId)?.description,
    'Bring the form',
  );
  assert.match(
    session.current!.writeError ?? '',
    /aren't saved yet/,
    'the banner comes back with the page',
  );
  assert.equal(session.current!.canRetryWrites, true);

  // And the retry still re-POSTs its current local state.
  let retried: Promise<boolean> | null = null;
  await act(async () => {
    retried = session.current!.retryUnsavedCreate(tempId);
  });
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  assert.equal(pendingPosts[1].body.summary, 'Dentist');
  assert.equal(pendingPosts[1].body.description, 'Bring the form');

  await act(async () => {
    pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-1'));
  });
  await act(async () => {
    await retried!;
  });
  await settle();

  assert.deepEqual(paintedIds(session.current!), ['srv-1']);
  assert.equal(session.current!.unsavedEventIds.size, 0);
  assert.deepEqual(deleteRequests(), []);
});

test('dragging an unsaved create retries its POST with the moved times', async () => {
  const session = await mountSession();
  await act(async () => {
    session.current!.beginDraft(hourRange(9, 10));
  });
  const tempId = session.current!.selectedEventId!;
  await act(async () => {
    session.current!.handleSaveTitle('Dentist');
  });
  await waitFor(() => assert.equal(pendingPosts.length, 1));
  pendingPosts[0].fail();
  await settle();

  const event = session.current!.selectedEvent!;
  assert.equal(event.id, tempId);
  const hourH = session.current!.hourH;
  const startMin = minutesOf(event.start_time);
  const endMin = minutesOf(event.end_time);
  const grabY = yForMinutes((startMin + endMin) / 2, hourH);
  const dom = mountChipDom(tempId, startMin, endMin, hourH);
  try {
    await act(async () => {
      chipPointerDown(session.current!, event, dom.chipEl, grabY);
    });
    // +120 minutes of travel from the mid-chip grab moves 9:00 to 11:00.
    await movePointer(yForMinutes((startMin + endMin) / 2 + 120, hourH));
    await dropPointer(yForMinutes((startMin + endMin) / 2 + 120, hourH));
  } finally {
    dom.remove();
  }

  // The drop is a save: an unsaved create re-POSTs with its current times.
  await waitFor(() => assert.equal(pendingPosts.length, 2));
  assert.equal(pendingPosts[1].body.summary, 'Dentist');
  assert.equal(
    pendingPosts[1].body.start,
    new Date(2026, 9, 18, 11, 0, 0, 0).toISOString(),
  );

  pendingPosts[1].respondWith(createdEvent(pendingPosts[1].body, 'srv-1'));
  await settle();
  assert.deepEqual(paintedIds(session.current!), ['srv-1']);
  assert.equal(
    paintedEvent(session.current!, 'srv-1')?.start_time,
    pendingPosts[1].body.start,
  );
  assert.equal(session.current!.unsavedEventIds.size, 0);
});

test('an identical refetch keeps row identity and still retires agreeing intent', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await settle();
  assert.equal(patchRequests().length, 1);

  // The cache now holds our PATCH response; keep the row object.
  const before = findCachedCalendarEvent(queryClient, 'e1');
  assert.equal(before?.title, 'New');
  assert.ok(session.queue.current!.getOverlay('e1'));

  // A GET that returns the same values: structural sharing keeps the cached
  // object identities, yet the window stamp still advances and retires.
  const sameGet = await startHeldGet(session.current!);
  await act(async () => {
    sameGet.respondWith([before!]);
  });
  await settle();

  assert.equal(
    findCachedCalendarEvent(queryClient, 'e1'),
    before,
    'structural sharing is on: the cached row keeps its identity',
  );
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'New');
  assert.equal(
    session.queue.current!.getOverlay('e1'),
    undefined,
    'the agreeing read retires the intent despite the shared identity',
  );
});

test('the events query does not disable structural sharing', async () => {
  const { calendarEventsQueryOptions } = await import('@/app/queries/calendar');
  const options = calendarEventsQueryOptions('min', 'max');
  // Read stamps live per window, not per row, so identical refetches may keep
  // their objects.
  assert.equal(options.structuralSharing, undefined);
});

test('two edits from the same stale render both survive', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old', description: '' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  holdPatches = true;
  await act(async () => {
    // No render between them: both handlers close over the same snapshot.
    // Field-level intent means the second cannot revert the first.
    session.current!.handleSaveTitle('New');
    session.current!.handleSaveDescription('note');
  });
  await waitFor(() => assert.equal(pendingPatches.length, 1));
  assert.deepEqual(pendingPatches[0].body, { summary: 'New' });

  await act(async () => {
    pendingPatches[0].respondWith();
  });
  await waitFor(() => assert.equal(pendingPatches.length, 2));
  assert.deepEqual(
    pendingPatches[1].body,
    { description: 'note' },
    'the follow-up request must not carry a reverted title',
  );

  await act(async () => {
    pendingPatches[1].respondWith();
  });
  await settle();
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'New');
  assert.equal(paintedEvent(session.current!, 'e1')?.description, 'note');
});

test('a remount restores the banner and Retry for a failed PATCH', async () => {
  seededEvents = [seededEvent('e1', { title: 'Old' })];
  const session = await mountSession();
  await act(async () => {
    session.current!.selectEvent('e1');
  });

  holdPatches = true;
  await act(async () => {
    session.current!.handleSaveTitle('New');
  });
  await waitFor(() => assert.equal(pendingPatches.length, 1));
  await act(async () => {
    pendingPatches[0].fail('boom');
  });
  await settle();
  assert.equal(session.current!.canRetryWrites, true);

  // Leave and come back: the provider still holds the kept intent, so the
  // strip and its Retry must not disappear with the page's state.
  await session.unmountPage();
  await session.remountPage();

  assert.match(session.current!.writeError ?? '', /aren't saved yet/);
  assert.equal(
    paintedEvent(session.current!, 'e1')?.title,
    'New',
    'the failed edit is still painted',
  );
  assert.equal(session.current!.canRetryWrites, true);

  await act(async () => {
    session.current!.retryWrites();
  });
  await waitFor(() => assert.equal(pendingPatches.length, 2));
  assert.deepEqual(pendingPatches[1].body, { summary: 'New' });

  await act(async () => {
    pendingPatches[1].respondWith();
  });
  await settle();
  assert.equal(paintedEvent(session.current!, 'e1')?.title, 'New');
  assert.equal(session.current!.writeError, null, 'the banner clears');
  assert.equal(session.current!.canRetryWrites, false);
});

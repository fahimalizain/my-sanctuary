// DOM + API base must exist before the API chain is imported: static ESM
// imports hoist, so installDom() and the __API_BASE_URL__ global are set at
// module top and everything that touches the API is dynamic-imported in
// `before` (same pattern as the calendar component tests).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ComponentType, type ReactNode } from 'react';
import type { QueryClient } from '@tanstack/react-query';
import type { BackgroundSyncSession } from './start-background-sync';
import { queryKeys } from '@/app/queries/keys';
import { installDom } from '../modules/calendar/components/install-dom';

installDom();
(globalThis as { __API_BASE_URL__?: string }).__API_BASE_URL__ = '';

let queryClient: QueryClient;
let startBackgroundSync: (client: QueryClient) => BackgroundSyncSession;
let stopBackgroundSync: () => void;
let setBoardTasksBusy: (next: boolean) => void;
let BackgroundSync: ComponentType;
let AuthProvider: ComponentType<{ children: ReactNode }>;
let QueryClientProvider: ComponentType<{
  client: QueryClient;
  children?: ReactNode;
}>;
let render: typeof import('@testing-library/react').render;
let waitFor: typeof import('@testing-library/react').waitFor;
let cleanup: () => void;

// A single mutable responder per test. Requests are matched by path suffix
// (`/api/lists`, …) because the client only ever prefixes API_BASE_URL ('').
let fetchCalls: string[] = [];
let respond: (url: string) => Response | Promise<Response>;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

const DEFAULT_BODIES: Record<string, unknown> = {
  '/auth/me': { user: null },
  '/api/lists': { lists: [] },
  '/api/categories': { categories: [] },
  '/api/tasks': { tasks: [] },
  '/api/routines': { routines: [] },
  '/api/calendar/calendars': { calendars: [] },
  '/api/agenda': { time_zone: 'Asia/Riyadh' },
};

function defaultRespond(url: string): Response {
  const path = url.split('?')[0];
  return path in DEFAULT_BODIES
    ? json(DEFAULT_BODIES[path])
    : json({ error: 'unexpected' }, 404);
}

/** Let the query pipeline drain (a few macrotasks, each after the microtasks). */
async function settle(): Promise<void> {
  for (let i = 0; i < 5; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

let visibilityState: 'visible' | 'hidden' = 'visible';
function setVisibility(next: 'visible' | 'hidden'): void {
  visibilityState = next;
}
Object.defineProperty(document, 'visibilityState', {
  configurable: true,
  get: () => visibilityState,
});

before(async () => {
  (globalThis as { fetch: typeof fetch }).fetch = ((input: string) => {
    fetchCalls.push(input);
    return Promise.resolve(respond(input));
  }) as unknown as typeof fetch;

  const rtl = await import('@testing-library/react');
  render = rtl.render;
  waitFor = rtl.waitFor;
  cleanup = rtl.cleanup;

  const rq = await import('@tanstack/react-query');
  QueryClientProvider = rq.QueryClientProvider;

  const qc = await import('@/lib/queryClient');
  queryClient = qc.queryClient;

  const bg = await import('./start-background-sync');
  startBackgroundSync = bg.startBackgroundSync;
  stopBackgroundSync = bg.stopBackgroundSync;

  const busy = await import('./board-tasks-busy');
  setBoardTasksBusy = busy.setBoardTasksBusy;

  const auth = await import('@/lib/auth');
  AuthProvider = auth.AuthProvider;

  const component = await import('./BackgroundSync');
  BackgroundSync = component.BackgroundSync;
});

afterEach(() => {
  cleanup();
  stopBackgroundSync();
  setBoardTasksBusy(false);
  queryClient.clear();
  fetchCalls = [];
});

after(() => {
  cleanup();
});

const called = (suffix: string) =>
  fetchCalls.some((url) => url.endsWith(suffix));

function mountBackgroundSync(): void {
  render(
    createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(AuthProvider, null, createElement(BackgroundSync)),
    ),
  );
}

// ── 1. Seed-gated fetch order ────────────────────────────────────────────

test('startBackgroundSync: categories/tasks wait for lists; the rest start at once', async () => {
  let resolveLists!: (res: Response) => void;
  const listsPending = new Promise<Response>((resolve) => {
    resolveLists = resolve;
  });
  respond = (url) =>
    url.endsWith('/api/lists') ? listsPending : defaultRespond(url);

  const session = startBackgroundSync(queryClient);
  await settle();

  // Seed-independent observers fetched without waiting for lists.
  assert.ok(called('/api/routines'), 'routines requested immediately');
  assert.ok(called('/api/calendar/calendars'), 'calendars requested at once');
  assert.ok(called('/api/agenda'), 'agenda current requested at once');
  // Seed-gated observers have not fetched while lists is pending.
  assert.equal(called('/api/categories'), false, 'categories gated on lists');
  assert.equal(called('/api/tasks'), false, 'tasks gated on lists');
  // Never observe calendar events or a history window.
  assert.equal(
    fetchCalls.some((url) => url.includes('/api/calendar/events')),
    false,
  );
  assert.equal(
    fetchCalls.some((url) => url.includes('/api/agenda?from=')),
    false,
  );

  resolveLists(json({ lists: [] }));
  await settle();

  assert.ok(called('/api/categories'), 'categories requested after lists');
  assert.ok(called('/api/tasks'), 'tasks requested after lists');

  session.stop();
});

// ── 2. Busy flag drives the tasks observer options ───────────────────────

test('busy flag toggles the tasks observer interval and focus option', async () => {
  respond = defaultRespond;

  const session = startBackgroundSync(queryClient);
  await settle();

  const tasksQuery = queryClient
    .getQueryCache()
    .find({ queryKey: queryKeys.tasks.all });
  assert.ok(tasksQuery, 'tasks query is observed');
  const observer = tasksQuery.observers[0];
  assert.ok(observer, 'exactly one tasks observer');

  assert.equal(observer.options.refetchInterval, 60_000);
  assert.equal(observer.options.refetchOnWindowFocus, true);

  session.setTasksBusy(true);
  assert.equal(observer.options.refetchInterval, false);
  assert.equal(observer.options.refetchOnWindowFocus, false);

  session.setTasksBusy(false);
  assert.equal(observer.options.refetchInterval, 60_000);
  assert.equal(observer.options.refetchOnWindowFocus, true);

  session.stop();
});

// ── 3. Stop while lists is pending ───────────────────────────────────────

test('stop while lists is pending prevents the seed-gated fetches', async () => {
  let resolveLists!: (res: Response) => void;
  const listsPending = new Promise<Response>((resolve) => {
    resolveLists = resolve;
  });
  respond = (url) =>
    url.endsWith('/api/lists') ? listsPending : defaultRespond(url);

  const session = startBackgroundSync(queryClient);
  await settle();

  session.stop();
  session.stop(); // idempotent

  resolveLists(json({ lists: [] }));
  await settle();

  assert.equal(called('/api/categories'), false);
  assert.equal(called('/api/tasks'), false);
});

// ── 4. Visibility refresh ────────────────────────────────────────────────

test('visibilitychange refetches the agenda only when the tab is visible', async () => {
  respond = defaultRespond;

  const session = startBackgroundSync(queryClient);
  await settle();

  const agendaCalls = () =>
    fetchCalls.filter((url) => url.endsWith('/api/agenda')).length;
  const before = agendaCalls();
  assert.ok(before >= 1, 'agenda fetched at start');

  setVisibility('hidden');
  document.dispatchEvent(new Event('visibilitychange'));
  await settle();
  assert.equal(agendaCalls(), before, 'hidden tab does not refetch');

  setVisibility('visible');
  document.dispatchEvent(new Event('visibilitychange'));
  await settle();
  assert.equal(agendaCalls(), before + 1, 'visible tab refetches');

  session.stop();
});

// ── 5. Mounted component ─────────────────────────────────────────────────

test('BackgroundSync: no session when /auth/me returns no user', async () => {
  respond = defaultRespond; // /auth/me -> { user: null }

  mountBackgroundSync();
  await waitFor(() => {
    assert.ok(called('/auth/me'), 'auth probe ran');
  });
  await settle();

  assert.equal(called('/api/lists'), false, 'logged out does not start');
  assert.equal(called('/api/routines'), false, 'logged out does not start');
});

test('BackgroundSync: starts the session for a logged-in user', async () => {
  respond = (url) =>
    url.endsWith('/auth/me')
      ? json({
          user: { id: 'u1', email: 'a@b.c', name: 'Ada', picture: '' },
        })
      : defaultRespond(url);

  mountBackgroundSync();
  await waitFor(() => {
    assert.ok(called('/api/lists'), 'logged in starts the observers');
  });
});

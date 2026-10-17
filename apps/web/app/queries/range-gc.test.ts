// The range/date-keyed caches (every calendar events window, agenda history)
// keep a 30 min `gcTime`; everything else must stay on TanStack's 5 min
// default. DOM + API base must exist before the query modules are imported:
// `lib/api/client.ts` evaluates `__API_BASE_URL__` at import and static ESM
// imports hoist, so both are set at module top and the query modules are
// dynamic-imported in `before` (same constraint as BackgroundSync.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ComponentType, type ReactNode } from 'react';
import { queryKeys } from './keys';
import { installDom } from '../modules/calendar/components/install-dom';

installDom();
(globalThis as { __API_BASE_URL__?: string }).__API_BASE_URL__ = '';

const ANCHOR = '2026-10-17';
const EARLIEST = '2026-01-01';

let QueryClient: typeof import('@tanstack/react-query').QueryClient;
let QueryClientProvider: ComponentType<{
  client: import('@tanstack/react-query').QueryClient;
  children?: ReactNode;
}>;
let RANGE_QUERY_GC_TIME_MS: number;
let calendarEventsQueryOptions: typeof import('./calendar').calendarEventsQueryOptions;
let calendarsQueryOptions: typeof import('./calendar').calendarsQueryOptions;
let agendaCurrentQueryOptions: typeof import('./agenda').agendaCurrentQueryOptions;
let useAgendaHistory: typeof import('./agenda').useAgendaHistory;

let fetchCalls: string[] = [];
let render: typeof import('@testing-library/react').render;
let cleanup: () => void;

before(async () => {
  // Nothing in this file should fetch: the events factory is only read as
  // options and the history hook mounts with `enabled: false`. Record any
  // request instead of serving it so the hook assertion fails loudly.
  (globalThis as { fetch: typeof fetch }).fetch = ((input: string) => {
    fetchCalls.push(input);
    return Promise.reject(new Error(`unexpected fetch: ${input}`));
  }) as unknown as typeof fetch;

  const rtl = await import('@testing-library/react');
  render = rtl.render;
  cleanup = rtl.cleanup;

  const rq = await import('@tanstack/react-query');
  QueryClient = rq.QueryClient;
  QueryClientProvider = rq.QueryClientProvider;

  const lib = await import('@/lib/queryClient');
  RANGE_QUERY_GC_TIME_MS = lib.RANGE_QUERY_GC_TIME_MS;

  const calendarQueries = await import('./calendar');
  calendarEventsQueryOptions = calendarQueries.calendarEventsQueryOptions;
  calendarsQueryOptions = calendarQueries.calendarsQueryOptions;

  const agendaQueries = await import('./agenda');
  agendaCurrentQueryOptions = agendaQueries.agendaCurrentQueryOptions;
  useAgendaHistory = agendaQueries.useAgendaHistory;
});

afterEach(() => {
  cleanup();
  fetchCalls = [];
});

after(() => {
  cleanup();
});

test('RANGE_QUERY_GC_TIME_MS is 30 minutes', () => {
  assert.equal(RANGE_QUERY_GC_TIME_MS, 30 * 60 * 1000);
});

test('every calendar events window keeps its cache for the range gcTime', () => {
  assert.equal(
    calendarEventsQueryOptions('a', 'b').gcTime,
    RANGE_QUERY_GC_TIME_MS,
  );
});

test('calendars inherits the 5 min default (no per-query gcTime)', () => {
  assert.equal(calendarsQueryOptions().gcTime, undefined);
});

test('agenda.current inherits the 5 min default (no per-query gcTime)', () => {
  assert.equal(agendaCurrentQueryOptions().gcTime, undefined);
});

test('useAgendaHistory registers its anchor-keyed cache with the range gcTime', () => {
  // A private client: the singleton from `@/lib/queryClient` must not be
  // polluted by this test's cache.
  const client = new QueryClient();

  function HistoryProbe() {
    useAgendaHistory(ANCHOR, EARLIEST, false);
    return null;
  }

  render(
    createElement(QueryClientProvider, { client }, createElement(HistoryProbe)),
  );

  const query = client
    .getQueryCache()
    .find({ queryKey: queryKeys.agenda.history(ANCHOR) });
  assert.ok(query, 'the hook registers its anchor-keyed history query');
  const observer = query.observers[0];
  assert.ok(observer, 'the mounted hook holds an observer on it');
  assert.equal(observer.options.gcTime, RANGE_QUERY_GC_TIME_MS);
  assert.deepEqual(fetchCalls, [], 'enabled: false must not fetch');

  // Unmount, then destroy the query: the 30 min gc timer the cache schedules
  // for it is a real `setTimeout` and would hold the test process open.
  cleanup();
  client.clear();
});

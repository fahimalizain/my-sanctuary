import { QueryObserver } from '@tanstack/react-query';
import type { QueryClient } from '@tanstack/react-query';
import { agendaCurrentQueryOptions } from '@/app/queries/agenda';
import {
  calendarEventsQueryOptions,
  calendarsQueryOptions,
} from '@/app/queries/calendar';
import { categoriesQueryOptions } from '@/app/queries/categories';
import { queryKeys } from '@/app/queries/keys';
import { listsQueryOptions } from '@/app/queries/lists';
import { routinesQueryOptions } from '@/app/queries/routines';
import {
  TASKS_REFETCH_INTERVAL_MS,
  tasksQueryOptions,
} from '@/app/queries/tasks';
import {
  LOCAL_MIDNIGHT_SKEW_MS,
  initialStripRange,
  msUntilLocalMidnight,
} from '@/app/modules/calendar/lib/initial-strip-range';
import {
  getPeriodLength,
  subscribePeriodLength,
} from '@/app/modules/calendar/period-length-store';
import { AGENDA_MIDNIGHT_SKEW_MS, msUntilNextMidnight } from './agenda-clock';
import { getBoardTasksBusy } from './board-tasks-busy';

// Headless, router-independent observers for the shared session queries. Kept
// alive above the router so window focus, the 60s tasks poll and
// `invalidateQueries` still reach them when no page is mounted. TanStack Query
// stays the only cache — nothing is copied out of it here.

/** The live session a mounted `<BackgroundSync />` owns. `stop` is idempotent
 *  so both the component cleanup and `stopBackgroundSync` (logout) may call
 *  it. */
export interface BackgroundSyncSession {
  setTasksBusy(busy: boolean): void;
  stop(): void;
}

/** The one session currently observing. `useLogout` destroys it synchronously
 *  before `queryClient.clear()` (see queries/auth.ts). */
let activeSession: BackgroundSyncSession | null = null;

/** Starts a fresh session, stopping any previous one first. */
export function startBackgroundSync(
  queryClient: QueryClient,
): BackgroundSyncSession {
  stopBackgroundSync();
  const session = createSession(queryClient);
  activeSession = session;
  return session;
}

/** Destroys the active session's observers, listeners and timers
 *  synchronously. Idempotent. */
export function stopBackgroundSync(): void {
  activeSession?.stop();
  activeSession = null;
}

function createSession(queryClient: QueryClient): BackgroundSyncSession {
  let stopped = false;
  // Remembered while tasks are not observed yet (the lists seed gate) and
  // applied when the tasks observer is created.
  let tasksBusy = getBoardTasksBusy();
  let midnightTimer: ReturnType<typeof setTimeout> | undefined;
  // The local-day timer that rolls the strip landing window forward.
  let eventsMidnightTimer: ReturnType<typeof setTimeout> | undefined;
  // The zone the armed timer belongs to — a refetch that resolves the same
  // zone must not reset it.
  let armedZone: string | null = null;

  const createCategoriesObserver = () =>
    new QueryObserver(queryClient, {
      ...categoriesQueryOptions(),
      notifyOnChangeProps: [],
    });
  const createTasksObserver = () =>
    new QueryObserver(queryClient, {
      ...tasksQueryOptions(),
      refetchInterval: tasksBusy ? false : TASKS_REFETCH_INTERVAL_MS,
      refetchOnWindowFocus: !tasksBusy,
      notifyOnChangeProps: [],
    });

  let categoriesObserver: ReturnType<typeof createCategoriesObserver> | null =
    null;
  let tasksObserver: ReturnType<typeof createTasksObserver> | null = null;

  // Lists is the first-visit seed: `GET /api/lists` seeds the lists AND the
  // category taxonomy, while `GET /api/categories` does not seed and a
  // parallel first fetch can cache `[]`. So categories and tasks wait for
  // lists success — and an error does not block them forever: the listener
  // stays subscribed and a later (focus) success starts them.
  const listsObserver = new QueryObserver(queryClient, listsQueryOptions());
  const seedDependents = () => {
    if (stopped || categoriesObserver) return;
    if (!listsObserver.getCurrentResult().isSuccess) return;
    categoriesObserver = createCategoriesObserver();
    categoriesObserver.subscribe(() => {});
    tasksObserver = createTasksObserver();
    tasksObserver.subscribe(() => {});
  };
  listsObserver.subscribe(seedDependents);
  // Lists may already be cached (another page fetched them) — start the
  // dependents now instead of waiting for a change.
  seedDependents();

  const routinesObserver = new QueryObserver(queryClient, {
    ...routinesQueryOptions(),
    notifyOnChangeProps: [],
  });
  routinesObserver.subscribe(() => {});

  const calendarsObserver = new QueryObserver(queryClient, {
    ...calendarsQueryOptions(),
    notifyOnChangeProps: [],
  });
  calendarsObserver.subscribe(() => {});

  // ── Calendar strip landing range ──────────────────────────────────────
  // Calendar's first paint queries this exact window; observing it here keeps
  // that key warm after the user leaves the page, so opening Calendar is a cache
  // hit instead of a blank grid. The factory is spread rather than rebuilt, so
  // queryFn, key and placeholderData stay identical to the page's.
  //
  // Landing window, not the scrolled one: after the user scrolls, the page's key
  // diverges and this one deliberately does not follow. The strip remounts from
  // the same helper, so that is the key worth keeping warm.
  let eventsRange = initialStripRange(new Date(), getPeriodLength());
  const eventsObserver = new QueryObserver(queryClient, {
    ...calendarEventsQueryOptions(eventsRange.timeMin, eventsRange.timeMax),
    notifyOnChangeProps: [],
  });

  /** Point the events observer at the current landing window. An unchanged
   *  range is left alone: `setOptions` would re-run the query for nothing. */
  const retargetEventsObserver = () => {
    if (stopped) return;
    const next = initialStripRange(new Date(), getPeriodLength());
    if (
      next.timeMin === eventsRange.timeMin &&
      next.timeMax === eventsRange.timeMax
    ) {
      return;
    }
    eventsRange = next;
    eventsObserver.setOptions({
      ...calendarEventsQueryOptions(next.timeMin, next.timeMax),
      notifyOnChangeProps: [],
    });
  };

  eventsObserver.subscribe(() => {});

  // A period change is a UI preference: follow it here rather than in a React
  // effect, which would destroy and recreate every observer in this session.
  const unsubscribePeriodLength = subscribePeriodLength(retargetEventsObserver);

  // The landing window is the browser's civil day, not the agenda's IANA zone,
  // so this rolls at *local* midnight — `msUntilNextMidnight` would be a day off
  // across a DST boundary.
  const armEventsMidnight = () => {
    eventsMidnightTimer = setTimeout(() => {
      eventsMidnightTimer = undefined;
      if (stopped) return;
      retargetEventsObserver();
      armEventsMidnight();
    }, msUntilLocalMidnight() + LOCAL_MIDNIGHT_SKEW_MS);
  };
  armEventsMidnight();

  // Agenda current must keep its listener: the result carries the IANA
  // `time_zone` that arms the midnight refresh. `notifyOnChangeProps: []` here
  // would suppress it.
  const agendaObserver = new QueryObserver(
    queryClient,
    agendaCurrentQueryOptions(),
  );

  const armMidnight = (zone: string) => {
    const delay = msUntilNextMidnight(zone);
    if (delay === null) return;
    // +30s: a fast clock must not refetch while the server is still on yesterday.
    midnightTimer = setTimeout(() => {
      midnightTimer = undefined;
      void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
      if (!stopped) armMidnight(zone);
    }, delay + AGENDA_MIDNIGHT_SKEW_MS);
  };

  const syncMidnightTimer = () => {
    if (stopped) return;
    const zone = agendaObserver.getCurrentResult().data?.time_zone ?? '';
    if (zone === armedZone) return;
    armedZone = zone;
    if (midnightTimer !== undefined) {
      clearTimeout(midnightTimer);
      midnightTimer = undefined;
    }
    if (zone) armMidnight(zone);
  };
  agendaObserver.subscribe(syncMidnightTimer);
  syncMidnightTimer();

  // Coming back to a backgrounded PWA: `today` may have rolled over while the
  // tab was hidden. Fire and forget — never on subscribe, never when hidden.
  // `agenda.all` (not just `current`) so Home's history observers refetch when
  // they exist and inactive history is left alone otherwise.
  const onVisibilityChange = () => {
    if (document.visibilityState !== 'visible') return;
    void queryClient.invalidateQueries({ queryKey: queryKeys.agenda.all });
  };
  document.addEventListener('visibilitychange', onVisibilityChange);

  const setTasksBusy = (next: boolean) => {
    tasksBusy = next;
    if (!tasksObserver) return;
    tasksObserver.setOptions({
      ...tasksQueryOptions(),
      refetchInterval: tasksBusy ? false : TASKS_REFETCH_INTERVAL_MS,
      refetchOnWindowFocus: !tasksBusy,
      notifyOnChangeProps: [],
    });
  };

  const stop = () => {
    if (stopped) return;
    stopped = true;
    listsObserver.destroy();
    categoriesObserver?.destroy();
    tasksObserver?.destroy();
    routinesObserver.destroy();
    calendarsObserver.destroy();
    eventsObserver.destroy();
    unsubscribePeriodLength();
    agendaObserver.destroy();
    document.removeEventListener('visibilitychange', onVisibilityChange);
    if (midnightTimer !== undefined) {
      clearTimeout(midnightTimer);
      midnightTimer = undefined;
    }
    // `stopped` is what actually keeps a late period callback or timer fire from
    // recreating the observer; clearing the timer only stops the wait.
    if (eventsMidnightTimer !== undefined) {
      clearTimeout(eventsMidnightTimer);
      eventsMidnightTimer = undefined;
    }
  };

  return { setTasksBusy, stop };
}

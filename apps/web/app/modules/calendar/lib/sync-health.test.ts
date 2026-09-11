import { test } from 'node:test';
import assert from 'node:assert/strict';
import type {
  CalendarEventsSync,
  CalendarSyncHealth,
  GoogleCalendar,
} from '@/app/types';
import { repairTargetCalendarId, selectSyncHealthBanner } from './sync-health';

function health(
  overrides: Partial<CalendarSyncHealth> &
    Pick<CalendarSyncHealth, 'calendar_id' | 'state'>,
): CalendarSyncHealth {
  return {
    initial_sync_complete: overrides.state === 'ready',
    last_success_at: null,
    last_attempt_at: null,
    stale: false,
    error_code: null,
    retry_after_seconds: null,
    projection: 'timed_masters_and_exceptions',
    cache_revision: 0,
    watch_coverage: 'missing',
    ...overrides,
  };
}

function sync(...calendars: CalendarSyncHealth[]): CalendarEventsSync {
  const hasAuth = calendars.some((c) => c.state === 'authorization_required');
  const hasDegraded = calendars.some(
    (c) =>
      c.state === 'retrying' ||
      c.state === 'rebuilding' ||
      (c.stale === true && c.state !== 'never_initialized'),
  );
  return {
    status: hasAuth
      ? 'authorization_required'
      : hasDegraded
        ? 'degraded'
        : 'ready',
    calendars,
  };
}

const personal: GoogleCalendar = {
  id: 'cal-personal',
  google_calendar_id: 'personal@example.com',
  summary: 'Personal Goals',
  time_zone: 'UTC',
  is_primary: true,
  access_role: 'owner',
  sync_enabled: true,
};

const work: GoogleCalendar = {
  id: 'cal-work',
  google_calendar_id: 'work@example.com',
  summary: 'Work',
  time_zone: 'UTC',
  is_primary: false,
  access_role: 'owner',
  sync_enabled: true,
};

const base = {
  calendars: [personal, work],
  fetchError: null as string | null,
  eventsEmpty: false,
  isLoading: false,
};

test('ready → none', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(health({ calendar_id: 'cal-personal', state: 'ready' })),
  });
  assert.equal(banner.kind, 'none');
});

test('empty + ready → none (truly empty window)', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    eventsEmpty: true,
    sync: sync(health({ calendar_id: 'cal-personal', state: 'ready' })),
  });
  assert.equal(banner.kind, 'none');
});

test('never_initialized → quiet syncing copy', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({ calendar_id: 'cal-personal', state: 'never_initialized' }),
    ),
  });
  assert.equal(banner.kind, 'never_initialized');
  assert.equal(banner.message, 'Syncing earlier events…');
  assert.equal(banner.showRetry, false);
  assert.equal(banner.showReconnect, false);
  assert.equal(banner.calendarName, 'Personal Goals');
  assert.equal(banner.calendarId, 'cal-personal');
  assert.equal(repairTargetCalendarId(banner), null);
});

test('never_initialized + stale → still quiet syncing (not out of date)', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'never_initialized',
        stale: true,
      }),
    ),
  });
  assert.equal(banner.kind, 'never_initialized');
  assert.equal(banner.message, 'Syncing earlier events…');
  assert.equal(banner.showRetry, false);
  assert.equal(banner.showReconnect, false);
});

test('empty window + never_initialized + stale → quiet syncing (not no events)', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    eventsEmpty: true,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'never_initialized',
        stale: true,
      }),
    ),
  });
  assert.equal(banner.kind, 'never_initialized');
  assert.equal(banner.message, 'Syncing earlier events…');
  assert.equal(banner.showRetry, false);
  assert.equal(banner.showReconnect, false);
});

test('authorization_required → reconnect, strongest vs stale sibling', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-work',
        state: 'ready',
        stale: true,
      }),
      health({
        calendar_id: 'cal-personal',
        state: 'authorization_required',
      }),
    ),
  });
  assert.equal(banner.kind, 'authorization_required');
  assert.equal(
    banner.message,
    'Reconnect Google to keep Personal Goals in sync.',
  );
  assert.equal(banner.showReconnect, true);
  assert.equal(banner.showRetry, false);
  assert.equal(banner.calendarName, 'Personal Goals');
  assert.equal(banner.calendarId, 'cal-personal');
  assert.equal(repairTargetCalendarId(banner), null);
});

test('degraded: retrying includes calendar name', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'retrying',
        error_code: 'rate_limited',
      }),
    ),
  });
  assert.equal(banner.kind, 'degraded');
  assert.equal(banner.message, 'Personal Goals is retrying (rate_limited)');
  assert.equal(banner.showRetry, true);
  assert.equal(banner.showReconnect, false);
  assert.equal(banner.calendarId, 'cal-personal');
  assert.equal(repairTargetCalendarId(banner), 'cal-personal');
});

test('degraded: stale ready is still degraded', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'ready',
        stale: true,
      }),
    ),
  });
  assert.equal(banner.kind, 'degraded');
  assert.equal(banner.message, 'Personal Goals is out of date');
  assert.equal(banner.showRetry, true);
  assert.equal(banner.calendarId, 'cal-personal');
  assert.equal(repairTargetCalendarId(banner), 'cal-personal');
});

test('degraded: rebuilding', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-work',
        state: 'rebuilding',
      }),
    ),
  });
  assert.equal(banner.kind, 'degraded');
  assert.equal(banner.message, 'Work is rebuilding');
  assert.equal(banner.calendarId, 'cal-work');
  assert.equal(repairTargetCalendarId(banner), 'cal-work');
});

test('disabled is ignored even when stale', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'disabled',
        stale: true,
        error_code: 'gone',
      }),
      health({ calendar_id: 'cal-work', state: 'ready' }),
    ),
  });
  assert.equal(banner.kind, 'none');
});

test('fetch_error with retained rows', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    fetchError: 'network down',
    eventsEmpty: false,
    sync: sync(health({ calendar_id: 'cal-personal', state: 'ready' })),
  });
  assert.equal(banner.kind, 'fetch_error');
  assert.equal(banner.message, "Couldn't refresh events: network down");
  assert.equal(banner.showRetry, true);
  assert.equal(banner.showReconnect, false);
  assert.equal(banner.calendarId, undefined);
  assert.equal(repairTargetCalendarId(banner), null);
});

test('first-load loading + empty → none', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    isLoading: true,
    eventsEmpty: true,
    fetchError: null,
    sync: undefined,
  });
  assert.equal(banner.kind, 'none');
});

test('hard empty fetch error → none (grid overlay owns it)', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    isLoading: false,
    eventsEmpty: true,
    fetchError: 'boom',
    sync: undefined,
  });
  assert.equal(banner.kind, 'none');
});

test('missing sync → none', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: undefined,
  });
  assert.equal(banner.kind, 'none');
});

test('calendar list missing → generic name', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    calendars: [],
    sync: sync(
      health({
        calendar_id: 'cal-unknown',
        state: 'authorization_required',
      }),
    ),
  });
  assert.equal(banner.kind, 'authorization_required');
  assert.equal(banner.message, 'Reconnect Google to keep A calendar in sync.');
});

test('empty summary falls back to google_calendar_id', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    calendars: [
      {
        ...personal,
        summary: '  ',
      },
    ],
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'retrying',
      }),
    ),
  });
  assert.equal(banner.message, 'personal@example.com is retrying');
});

test('featured name: first auth among several', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({ calendar_id: 'cal-work', state: 'authorization_required' }),
      health({
        calendar_id: 'cal-personal',
        state: 'authorization_required',
      }),
    ),
  });
  assert.equal(banner.calendarName, 'Work');
});

test('featured name: first degraded when no auth', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({ calendar_id: 'cal-work', state: 'ready', stale: true }),
      health({ calendar_id: 'cal-personal', state: 'retrying' }),
    ),
  });
  assert.equal(banner.calendarName, 'Work');
  assert.equal(banner.kind, 'degraded');
  assert.equal(banner.calendarId, 'cal-work');
  assert.equal(repairTargetCalendarId(banner), 'cal-work');
});

test('never_initialized loses to degraded sibling', () => {
  const banner = selectSyncHealthBanner({
    ...base,
    sync: sync(
      health({
        calendar_id: 'cal-personal',
        state: 'never_initialized',
      }),
      health({ calendar_id: 'cal-work', state: 'retrying' }),
    ),
  });
  assert.equal(banner.kind, 'degraded');
  assert.equal(banner.calendarName, 'Work');
});

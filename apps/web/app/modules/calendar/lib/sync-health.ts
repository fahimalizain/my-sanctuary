import type {
  CalendarEventsSync,
  CalendarSyncHealth,
  GoogleCalendar,
} from '@/app/types';

export type SyncHealthBannerKind =
  | 'none'
  | 'fetch_error'
  | 'authorization_required'
  | 'never_initialized'
  | 'degraded';

export type SyncHealthBanner = {
  kind: SyncHealthBannerKind;
  message: string;
  showRetry: boolean;
  showReconnect: boolean;
  calendarName?: string;
  state?: CalendarSyncHealth['state'];
  errorCode?: string | null;
  retryAfterSeconds?: number | null;
  lastSuccessAt?: string | null;
};

const NONE: SyncHealthBanner = {
  kind: 'none',
  message: '',
  showRetry: false,
  showReconnect: false,
};

function calendarDisplayName(
  health: CalendarSyncHealth,
  calendars: GoogleCalendar[],
): string {
  const cal = calendars.find((c) => c.id === health.calendar_id);
  if (!cal) return 'A calendar';
  const summary = cal.summary?.trim();
  if (summary) return summary;
  const googleId = cal.google_calendar_id?.trim();
  if (googleId) return googleId;
  return 'A calendar';
}

function isEnabled(health: CalendarSyncHealth): boolean {
  return health.state !== 'disabled';
}

function isDegraded(health: CalendarSyncHealth): boolean {
  return (
    health.state === 'retrying' ||
    health.state === 'rebuilding' ||
    (health.stale === true && health.state !== 'never_initialized')
  );
}

function withFeatured(
  kind: SyncHealthBannerKind,
  health: CalendarSyncHealth,
  calendars: GoogleCalendar[],
  message: string,
  showRetry: boolean,
  showReconnect: boolean,
): SyncHealthBanner {
  const calendarName = calendarDisplayName(health, calendars);
  return {
    kind,
    message,
    showRetry,
    showReconnect,
    calendarName,
    state: health.state,
    errorCode: health.error_code,
    retryAfterSeconds: health.retry_after_seconds,
    lastSuccessAt: health.last_success_at,
  };
}

function degradedMessage(name: string, health: CalendarSyncHealth): string {
  let base: string;
  if (health.state === 'retrying') {
    base = `${name} is retrying`;
  } else if (health.state === 'rebuilding') {
    base = `${name} is rebuilding`;
  } else {
    base = `${name} is out of date`;
  }
  if (health.error_code) {
    return `${base} (${health.error_code})`;
  }
  return base;
}

/**
 * Pick at most one chrome banner from the GET events `sync` envelope +
 * fetch error. First-load and hard-empty errors stay on the week-grid overlays.
 */
export function selectSyncHealthBanner(input: {
  sync: CalendarEventsSync | undefined;
  calendars: GoogleCalendar[];
  fetchError: string | null;
  eventsEmpty: boolean;
  isLoading: boolean;
}): SyncHealthBanner {
  const { sync, calendars, fetchError, eventsEmpty, isLoading } = input;

  // Existing first-load overlay owns this.
  if (isLoading && eventsEmpty) return NONE;

  // Existing grid hard-error overlay owns this.
  if (fetchError && eventsEmpty && !isLoading) return NONE;

  // Soft fetch failure while retained rows stay painted.
  if (fetchError && !eventsEmpty) {
    return {
      kind: 'fetch_error',
      message: `Couldn't refresh events: ${fetchError}`,
      showRetry: true,
      showReconnect: false,
    };
  }

  if (!sync) return NONE;

  const enabled = sync.calendars.filter(isEnabled);

  const auth = enabled.find((c) => c.state === 'authorization_required');
  if (auth) {
    const name = calendarDisplayName(auth, calendars);
    return withFeatured(
      'authorization_required',
      auth,
      calendars,
      `Reconnect Google to keep ${name} in sync.`,
      false,
      true,
    );
  }

  const degraded = enabled.find(isDegraded);
  if (degraded) {
    const name = calendarDisplayName(degraded, calendars);
    return withFeatured(
      'degraded',
      degraded,
      calendars,
      degradedMessage(name, degraded),
      true,
      false,
    );
  }

  const neverInit = enabled.find((c) => c.state === 'never_initialized');
  if (neverInit) {
    return withFeatured(
      'never_initialized',
      neverInit,
      calendars,
      'Syncing earlier events…',
      false,
      false,
    );
  }

  return NONE;
}

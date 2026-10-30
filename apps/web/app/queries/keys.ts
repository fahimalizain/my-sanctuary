import type { RecordRangeKey } from '@/app/types';

export const queryKeys = {
  auth: {
    all: ['auth'] as const,
    me: () => [...queryKeys.auth.all, 'me'] as const,
  },
  lists: {
    all: ['lists'] as const,
  },
  categories: {
    all: ['categories'] as const,
  },
  tasks: {
    all: ['tasks'] as const,
  },
  routines: {
    all: ['routines'] as const,
  },
  rules: {
    all: ['rules'] as const,
    scoreboard: () => [...queryKeys.rules.all, 'scoreboard'] as const,
    archive: () => [...queryKeys.rules.all, 'archive'] as const,
    // `/api/pauses` lives under `rules` on purpose: every rule mutation
    // invalidates the pause list, so one prefix invalidation reaches it.
    pauses: () => [...queryKeys.rules.all, 'pauses'] as const,
  },
  // "The Record" (reports slice 5). Keyed by the range picker's value, so a
  // range change is a fresh read and `record.all` invalidates every range.
  record: {
    all: ['record'] as const,
    range: (range: RecordRangeKey) => [...queryKeys.record.all, range] as const,
  },
  agenda: {
    all: ['agenda'] as const,
    // Home's two reads (ADR 0004 amendment, ADR 0007): the default window
    // (`anchor_date … today + 6` plus the backlog) and the history walk keyed
    // by the anchor it walks behind. `agenda.all` stays the prefix of both, so
    // one invalidation refetches today together with every loaded history
    // week.
    current: () => [...queryKeys.agenda.all, 'current'] as const,
    history: (anchor: string) =>
      [...queryKeys.agenda.all, 'history', anchor] as const,
  },
  calendar: {
    all: ['calendar'] as const,
    calendars: () => [...queryKeys.calendar.all, 'calendars'] as const,
    events: (timeMin: string, timeMax: string) =>
      [...queryKeys.calendar.all, 'events', timeMin, timeMax] as const,
  },
  // Settings' API keys. Named `apiKeys` because `keys` would read as this
  // file's own key factories.
  apiKeys: {
    all: ['apiKeys'] as const,
  },
};

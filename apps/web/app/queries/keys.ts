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
};

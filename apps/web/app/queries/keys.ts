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
    // Home's one infinite day-stream (`useAgendaStream`): the only agenda
    // cache key. Every page of it answers to `agenda.all` — task and routine
    // writes invalidate the whole stream so the server's row repointing lands.
    stream: () => [...queryKeys.agenda.all, 'stream'] as const,
  },
  calendar: {
    all: ['calendar'] as const,
    calendars: () => [...queryKeys.calendar.all, 'calendars'] as const,
    events: (timeMin: string, timeMax: string) =>
      [...queryKeys.calendar.all, 'events', timeMin, timeMax] as const,
  },
};

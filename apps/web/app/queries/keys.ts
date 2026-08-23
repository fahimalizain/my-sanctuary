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
    // `undefined` / empty → the 'today' sentinel. Slice 8 will write the
    // server's civil today into the dated key after the first load.
    byDate: (date?: string) =>
      [...queryKeys.agenda.all, date ? date : 'today'] as const,
  },
  calendar: {
    all: ['calendar'] as const,
    calendars: () => [...queryKeys.calendar.all, 'calendars'] as const,
    events: (timeMin: string, timeMax: string) =>
      [...queryKeys.calendar.all, 'events', timeMin, timeMax] as const,
  },
};
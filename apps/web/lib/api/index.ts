// The app's API client — every typed request function under one roof.
// `@/lib/api` imports (pages, lib/auth.tsx) resolve here.

export { API_BASE_URL } from './base';
export { ApiError, parseApiError } from './error';
export { fetchJson } from './client';
export * from './lists';
export * from './categories';
export * from './tasks';
export * from './agenda';
export * from './occurrences';
export * from './routines';
export * from './calendar';
export * from './auth';

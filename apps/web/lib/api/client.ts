import { API_BASE_URL } from './base';
import { parseApiError } from './error';

/** The one fetch wrapper behind every domain function. Always sends
 *  credentials, prefixes `API_BASE_URL`, JSON-encodes a present `body`, and
 *  throws `ApiError` (via parseApiError) on any non-ok response — so these
 *  functions are ready to become `queryFn`/`mutationFn` later (they throw on
 *  HTTP error instead of returning a Response).
 *
 *  - `body` is optional and JSON.stringified when present; the caller's
 *    `headers` win on conflict with the default `Content-Type`.
 *  - Empty bodies (204 / no text) resolve as `undefined`.
 *  - AbortSignal passes straight through — an abort rejects with the DOM
 *    AbortError untouched; network failures throw the fetch TypeError as
 *    today. */
export async function fetchJson<T>(
  path: string,
  options?: Omit<RequestInit, 'body'> & { body?: unknown },
): Promise<T> {
  // Destructure `body` so the rest spreads cleanly into RequestInit (the
  // raw `body?: unknown` would not be assignable to `BodyInit`).
  const { body, ...rest } = options ?? {};
  const init: RequestInit = { ...rest, credentials: 'include' };
  if (body !== undefined) {
    init.body = JSON.stringify(body);
    init.headers = { 'Content-Type': 'application/json', ...rest.headers };
  }
  const res = await fetch(`${API_BASE_URL}${path}`, init);
  if (!res.ok) throw await parseApiError(res);
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}
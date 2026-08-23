// The shared HTTP error for the api client. No Vite globals here on purpose —
// this module must stay unit-testable under tsx (node), where the
// `__API_BASE_URL__` define does not exist.

/** A failed HTTP request. `message` is the server's `{"error": "…"}` body
 *  when it carries one, otherwise a generic status line. Extends Error so
 *  existing `err instanceof Error ? err.message` callers keep working. */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

/** Builds an ApiError from a non-ok Response — the same envelope the pages
 *  used to parse locally: `{"error": "message"}` → that string; anything
 *  else → `Request failed with status ${status}`. */
export async function parseApiError(res: Response): Promise<ApiError> {
  try {
    const data: unknown = await res.json();
    if (
      data &&
      typeof data === 'object' &&
      'error' in data &&
      typeof (data as { error: unknown }).error === 'string'
    ) {
      return new ApiError((data as { error: string }).error, res.status);
    }
  } catch {
    // Not JSON — fall through to the generic message.
  }
  return new ApiError(`Request failed with status ${res.status}`, res.status);
}
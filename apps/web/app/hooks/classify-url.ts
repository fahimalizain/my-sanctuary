// Pure URL builder for `GET /api/tasks/classify` — no fetch, no React, no
// `@/lib/api` (whose `__API_BASE_URL__` global only exists under Vite), so it
// can be unit-tested with node:test. The hook prefixes `API_BASE_URL`.
//
// A `null` (or empty) lock appends no `category_id`; an empty title is only
// legal when locked (the server 400s an empty unlocked classify).
export function buildClassifyUrl(
  title: string,
  categoryId: string | null,
): string {
  let url = `/api/tasks/classify?title=${encodeURIComponent(title)}`;
  if (categoryId !== null && categoryId !== '') {
    url += `&category_id=${encodeURIComponent(categoryId)}`;
  }
  return url;
}

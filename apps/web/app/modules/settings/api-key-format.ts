import type { ApiKey } from '@/app/types';

// ──────────────────────────────────────────
// Pure label/format helpers for the API key list. Kept out of the component
// so they are unit-testable under tsx (no DOM, no Intl for dates).
// ──────────────────────────────────────────

const MONTHS = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
];

/**
 * `YYYY-MM-DD` → `12 Nov 2026`.
 *
 * Parsed by hand on purpose: `new Date('2026-11-12')` reads the date as UTC
 * midnight, so a negative-offset zone would render the day before. This
 * helper never touches `Date`, so its output is independent of `TZ`.
 */
export function formatExpiryDate(expiresOn: string): string {
  const [year, month, day] = expiresOn.split('-');
  const monthLabel = MONTHS[Number(month) - 1];
  if (!year || !monthLabel || !day) return expiresOn;
  return `${Number(day)} ${monthLabel} ${year}`;
}

/**
 * Whether the server would reject the key now. The exact instant counts as
 * expired — the server compares `expires_at <= now` too. An unparseable
 * `expires_at` is treated as not expired (the server only writes RFC 3339).
 */
export function isExpired(expiresAt: string | null, now: number): boolean {
  return expiresAt !== null && Date.parse(expiresAt) <= now;
}

/**
 * The list-row expiry line: `Never expires`, `Expires 12 Nov 2026`, or
 * `Expired 12 Nov 2026`. The date shown is the civil `expires_on`; the
 * `expires_at` instant only decides the verdict.
 */
export function expiryLabel(
  key: Pick<ApiKey, 'expires_on' | 'expires_at'>,
  now: number,
): string {
  if (key.expires_on !== null) {
    const date = formatExpiryDate(key.expires_on);
    return isExpired(key.expires_at, now)
      ? `Expired ${date}`
      : `Expires ${date}`;
  }
  return isExpired(key.expires_at, now) ? 'Expired' : 'Never expires';
}

// `en-GB` medium date + short time: `12 Nov 2026, 14:05`.
const LAST_USED_FORMAT = new Intl.DateTimeFormat('en-GB', {
  dateStyle: 'medium',
  timeStyle: 'short',
});

/** `Never used`, or `Last used 12 Nov 2026, 14:05` in the viewer's zone. */
export function lastUsedLabel(lastUsedAt: string | null): string {
  if (lastUsedAt === null) return 'Never used';
  const at = Date.parse(lastUsedAt);
  if (Number.isNaN(at)) return 'Never used';
  return `Last used ${LAST_USED_FORMAT.format(at)}`;
}

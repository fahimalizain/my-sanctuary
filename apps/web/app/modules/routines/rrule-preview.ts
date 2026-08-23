// RRULE expansion + summaries for /routines (ADR 0004 amendment § Recurrence).
//
// The npm `rrule` engine must produce the SAME local civil dates as the Rust
// crate (`packages/api-core/src/routines.rs::occurrence_dates`) — golden
// fixtures (`packages/api-core/fixtures/rrule_golden.json`) are asserted on
// BOTH engines so they cannot drift.
//
// The stored recurrence is ONE blob with exactly two newline-separated lines:
// `DTSTART:YYYYMMDDTHHMMSS` (floating local, no Z/TZID) + `RRULE:<body>`.
// `parseRruleBlob` splits it; the body goes to npm `rrule` and the DTSTART is
// converted to the civil `YYYY-MM-DDTHH:MM:SS` form the engine helpers take.
//
// Floating local civil time, always: the civil dtstart is attached to UTC
// purely as a carrier when handed to npm `rrule`, and results are read back
// off the UTC components. The browser's local timezone is NEVER consulted for
// membership — same UTC-carrier trick as the Rust side.
//
// One engine quirk this helper works around: with `forceset: true`,
// `rrulestr` ignores the `dtstart` OPTION and only reads a `DTSTART:` line
// inside the string itself, so we prepend a Z-form DTSTART line built from
// the civil components.

import * as rruleModule from 'rrule';

// npm `rrule` ships a Babel-compiled CJS main: under Node ESM (tsx --test)
// its real exports hide behind the interop `default`, while bundlers (Vite)
// resolve the ESM build and expose true named exports. Resolve both shapes
// once so the helper runs identically in tests and in the app.
type RruleApi = typeof rruleModule;
const rrule: RruleApi = ((rruleModule as unknown as { default?: RruleApi }).default ??
  rruleModule) as RruleApi;

const CIVIL_DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const CIVIL_DATETIME_RE = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})$/;
const BASIC_DTSTART_RE = /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})$/;
const MS_PER_DAY = 86_400_000;

const BYDAY_LABELS: Record<string, string> = {
  MO: 'Mon',
  TU: 'Tue',
  WE: 'Wed',
  TH: 'Thu',
  FR: 'Fri',
  SA: 'Sat',
  SU: 'Sun',
};

function pad(n: number): string {
  return String(n).padStart(2, '0');
}

/** Formats a UTC-carrier instant's civil date (`YYYY-MM-DD`). */
function formatCivilDate(date: Date): string {
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`;
}

/** True when `date` is a real calendar date in `YYYY-MM-DD` form (component
 *  round-trip rejects rollovers like month 13 — chrono parses strictly, so
 *  the helper must too). */
export function isCivilDateValid(date: string): boolean {
  const m = date.trim().match(CIVIL_DATE_RE);
  if (!m) return false;
  const [, y, mo, d] = m;
  const carrier = new Date(Date.UTC(Number(y), Number(mo) - 1, Number(d)));
  return (
    `${carrier.getUTCFullYear()}` === y &&
    pad(carrier.getUTCMonth() + 1) === mo &&
    pad(carrier.getUTCDate()) === d
  );
}

/** True when `dtstart` is a real floating local datetime in
 *  `YYYY-MM-DDTHH:MM:SS` form (no offset, no `Z`). */
export function isCivilDateTimeValid(dtstart: string): boolean {
  const m = dtstart.trim().match(CIVIL_DATETIME_RE);
  if (!m) return false;
  const [, y, mo, d, h, mi, s] = m;
  const carrier = new Date(
    Date.UTC(Number(y), Number(mo) - 1, Number(d), Number(h), Number(mi), Number(s)),
  );
  return (
    `${carrier.getUTCFullYear()}` === y &&
    pad(carrier.getUTCMonth() + 1) === mo &&
    pad(carrier.getUTCDate()) === d &&
    pad(carrier.getUTCHours()) === h &&
    pad(carrier.getUTCMinutes()) === mi &&
    pad(carrier.getUTCSeconds()) === s
  );
}

/** Converts the blob's basic DTSTART value (`YYYYMMDDTHHMMSS`) to the civil
 *  hyphenated form (`YYYY-MM-DDTHH:MM:SS`); null when malformed. */
function basicToCivil(value: string): string | null {
  const m = value.trim().match(BASIC_DTSTART_RE);
  if (!m) return null;
  const [, y, mo, d, h, mi, s] = m;
  return `${y}-${mo}-${d}T${h}:${mi}:${s}`;
}

/**
 * Parses the two-line recurrence blob (ADR 0004 amendment) — exactly
 * `DTSTART:YYYYMMDDTHHMMSS\nRRULE:<body>`, no trailing extras. Mirrors the
 * Rust `parse_blob`: rejects `EXDATE`/`RDATE`/`EXRULE`/`TZID` anywhere, more
 * than one `RRULE:` line, a `Z` on the DTSTART value, and any other shape.
 * Returns the civil dtstart (`YYYY-MM-DDTHH:MM:SS`) plus the bare RRULE
 * body, or null when the blob does not match the locked format.
 */
export function parseRruleBlob(
  rrule: string,
): { dtstart: string; body: string } | null {
  const blob = rrule.trim();
  const upper = blob.toUpperCase();
  for (const forbidden of ['EXDATE', 'RDATE', 'EXRULE', 'TZID']) {
    if (upper.includes(forbidden)) return null;
  }
  if (upper.split('RRULE:').length - 1 !== 1) return null;
  const lines = blob
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  if (lines.length !== 2) return null;
  const [dtstartLine, rruleLine] = lines;
  if (!dtstartLine.toUpperCase().startsWith('DTSTART:')) return null;
  const value = dtstartLine.slice('DTSTART:'.length).trim();
  // Floating local: the basic form must never carry a Z (or an offset).
  if (value.toUpperCase().includes('Z')) return null;
  const dtstart = basicToCivil(value);
  if (!dtstart) return null;
  if (!rruleLine.toUpperCase().startsWith('RRULE:')) return null;
  const body = rruleLine.slice('RRULE:'.length).trim();
  if (!body) return null;
  return { dtstart, body };
}

/** Composes the two-line blob from a civil dtstart (`YYYY-MM-DDTHH:MM:SS`)
 *  and a bare RRULE body — the exact shape the API stores. */
export function composeRruleBlob(dtstart: string, body: string): string {
  const compact = dtstart.replaceAll('-', '').replaceAll(':', '');
  return `DTSTART:${compact}\nRRULE:${body.trim()}`;
}

function parseCivilDateCarrier(date: string): Date | null {
  const m = date.trim().match(CIVIL_DATE_RE);
  if (!m || !isCivilDateValid(date)) return null;
  return new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
}

function parseCivilDateTimeCarrier(dtstart: string): Date | null {
  const m = dtstart.trim().match(CIVIL_DATETIME_RE);
  if (!m || !isCivilDateTimeValid(dtstart)) return null;
  return new Date(
    Date.UTC(
      Number(m[1]),
      Number(m[2]) - 1,
      Number(m[3]),
      Number(m[4]),
      Number(m[5]),
      Number(m[6]),
    ),
  );
}

/** The stored body must be the bare property value — never a content line
 *  (mirrors `rule_body` on the Rust side, which 400s these). */
function ruleBody(rrule: string): string | null {
  const body = rrule.trim();
  const upper = body.toUpperCase();
  if (!body || upper.startsWith('RRULE:') || upper.startsWith('DTSTART')) {
    return null;
  }
  return body;
}

/** Builds the VTEXT the npm parser needs: the floating civil dtstart
 *  expressed as a Z-form UTC line (the carrier trick) followed by the bare
 *  RRULE body. Throws exactly what `rrulestr` throws on invalid input. */
function toRuleText(rruleBody: string, dtstartCarrier: Date): string {
  const z =
    `DTSTART:${dtstartCarrier.getUTCFullYear()}${pad(dtstartCarrier.getUTCMonth() + 1)}` +
    `${pad(dtstartCarrier.getUTCDate())}T${pad(dtstartCarrier.getUTCHours())}` +
    `${pad(dtstartCarrier.getUTCMinutes())}${pad(dtstartCarrier.getUTCSeconds())}Z`;
  return `${z}\n${rruleBody}`;
}

/**
 * Expands a recurrence blob into the local civil dates it covers in the
 * inclusive window `[from, to]` (`YYYY-MM-DD`, both ends included). Dates
 * come back ascending and deduplicated.
 *
 * Mirrors Rust `occurrence_dates`: window bounds are widened by a day on each
 * side before hitting the engine so edge inclusivity never depends on the
 * library's boundary conventions; occurrences are then filtered to the exact
 * civil-date window.
 *
 * Returns [] on any malformed input (bad dates, unparsable blob) — callers
 * gate saving with `isValidRruleBody`; the server remains the validation
 * authority either way.
 */
export function occurrenceDates(args: {
  rrule: string; // the two-line blob
  from: string; // YYYY-MM-DD inclusive
  to: string; // YYYY-MM-DD inclusive
}): string[] {
  const from = args.from.trim();
  const to = args.to.trim();
  const fromCarrier = parseCivilDateCarrier(from);
  const toCarrier = parseCivilDateCarrier(to);
  if (!fromCarrier || !toCarrier || fromCarrier.getTime() > toCarrier.getTime()) {
    return [];
  }
  const parsed = parseRruleBlob(args.rrule);
  if (!parsed) return [];
  const dtstartCarrier = parseCivilDateTimeCarrier(parsed.dtstart);
  if (!dtstartCarrier) return [];
  const body = ruleBody(parsed.body);
  if (!body) return [];

  let dates: Date[];
  try {
    const set = rrule.rrulestr(toRuleText(body, dtstartCarrier), { forceset: true });
    // Widen ±1 day (same as the Rust side), then filter to the exact window
    // below. `between` terminates at its bound, so infinite rules are safe.
    dates = set.between(
      new Date(fromCarrier.getTime() - MS_PER_DAY),
      new Date(toCarrier.getTime() + MS_PER_DAY),
      true,
    );
  } catch {
    return [];
  }

  const out: string[] = [];
  for (const date of dates) {
    // The carrier tz is UTC, so the UTC view IS the floating civil time.
    const day = formatCivilDate(date);
    if (day < from || day > to) continue;
    if (!out.includes(day)) out.push(day);
  }
  return out;
}

/** Create/update gate mirroring Rust `validate_recurrence`: the blob must
 *  parse as a valid recurrence. UX only — the server re-validates and 400s
 *  authoritatively. */
export function isValidRruleBody(blob: string): boolean {
  const parsed = parseRruleBlob(blob);
  if (!parsed) return false;
  const dtstartCarrier = parseCivilDateTimeCarrier(parsed.dtstart);
  if (!dtstartCarrier) return false;
  try {
    rrule.rrulestr(toRuleText(parsed.body, dtstartCarrier), { forceset: true });
    return true;
  } catch {
    return false;
  }
}

/** The viewer's civil today (`YYYY-MM-DD`) via BROWSER-LOCAL components.
 *  Only ever used as the anchor of preview windows — never for membership
 *  (expansion itself stays UTC-carried). */
export function civilToday(): string {
  const now = new Date();
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/** Pure civil arithmetic: `YYYY-MM-DD` shifted by whole days (UTC math, no
 *  DST involvement). Returns the input unchanged when malformed. */
export function addCivilDays(date: string, days: number): string {
  const carrier = parseCivilDateCarrier(date);
  if (!carrier) return date;
  return formatCivilDate(new Date(carrier.getTime() + days * MS_PER_DAY));
}

/** Strips ordinal prefixes from BYDAY tokens (`-1MO` → `MO`). */
function byDayLabels(raw: string | undefined): string {
  if (!raw) return '';
  const labels: string[] = [];
  for (const token of raw.split(',')) {
    const code = token.trim().replace(/^[+-]?\d{1,2}/, '').toUpperCase();
    const label = BYDAY_LABELS[code];
    if (!label) return ''; // unknown token → caller falls back to no days
    labels.push(label);
  }
  return labels.join(', ');
}

/** Parses an RRULE body into its `KEY=VALUE` parts (keys uppercased, values
 *  trimmed). Chunks without an `=` are skipped. The editor uses this to
 *  prefill its builder from a stored rule and to decide whether a rule is
 *  rebuildable or must fall back to the raw override. */
export function rruleParts(rrule: string): Map<string, string> {
  const parts = new Map<string, string>();
  for (const chunk of rrule.trim().split(';')) {
    const eq = chunk.indexOf('=');
    if (eq <= 0) continue;
    parts.set(
      chunk.slice(0, eq).trim().toUpperCase(),
      chunk.slice(eq + 1).trim(),
    );
  }
  return parts;
}

/** True when UNTIL's clock (HHMMSS, optional trailing Z) equals the civil
 *  dtstart time (`YYYY-MM-DDTHH:MM:SS`). Date-only UNTIL is not a match. */
export function untilMatchesDtstartTime(until: string, dtstart: string): boolean {
  const untilTime = until.trim().toUpperCase().replace(/Z$/, '');
  const t = untilTime.indexOf('T');
  if (t < 0) return false;
  return untilTime.slice(t + 1) === dtstart.slice(11, 19).replaceAll(':', '');
}

/**
 * Human summary of a recurrence blob for list rows ("Daily", "Weekly on Mon,
 * Wed"). Unknown/malformed blobs degrade to the raw stored string so power
 * users still see what is there.
 */
export function rruleSummary(rrule: string): string {
  const parsed = parseRruleBlob(rrule);
  if (!parsed) return rrule.trim() || 'No repeat rule';
  const raw = parsed.body.trim();
  const parts = rruleParts(raw);
  const freq = parts.get('FREQ')?.toUpperCase();
  const intervalRaw = parts.get('INTERVAL');
  const interval = intervalRaw ? Number.parseInt(intervalRaw, 10) : NaN;
  const n = Number.isFinite(interval) && interval > 1 ? interval : null;

  let base: string | null = null;
  if (freq === 'DAILY') {
    base = n ? `Every ${n} days` : 'Daily';
  } else if (freq === 'WEEKLY') {
    base = n ? `Every ${n} weeks` : 'Weekly';
    const days = byDayLabels(parts.get('BYDAY'));
    if (days) base += ` on ${days}`;
  } else if (freq === 'MONTHLY') {
    base = n ? `Every ${n} months` : 'Monthly';
    const monthdays = parts.get('BYMONTHDAY');
    if (monthdays) base += ` on day ${monthdays}`;
  } else if (freq === 'YEARLY') {
    base = n ? `Every ${n} years` : 'Yearly';
  }
  if (base === null) return raw;

  const until = parts.get('UNTIL')?.toUpperCase();
  if (until) {
    const m = until.match(/^(\d{4})(\d{2})(\d{2})/);
    if (m) base += ` until ${m[1]}-${m[2]}-${m[3]}`;
  } else {
    const count = parts.get('COUNT');
    if (count) base += ` · ${count} times`;
  }
  return base;
}

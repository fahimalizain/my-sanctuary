// RRULE expansion + summaries for /routines (ADR 0004 § Recurrence).
//
// The npm `rrule` engine must produce the SAME local civil dates as the Rust
// crate (`packages/api-core/src/routines.rs::occurrence_dates`) — golden
// fixtures (`packages/api-core/fixtures/rrule_golden.json`) are asserted on
// BOTH engines so they cannot drift.
//
// Floating local civil time, always: a stored `dtstart`
// (`YYYY-MM-DDTHH:MM:SS`, no offset) is attached to UTC purely as a carrier
// when handed to npm `rrule`, and results are read back off the UTC
// components. The browser's local timezone is NEVER consulted for membership
// — same UTC-carrier trick as the Rust side.
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
 *  `YYYY-MM-DDTHH:MM:SS` form (no offset, no `Z` — the storage format). */
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
 * Expands a recurrence into the local civil dates it covers in the inclusive
 * window `[from, to]` (`YYYY-MM-DD`, both ends included), minus `exdates`
 * (local-date exclusion). Dates come back ascending and deduplicated.
 *
 * Mirrors Rust `occurrence_dates`: window bounds are widened by a day on each
 * side before hitting the engine so edge inclusivity never depends on the
 * library's boundary conventions; occurrences are then filtered to the exact
 * civil-date window. Exdates apply AFTER expansion (a COUNT=5 series minus an
 * exdate still counts 5 internally), matching the fixtures.
 *
 * Returns [] on any malformed input (bad dates, bad dtstart, unparsable rule)
 * — callers gate saving with `isValidRruleBody`; the server remains the
 * validation authority either way.
 */
export function occurrenceDates(args: {
  dtstart: string; // YYYY-MM-DDTHH:MM:SS
  rrule: string; // body only
  exdates: string[];
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
  const dtstartCarrier = parseCivilDateTimeCarrier(args.dtstart);
  if (!dtstartCarrier) return [];
  const body = ruleBody(args.rrule);
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

  const exdates = new Set(args.exdates.map((date) => date.trim()));
  const out: string[] = [];
  for (const date of dates) {
    // The carrier tz is UTC, so the UTC view IS the floating civil time.
    const day = formatCivilDate(date);
    if (day < from || day > to) continue;
    if (exdates.has(day)) continue;
    if (!out.includes(day)) out.push(day);
  }
  return out;
}

/** Create/update gate mirroring Rust `validate_recurrence`: both halves of
 *  the recurrence pair must parse together. UX only — the server re-validates
 *  and 400s authoritatively. */
export function isValidRruleBody(rule: string, dtstart: string): boolean {
  const body = ruleBody(rule);
  if (!body) return false;
  const dtstartCarrier = parseCivilDateTimeCarrier(dtstart);
  if (!dtstartCarrier) return false;
  try {
rrule.rrulestr(toRuleText(body, dtstartCarrier), { forceset: true });
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

/**
 * Human summary of an RRULE BODY for list rows ("Daily", "Weekly on Mon,
 * Wed"). Unknown/malformed rules degrade to the raw stored string so power
 * users still see what is there.
 */
export function rruleSummary(rrule: string): string {
  const raw = rrule.trim();
  if (!raw) return 'No repeat rule';
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

// Golden-fixture gate (ADR 0004 § Recurrence): every case in
// `packages/api-core/fixtures/rrule_golden.json` must expand to the same
// local civil dates on the npm `rrule` engine as on the Rust crate. The Rust
// suite owns those numbers — if a case fails here, fix THIS helper, never the
// fixture.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import {
  addCivilDays,
  civilToday,
  composeRruleBlob,
  isCivilDateValid,
  isCivilDateTimeValid,
  isValidRruleBody,
  occurrenceDates,
  parseRruleBlob,
  rruleSummary,
  untilMatchesDtstartTime,
} from './rrule-preview';

interface GoldenCase {
  name: string;
  rrule: string;
  from: string;
  to: string;
  expected: string[];
}

interface GoldenFixture {
  comment: string;
  cases: GoldenCase[];
}

// Repo-relative load from this module's URL:
// apps/web/app/modules/routines → repo root is five levels up.
const fixturePath = fileURLToPath(
  new URL(
    '../../../../../packages/api-core/fixtures/rrule_golden.json',
    import.meta.url,
  ),
);
const fixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as GoldenFixture;

test('golden fixture loads with cases', () => {
  assert.ok(fixture.cases.length > 0, 'fixture must not be empty');
});

for (const c of fixture.cases) {
  test(`golden: ${c.name}`, () => {
    const got = occurrenceDates({
      rrule: c.rrule,
      from: c.from,
      to: c.to,
    });
    assert.deepEqual(got, c.expected);
  });
}

// ── Helper semantics beyond the goldens ───────────────────────────────────

test('occurrenceDates returns [] on malformed input', () => {
  const base = {
    rrule: 'DTSTART:20260101T063000\nRRULE:FREQ=DAILY',
    from: '2026-01-01',
    to: '2026-01-07',
  };
  // Bad blobs (the API 400s these too).
  for (const rrule of [
    '',
    'FREQ=DAILY', // missing the DTSTART line
    'DTSTART:20260101T063000', // missing the RRULE line
    'DTSTART:20260101T063000Z\nRRULE:FREQ=DAILY', // Z on DTSTART
    'DTSTART:2026-01-01T06:30:00\nRRULE:FREQ=DAILY', // not the basic form
    'DTSTART:20260101T063000\nRRULE:FREQ=DAILY;EXDATE:20260102', // exdate
    'DTSTART:20260101T063000\nRRULE:FREQ=DAILY\nEXTRA', // third line
    'DTSTART:20260101T063000\nRRULE:NOT_A_RULE=1',
    'DTSTART:20260101T063000\nRRULE:',
  ]) {
    assert.deepEqual(occurrenceDates({ ...base, rrule }), [], `rrule ${rrule}`);
  }
  // Impossible calendar dates must roll over nowhere.
  assert.deepEqual(
    occurrenceDates({ ...base, rrule: 'DTSTART:20261301T063000\nRRULE:FREQ=DAILY' }),
    [],
  );
  assert.deepEqual(
    occurrenceDates({ ...base, rrule: 'DTSTART:20260230T063000\nRRULE:FREQ=DAILY' }),
    [],
  );
  // Bad window.
  assert.deepEqual(occurrenceDates({ ...base, from: 'not-a-date' }), []);
  assert.deepEqual(occurrenceDates({ ...base, to: '2026/01/07' }), []);
  assert.deepEqual(
    occurrenceDates({ ...base, from: '2026-01-07', to: '2026-01-01' }),
    [],
    'from after to',
  );
});

test('occurrenceDates is sorted and unique regardless of engine output', () => {
  const got = occurrenceDates({
    rrule: 'DTSTART:20260101T000000\nRRULE:FREQ=HOURLY;INTERVAL=12',
    from: '2026-01-01',
    to: '2026-01-05',
  });
  // Two occurrences land on every day (00:00 and 12:00) — dedupe to dates.
  assert.deepEqual(got, [
    '2026-01-01',
    '2026-01-02',
    '2026-01-03',
    '2026-01-04',
    '2026-01-05',
  ]);
});

test('window edges are inclusive at the exact time-of-day boundary', () => {
  const got = occurrenceDates({
    rrule: 'DTSTART:20260101T063000\nRRULE:FREQ=DAILY;UNTIL=20260104T063000Z',
    from: '2026-01-04',
    to: '2026-01-04',
  });
  assert.deepEqual(got, ['2026-01-04'], 'single-day window still catches the rule');
});

test('isCivilDate / isCivilDateTime validators', () => {
  assert.equal(isCivilDateValid('2026-02-29'), false, '2026 is not a leap year');
  assert.equal(isCivilDateValid('2028-02-29'), true);
  assert.equal(isCivilDateValid('2026-1-01'), false, 'components must be padded');
  assert.equal(isCivilDateTimeValid('2026-01-01T24:00:00'), false);
  assert.equal(isCivilDateTimeValid('2026-01-01T06:60:00'), false);
  assert.equal(isCivilDateTimeValid(' 2026-01-01T06:30:00 '), true, 'trim tolerated');
});

test('parseRruleBlob splits the locked two-line format', () => {
  assert.deepEqual(
    parseRruleBlob('DTSTART:20260105T063000\nRRULE:FREQ=WEEKLY;BYDAY=MO'),
    { dtstart: '2026-01-05T06:30:00', body: 'FREQ=WEEKLY;BYDAY=MO' },
  );
  // A trailing newline is trimmed away — still exactly two lines.
  assert.deepEqual(
    parseRruleBlob('DTSTART:20260105T063000\nRRULE:FREQ=DAILY\n'),
    { dtstart: '2026-01-05T06:30:00', body: 'FREQ=DAILY' },
  );
  for (const bad of [
    '',
    'FREQ=DAILY',
    'DTSTART:20260105T063000',
    'DTSTART:20260105T063000Z\nRRULE:FREQ=DAILY',
    'DTSTART;TZID=Asia/Kolkata:20260105T063000\nRRULE:FREQ=DAILY',
    'DTSTART:20260105T063000\nRRULE:FREQ=DAILY\nRRULE:FREQ=WEEKLY',
    'DTSTART:20260105T063000\nRRULE:FREQ=DAILY;RDATE:20260106',
  ]) {
    assert.equal(parseRruleBlob(bad), null, `bad blob: ${bad}`);
  }
});

test('composeRruleBlob round-trips through parseRruleBlob', () => {
  const blob = composeRruleBlob('2026-01-05T06:30:00', 'FREQ=WEEKLY;BYDAY=MO');
  assert.equal(blob, 'DTSTART:20260105T063000\nRRULE:FREQ=WEEKLY;BYDAY=MO');
  assert.deepEqual(parseRruleBlob(blob), {
    dtstart: '2026-01-05T06:30:00',
    body: 'FREQ=WEEKLY;BYDAY=MO',
  });
});

test('isValidRruleBody mirrors the server gate', () => {
  assert.equal(isValidRruleBody('DTSTART:20260101T063000\nRRULE:FREQ=DAILY'), true);
  assert.equal(
    isValidRruleBody('DTSTART:20260101T063000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;INTERVAL=2'),
    true,
  );
  assert.equal(
    isValidRruleBody('DTSTART:20260101T063000\nRRULE:FREQ=DAILY;UNTIL=20260104T063000Z'),
    true,
  );
  assert.equal(isValidRruleBody(''), false);
  assert.equal(isValidRruleBody('FREQ=DAILY'), false);
  assert.equal(isValidRruleBody('DTSTART:20260101T063000\nRRULE:FREQ=BOGUS'), false);
  assert.equal(isValidRruleBody('DTSTART:20260101T063000Z\nRRULE:FREQ=DAILY'), false);
});

// ── rruleSummary ──────────────────────────────────────────────────────────

test('rruleSummary renders the common bodies', () => {
  const blob = (body: string) => `DTSTART:20260101T063000\nRRULE:${body}`;
  assert.equal(rruleSummary(blob('FREQ=DAILY')), 'Daily');
  assert.equal(rruleSummary(blob('FREQ=DAILY;INTERVAL=2')), 'Every 2 days');
  assert.equal(rruleSummary(blob('FREQ=WEEKLY;BYDAY=MO,WE')), 'Weekly on Mon, Wed');
  assert.equal(rruleSummary(blob('FREQ=WEEKLY;INTERVAL=2;BYDAY=TU')), 'Every 2 weeks on Tue');
  assert.equal(
    rruleSummary(blob('freq=weekly;byday=mo')),
    'Weekly on Mon',
    'case-insensitive keys/values',
  );
  assert.equal(rruleSummary(blob('FREQ=WEEKLY')), 'Weekly');
  assert.equal(rruleSummary(blob('FREQ=MONTHLY;BYMONTHDAY=1,15')), 'Monthly on day 1,15');
  assert.equal(rruleSummary(blob('FREQ=MONTHLY')), 'Monthly');
  assert.equal(rruleSummary(blob('FREQ=YEARLY')), 'Yearly');
  assert.equal(
    rruleSummary(blob('FREQ=DAILY;UNTIL=20260104T063000Z')),
    'Daily until 2026-01-04',
  );
  assert.equal(rruleSummary(blob('FREQ=DAILY;COUNT=5')), 'Daily · 5 times');
  assert.equal(
    rruleSummary(blob('FREQ=MINUTELY;INTERVAL=30')),
    'FREQ=MINUTELY;INTERVAL=30',
    'unknown freq degrades to raw body',
  );
  assert.equal(rruleSummary(''), 'No repeat rule');
  // A malformed blob degrades to the raw stored string.
  assert.equal(rruleSummary('FREQ=DAILY'), 'FREQ=DAILY');
});

// ── untilMatchesDtstartTime (editor prefill) ──────────────────────────────

test('untilMatchesDtstartTime matches the builder round-trip (Z-form)', () => {
  // Save emits `UNTIL=${date}T${hhmm}00Z`; the stored rule must reopen in the
  // builder, not in raw-override mode.
  assert.equal(
    untilMatchesDtstartTime('20260104T063000Z', '2026-01-05T06:30:00'),
    true,
  );
});

test('untilMatchesDtstartTime matches without the trailing Z', () => {
  assert.equal(
    untilMatchesDtstartTime('20260104T063000', '2026-01-05T06:30:00'),
    true,
  );
});

test('untilMatchesDtstartTime rejects a different clock time', () => {
  assert.equal(
    untilMatchesDtstartTime('20260104T070000Z', '2026-01-05T06:30:00'),
    false,
  );
});

test('untilMatchesDtstartTime rejects date-only UNTIL', () => {
  assert.equal(untilMatchesDtstartTime('20260104', '2026-01-05T06:30:00'), false);
});

test('untilMatchesDtstartTime never falls into the colon-form trap', () => {
  // The pre-fix prefill compared `UNTIL.slice(9, 15)` ("063000") against
  // `dtstart.slice(11, 16) + "00"` — that splices the colon mid-number into
  // "06:3000", which can never equal the compact form. The helper strips the
  // Z, keeps the compact HHMMSS and strips the dtstart's colons instead, so
  // both sides are compact.
  const trap = `${'2026-01-05T06:30:00'.slice(11, 16)}00`; // the old dtTime
  assert.equal(trap, '06:3000');
  assert.notEqual(trap, '063000', 'the broken comparison can never match');
  assert.equal(
    untilMatchesDtstartTime('20260104T063000Z', '2026-01-05T06:30:00'),
    true,
    'compact form on both sides matches',
  );
});

// ── Civil date arithmetic ─────────────────────────────────────────────────

test('civilToday + addCivilDays do pure calendar math', () => {
  assert.match(civilToday(), /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(addCivilDays('2026-01-31', 1), '2026-02-01', 'month rollover');
  assert.equal(addCivilDays('2026-03-01', -1), '2026-02-28', 'negative shift');
  assert.equal(addCivilDays('bogus', 3), 'bogus', 'malformed input returned unchanged');
});
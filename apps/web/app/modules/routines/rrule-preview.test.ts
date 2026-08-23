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
  isCivilDateValid,
  isCivilDateTimeValid,
  isValidRruleBody,
  occurrenceDates,
  rruleSummary,
} from './rrule-preview';

interface GoldenCase {
  name: string;
  dtstart: string;
  rrule: string;
  exdates: string[];
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
      dtstart: c.dtstart,
      rrule: c.rrule,
      exdates: c.exdates,
      from: c.from,
      to: c.to,
    });
    assert.deepEqual(got, c.expected);
  });
}

// ── Helper semantics beyond the goldens ───────────────────────────────────

test('occurrenceDates returns [] on malformed input', () => {
  const base = {
    dtstart: '2026-01-01T06:30:00',
    rrule: 'FREQ=DAILY',
    exdates: [] as string[],
    from: '2026-01-01',
    to: '2026-01-07',
  };
  // Bad dtstart forms (the API 400s these too).
  for (const dtstart of ['', '2026-01-01', '2026-01-01T06:30', '2026-01-01T06:30:00Z']) {
    assert.deepEqual(occurrenceDates({ ...base, dtstart }), [], `dtstart ${dtstart}`);
  }
  // Impossible calendar dates must roll over nowhere.
  assert.deepEqual(occurrenceDates({ ...base, dtstart: '2026-13-01T06:30:00' }), []);
  assert.deepEqual(occurrenceDates({ ...base, dtstart: '2026-02-30T06:30:00' }), []);
  // Bad window.
  assert.deepEqual(occurrenceDates({ ...base, from: 'not-a-date' }), []);
  assert.deepEqual(occurrenceDates({ ...base, to: '2026/01/07' }), []);
  assert.deepEqual(
    occurrenceDates({ ...base, from: '2026-01-07', to: '2026-01-01' }),
    [],
    'from after to',
  );
  // Bad rules.
  assert.deepEqual(occurrenceDates({ ...base, rrule: '' }), []);
  assert.deepEqual(occurrenceDates({ ...base, rrule: 'RRULE:FREQ=DAILY' }), []);
  assert.deepEqual(occurrenceDates({ ...base, rrule: 'NOT_A_RULE=1' }), []);
});

test('occurrenceDates is sorted and unique regardless of engine output', () => {
  const got = occurrenceDates({
    dtstart: '2026-01-01T00:00:00',
    rrule: 'FREQ=HOURLY;INTERVAL=12',
    exdates: [],
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
    dtstart: '2026-01-01T06:30:00',
    rrule: 'FREQ=DAILY;UNTIL=20260104T063000Z',
    exdates: [],
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

test('isValidRruleBody mirrors the server gate', () => {
  const ok = '2026-01-01T06:30:00';
  assert.equal(isValidRruleBody('FREQ=DAILY', ok), true);
  assert.equal(isValidRruleBody('FREQ=WEEKLY;BYDAY=MO,WE;INTERVAL=2', ok), true);
  assert.equal(isValidRruleBody('FREQ=DAILY;UNTIL=20260104T063000Z', ok), true);
  assert.equal(isValidRruleBody('', ok), false);
  assert.equal(isValidRruleBody('RRULE:FREQ=DAILY', ok), false);
  assert.equal(isValidRruleBody('DTSTART:20260101T000000Z\nFREQ=DAILY', ok), false);
  assert.equal(isValidRruleBody('FREQ=DAILY', '2026-01-01T06:30'), false, 'bad dtstart');
  assert.equal(isValidRruleBody('FREQ=BOGUS', ok), false);
});

// ── rruleSummary ──────────────────────────────────────────────────────────

test('rruleSummary renders the common bodies', () => {
  assert.equal(rruleSummary('FREQ=DAILY'), 'Daily');
  assert.equal(rruleSummary('FREQ=DAILY;INTERVAL=2'), 'Every 2 days');
  assert.equal(rruleSummary('FREQ=WEEKLY;BYDAY=MO,WE'), 'Weekly on Mon, Wed');
  assert.equal(rruleSummary('FREQ=WEEKLY;INTERVAL=2;BYDAY=TU'), 'Every 2 weeks on Tue');
  assert.equal(rruleSummary('freq=weekly;byday=mo'), 'Weekly on Mon', 'case-insensitive keys/values');
  assert.equal(rruleSummary('FREQ=WEEKLY'), 'Weekly');
  assert.equal(rruleSummary('FREQ=MONTHLY;BYMONTHDAY=1,15'), 'Monthly on day 1,15');
  assert.equal(rruleSummary('FREQ=MONTHLY'), 'Monthly');
  assert.equal(rruleSummary('FREQ=YEARLY'), 'Yearly');
  assert.equal(
    rruleSummary('FREQ=DAILY;UNTIL=20260104T063000Z'),
    'Daily until 2026-01-04',
  );
  assert.equal(rruleSummary('FREQ=DAILY;COUNT=5'), 'Daily · 5 times');
  assert.equal(rruleSummary('FREQ=MINUTELY;INTERVAL=30'), 'FREQ=MINUTELY;INTERVAL=30', 'unknown freq degrades to raw');
  assert.equal(rruleSummary(''), 'No repeat rule');
});

// ── Civil date arithmetic ─────────────────────────────────────────────────

test('civilToday + addCivilDays do pure calendar math', () => {
  assert.match(civilToday(), /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(addCivilDays('2026-01-31', 1), '2026-02-01', 'month rollover');
  assert.equal(addCivilDays('2026-03-01', -1), '2026-02-28', 'negative shift');
  assert.equal(addCivilDays('bogus', 3), 'bogus', 'malformed input returned unchanged');
});

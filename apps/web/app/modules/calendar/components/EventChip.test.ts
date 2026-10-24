// Mount tests for the EventChip time detail. A non-compact chip's second line
// is the same "start – end · duration" string the drag ghost paints; a compact
// chip stays title + start time and keeps the full detail in its tooltip.
// DOM must exist before the component module is imported — installDom() then
// dynamic import (see EventInspector.test.ts).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import type { CalendarEvent } from '@/app/types';
import { CHIP_MIN_H } from '../lib/week-layout';
import { notionEventTheme } from '../lib/event-colors';
import type { PositionedEvent } from './EventChip';
import { installDom } from './install-dom';

installDom();

let EventChip: typeof import('./EventChip').EventChip;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  screen = rtl.screen;
  const mod = await import('./EventChip');
  EventChip = mod.EventChip;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

/** Local instants → ISO, so `new Date(event.start_time)` round-trips. */
function makeEvent(title: string, start: Date, end: Date): CalendarEvent {
  return {
    id: 'e1',
    calendar_id: 'cal-1',
    google_event_id: 'g-1',
    title,
    description: '',
    start_time: start.toISOString(),
    end_time: end.toISOString(),
    last_synced_at: '2026-09-08T00:00:00.000Z',
  };
}

function makePositioned(
  event: CalendarEvent,
  startMin: number,
  endMin: number,
  height: number,
): PositionedEvent {
  return {
    event,
    startMin,
    endMin,
    top: 0,
    height,
    leftPercent: 0,
    widthPercent: 1,
    layerIndex: 0,
    leftPixels: 0,
    color: '#2a5c8a',
  };
}

/** Render an unselected chip (no touch handles) and return its root. */
function mount(
  positioned: PositionedEvent,
  extra: { past?: boolean; unsaved?: boolean } = {},
): HTMLElement {
  render(
    createElement(EventChip, {
      positioned,
      selected: false,
      onSelect: () => {},
      ...extra,
    }),
  );
  return screen.getByRole('button');
}

const RANGE_DETAIL = '5 AM – 6:30 AM · 1h 30min';

test('non-compact chip: second line is the range · duration detail', () => {
  const event = makeEvent(
    'Deep work',
    new Date(2026, 8, 13, 5, 0),
    new Date(2026, 8, 13, 6, 30),
  );
  const chip = mount(makePositioned(event, 5 * 60, 6 * 60 + 30, 48));

  assert.ok(
    (chip.textContent ?? '').includes(RANGE_DETAIL),
    'chip text carries the detail',
  );

  // Stacked layout: title and detail are sibling divs in the chip body, not
  // the single flex row a compact chip uses.
  const titleNode = screen.getByText('Deep work', { selector: 'div' });
  const detailNode = screen.getByText(RANGE_DETAIL, { selector: 'div' });
  assert.equal(titleNode.tagName, 'DIV');
  assert.equal(detailNode.tagName, 'DIV');
  assert.equal(detailNode.parentElement, titleNode.parentElement);
  assert.equal(titleNode.nextElementSibling, detailNode);
  assert.ok(
    !(detailNode.parentElement as HTMLElement).className.includes('flex'),
    'the detail line is not inside the compact flex row',
  );

  assert.equal(chip.getAttribute('title'), `Deep work · ${RANGE_DETAIL}`);
});

test('compact chip: title + start time only; tooltip has the full detail', () => {
  assert.equal(CHIP_MIN_H, 18, 'compact case uses the min chip height');
  const event = makeEvent(
    'Standup',
    new Date(2026, 8, 13, 5, 15),
    new Date(2026, 8, 13, 5, 30),
  );
  const chip = mount(
    makePositioned(event, 5 * 60 + 15, 5 * 60 + 30, CHIP_MIN_H),
  );

  const visible = chip.textContent ?? '';
  assert.ok(visible.includes('Standup'), 'title stays visible');
  assert.ok(visible.includes('5:15 AM'), 'start time stays visible');
  assert.ok(!visible.includes('5:30'), 'end time is not shown');
  assert.ok(!visible.includes('15min'), 'duration is not shown');
  assert.ok(!visible.includes('·'), 'no detail separator in the compact row');

  assert.equal(
    chip.getAttribute('title'),
    'Standup · 5:15 AM – 5:30 AM · 15min',
  );
});

// ── washed Notion tones ─────────────────────────────────────────────────

/** A red fixture no Google→Notion palette would produce for #d50000. */
function redChip(color = '#d50000'): HTMLElement {
  const positioned = makePositioned(
    makeEvent('Red', new Date(2026, 8, 13, 5, 0), new Date(2026, 8, 13, 6, 0)),
    5 * 60,
    6 * 60,
    48,
  );
  positioned.color = color;
  return mount(positioned);
}

test('chip derives its dark fill from its own hex, not a palette remap', () => {
  const chip = redChip('#d50000');
  const { dark } = notionEventTheme('#d50000');

  assert.equal(
    chip.style.getPropertyValue('--ev-fill-dark'),
    dark.backgroundSecondary,
  );
  assert.notEqual(
    chip.style.getPropertyValue('--ev-fill-dark'),
    'rgb(108, 41, 32)',
    'the ED201D fixture theme must not be substituted',
  );
  assert.equal(
    chip.style.getPropertyValue('--ev-accent-dark'),
    dark.backgroundPrimary,
  );
});

test('clipped body takes the washed fill; the raw hex is not a background', () => {
  const chip = redChip('#d50000');
  const body = chip.firstElementChild as HTMLElement;

  assert.equal(body.style.backgroundColor, '', 'no inline raw fill');
  assert.ok(
    body.className.includes('bg-[var(--ev-fill)]'),
    'body paints the washed light fill',
  );
  assert.ok(
    body.className.includes('dark:bg-[var(--ev-fill-dark)]'),
    'body paints the washed dark fill',
  );
});

test('past sets data-past on the root', () => {
  const positioned = makePositioned(
    makeEvent('Past', new Date(2026, 8, 13, 5, 0), new Date(2026, 8, 13, 6, 0)),
    5 * 60,
    6 * 60,
    48,
  );
  const past = mount(positioned, { past: true });
  assert.ok(past.hasAttribute('data-past'));
  assert.equal(past.getAttribute('data-past'), '');
});

test('omitting past leaves data-past off the root', () => {
  const positioned = makePositioned(
    makeEvent('Now', new Date(2026, 8, 13, 5, 0), new Date(2026, 8, 13, 6, 0)),
    5 * 60,
    6 * 60,
    48,
  );
  assert.equal(mount(positioned).hasAttribute('data-past'), false);
});

// ── unsaved creates ─────────────────────────────────────────────────────

test('an unsaved chip is dashed and marked; a saved chip is not', () => {
  const positioned = makePositioned(
    makeEvent(
      'Not saved',
      new Date(2026, 8, 13, 5, 0),
      new Date(2026, 8, 13, 6, 0),
    ),
    5 * 60,
    6 * 60,
    48,
  );
  const unsaved = mount(positioned, { unsaved: true });
  assert.equal(unsaved.getAttribute('data-unsaved'), '');
  assert.ok(
    unsaved.className.includes('[outline-style:dashed]'),
    'the chip reads as local-only',
  );
  assert.ok(
    !unsaved.className.includes('[outline-style:solid]'),
    'never both outline styles',
  );

  cleanup();
  const saved = mount(positioned);
  assert.equal(saved.hasAttribute('data-unsaved'), false);
  assert.ok(saved.className.includes('[outline-style:solid]'));
});

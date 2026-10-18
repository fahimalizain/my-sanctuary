// Mount tests for QuotesSection (Daily Inspiration strip + full-quote dialog).
// DOM must exist before the component module is imported — installDom() then
// dynamic import.

import { after, afterEach, before, mock, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ComponentType } from 'react';
import { installDom } from '../modules/calendar/components/install-dom';

installDom();

type Quote = { text: string; author: string };

type QuotesSectionProps = {
  quotes: Quote[];
  className?: string;
};

// Inline fixtures (never the real catalog). The first quote is a long sentence
// so the dialog assertion can only pass with the whole string.
const QUOTES: Quote[] = [
  {
    text: 'We are what we repeatedly do. Excellence, then, is not an act but a habit; it is the steady accumulation of small choices made well, day after day, long after the first enthusiasm has faded and only the practice is left to carry us.',
    author: 'Will Durant',
  },
  {
    text: 'Discipline is choosing between what you want now and what you want most.',
    author: 'Abraham Lincoln',
  },
  {
    text: 'The obstacle is the way.',
    author: 'Marcus Aurelius',
  },
];

let QuotesSection: ComponentType<QuotesSectionProps>;
let QUOTE_ADVANCE_MS: number;
let act: typeof import('@testing-library/react').act;
let cleanup: () => void;
let fireEvent: typeof import('@testing-library/react').fireEvent;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;

before(async () => {
  const rtl = await import('@testing-library/react');
  act = rtl.act;
  cleanup = rtl.cleanup;
  fireEvent = rtl.fireEvent;
  render = rtl.render;
  screen = rtl.screen;
  const mod = await import('./QuotesSection');
  QuotesSection = mod.QuotesSection;
  QUOTE_ADVANCE_MS = mod.QUOTE_ADVANCE_MS;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

function mount(quotes: Quote[] = QUOTES) {
  render(createElement(QuotesSection, { quotes }));
}

/**
 * The strip lives in an `aria-hidden` subtree while the modal dialog is open
 * (Radix hides its siblings), so role queries cannot reach it. Query the
 * attribute instead.
 */
function stripButton(): HTMLElement {
  const button = document.querySelector<HTMLElement>(
    '[aria-label="Read full quote"]',
  );
  assert.ok(button, 'the strip quote button must be rendered');
  return button;
}

function stripText(): string {
  return stripButton().textContent ?? '';
}

function dialogText(): string {
  return screen.getByRole('dialog').textContent ?? '';
}

/**
 * Presence is checked as a boolean on purpose: a failing `assert.equal(el,
 * null)` builds its message by inspecting the jsdom element, which burns tens
 * of seconds of CPU and hides the assertion message.
 */
function isDialogOpen(): boolean {
  return screen.queryByRole('dialog') !== null;
}

/** The interval callback sets React state outside a React event. */
function tickTimers(ms: number): Promise<void> {
  return act(async () => {
    mock.timers.tick(ms);
  });
}

test('renders the first quote in the strip', () => {
  mount();

  const button = screen.getByRole('button', { name: 'Read full quote' });
  assert.equal(button.getAttribute('aria-haspopup'), 'dialog');
  assert.equal(button.getAttribute('aria-expanded'), 'false');

  assert.ok(stripText().includes(QUOTES[0].text));
  assert.ok(stripText().includes(QUOTES[0].author));
});

test('advances only after QUOTE_ADVANCE_MS', async () => {
  // Only intervals are faked: Radix uses real timers for animations.
  mock.timers.enable({ apis: ['setInterval'] });
  try {
    mount();

    await tickTimers(10_000);
    assert.ok(
      stripText().includes(QUOTES[0].text),
      'still on the first quote after 10s',
    );

    await tickTimers(QUOTE_ADVANCE_MS);
    assert.ok(stripText().includes(QUOTES[1].text), 'second quote after 60s');
    assert.equal(stripText().includes(QUOTES[0].text), false);
  } finally {
    mock.timers.reset();
  }
});

test('clicking the quote opens the full quote in a dialog', () => {
  mount();

  fireEvent.click(stripButton());

  const dialog = screen.getByRole('dialog');
  assert.ok(dialog.textContent?.includes(QUOTES[0].text));
  assert.ok(dialog.textContent?.includes(`— ${QUOTES[0].author}`));
  assert.ok(dialog.textContent?.includes('Daily Inspiration'));
  assert.equal(stripButton().getAttribute('aria-expanded'), 'true');

  // The full quote is never clamped, and stays selectable for copying.
  const quoteParagraph = Array.from(dialog.querySelectorAll('p')).find(
    (paragraph) => paragraph.textContent?.includes(QUOTES[0].text),
  );
  assert.ok(quoteParagraph, 'the dialog body holds the quote');
  assert.ok(quoteParagraph.className.includes('selectable'));
  assert.equal(quoteParagraph.className.includes('line-clamp'), false);

  // The strip keeps showing the same quote behind the dialog.
  assert.ok(stripText().includes(QUOTES[0].text));
});

test('Next advances the strip without opening the dialog', () => {
  mount();

  // Mobile and desktop chevrons are both in the DOM; CSS only hides one.
  const nextButtons = screen.getAllByRole('button', { name: 'Next quote' });
  assert.equal(nextButtons.length, 2);

  fireEvent.click(nextButtons[0]);

  assert.equal(
    isDialogOpen(),
    false,
    'stepping the carousel must not open the dialog',
  );
  assert.ok(stripText().includes(QUOTES[1].text));
});

test('auto-advance pauses while reading and restarts once closed', async () => {
  mock.timers.enable({ apis: ['setInterval'] });
  try {
    mount();

    fireEvent.click(stripButton());

    await tickTimers(QUOTE_ADVANCE_MS);
    assert.ok(
      dialogText().includes(QUOTES[0].text),
      'the open dialog keeps its quote',
    );
    assert.ok(
      stripText().includes(QUOTES[0].text),
      'the strip does not swap underneath the reader',
    );

    fireEvent.click(screen.getByRole('button', { name: 'Close' }));

    await tickTimers(QUOTE_ADVANCE_MS);
    assert.ok(
      stripText().includes(QUOTES[1].text),
      'closing starts a fresh interval',
    );
  } finally {
    mock.timers.reset();
  }
});

test('an in-progress text selection does not open the dialog', () => {
  const original = window.getSelection;
  window.getSelection = (() => ({
    toString: () => QUOTES[0].text,
  })) as unknown as typeof window.getSelection;
  try {
    mount();

    fireEvent.click(stripButton());
    assert.equal(
      isDialogOpen(),
      false,
      'copying the visible quote must not open the dialog',
    );
  } finally {
    window.getSelection = original;
  }

  // The same click without a selection still opens it.
  fireEvent.click(stripButton());
  assert.equal(isDialogOpen(), true);
});

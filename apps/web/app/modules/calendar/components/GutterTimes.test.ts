// The gutter edge pills are precision handles. A touch that starts on one must
// not pan the calendar: React's onTouchMove is passive, so the block is a
// native non-passive listener attached for the life of the pill.
//
// DOM must exist before the component module is imported — installDom() then a
// dynamic import (static ESM imports hoist and can race the DOM setup).

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement } from 'react';
import { installDom } from './install-dom';

installDom();

let GutterTimes: typeof import('./GutterTimes').GutterTimes;
let cleanup: () => void;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  render = rtl.render;
  screen = rtl.screen;
  GutterTimes = (await import('./GutterTimes')).GutterTimes;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

test('a touch on a gutter edge pill does not scroll', () => {
  render(
    createElement(GutterTimes, {
      hourH: 48,
      hourLabels: [],
      now: null,
      edges: {
        start: new Date(2026, 9, 18, 9, 0, 0, 0),
        end: new Date(2026, 9, 18, 10, 0, 0, 0),
      },
      headerOffset: 40,
      onEdgePointerDown: () => {},
    }),
  );

  const end = screen.getByRole('slider', { name: 'End time' });
  const touch = new window.Event('touchmove', {
    bubbles: true,
    cancelable: true,
  });
  end.dispatchEvent(touch);
  assert.equal(touch.defaultPrevented, true);
});

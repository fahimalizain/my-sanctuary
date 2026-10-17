// Mount tests for the Appearance theme buttons. DOM must exist before the
// component module is imported — installDom() then dynamic import.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, type ComponentType } from 'react';
import type { ThemePreference } from '@/lib/theme';
import { installDom } from '../calendar/components/install-dom';

installDom();

type ThemePreferenceControlProps = {
  preference: ThemePreference;
  onChange: (preference: ThemePreference) => void;
};

let ThemePreferenceControl: ComponentType<ThemePreferenceControlProps>;
let cleanup: () => void;
let fireEvent: typeof import('@testing-library/react').fireEvent;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  fireEvent = rtl.fireEvent;
  render = rtl.render;
  screen = rtl.screen;
  const mod = await import('./ThemePreferenceControl');
  ThemePreferenceControl = mod.ThemePreferenceControl;
});

afterEach(() => {
  cleanup();
});

after(() => {
  cleanup();
});

function makeSpy() {
  const calls: ThemePreference[] = [];
  return {
    calls,
    onChange: (preference: ThemePreference) => {
      calls.push(preference);
    },
  };
}

function mount(preference: ThemePreference) {
  const spy = makeSpy();
  render(
    createElement(ThemePreferenceControl, {
      preference,
      onChange: spy.onChange,
    }),
  );
  return spy;
}

function button(name: 'Light' | 'Dark' | 'System'): HTMLButtonElement {
  return screen.getByRole('button', { name }) as HTMLButtonElement;
}

function pressed(name: 'Light' | 'Dark' | 'System'): string | null {
  return button(name).getAttribute('aria-pressed');
}

test('light preference: Light is the only pressed option', () => {
  mount('light');

  assert.equal(pressed('Light'), 'true');
  assert.equal(pressed('Dark'), 'false');
  assert.equal(pressed('System'), 'false');

  // Selected and unselected visuals match the existing Appearance buttons.
  assert.ok(button('Light').className.includes('border-primary'));
  assert.ok(button('Dark').className.includes('border-transparent'));
  assert.ok(button('System').className.includes('border-transparent'));

  assert.equal(screen.getByRole('group', { name: 'Theme' }).tagName, 'DIV');
  for (const name of ['Light', 'Dark', 'System'] as const) {
    assert.equal(button(name).type, 'button');
  }
});

test('clicking Dark reports dark once', () => {
  const spy = mount('light');

  fireEvent.click(button('Dark'));

  assert.deepEqual(spy.calls, ['dark']);
});

test('system preference: System stays pressed; clicking Light reports light', () => {
  const spy = mount('system');

  assert.equal(pressed('System'), 'true');
  assert.equal(pressed('Light'), 'false');
  assert.equal(pressed('Dark'), 'false');

  fireEvent.click(button('Light'));

  assert.deepEqual(spy.calls, ['light']);
});

test('dark preference: clicking System reports system', () => {
  const spy = mount('dark');

  assert.equal(pressed('Dark'), 'true');
  assert.equal(pressed('Light'), 'false');
  assert.equal(pressed('System'), 'false');

  fireEvent.click(button('System'));

  assert.deepEqual(spy.calls, ['system']);
});

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { focusPinVisibility } from './focus-pin';

test('focusPinVisibility: focused pin is always visible', () => {
  assert.equal(focusPinVisibility(true), 'opacity-100');
});

test('focusPinVisibility: unfocused pin is hidden until hover, but always visible on no-hover pointers', () => {
  const classes = focusPinVisibility(false);
  assert.ok(classes.includes('opacity-0'));
  assert.ok(classes.includes('group-hover:opacity-100'));
  // Coarse / no-hover pointers (mobile) always show the pin — the hover
  // affordance does not exist there, so the pin cannot be hidden behind it.
  assert.ok(classes.includes('[@media(hover:none)]:opacity-100'));
});

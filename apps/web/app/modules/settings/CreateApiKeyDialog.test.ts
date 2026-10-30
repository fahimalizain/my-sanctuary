// Mount tests for the create-key dialog (form → one-time secret reveal).
// DOM must exist before the component module is imported — installDom() then
// dynamic import, same constraint as ThemePreferenceControl.test.ts.

import { after, afterEach, before, test } from 'node:test';
import assert from 'node:assert/strict';
import { createElement, useState, type ComponentType } from 'react';
import type { CreateApiKeyResponse, NewApiKeyInput } from '@/app/types';
import { addCivilDays, civilToday } from '../routines/rrule-preview';
import { installDom } from '../calendar/components/install-dom';

// `@/lib/api/base` reads the Vite `__API_BASE_URL__` define at module load
// and static ESM imports hoist, so the global is set before the dialog is
// dynamic-imported in `before`.
(globalThis as { __API_BASE_URL__?: string }).__API_BASE_URL__ = '';

installDom();

type CreateApiKeyDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreate: (input: NewApiKeyInput) => Promise<CreateApiKeyResponse>;
};

let CreateApiKeyDialog: ComponentType<CreateApiKeyDialogProps>;
let cleanup: typeof import('@testing-library/react').cleanup;
let fireEvent: typeof import('@testing-library/react').fireEvent;
let render: typeof import('@testing-library/react').render;
let screen: typeof import('@testing-library/react').screen;
let waitFor: typeof import('@testing-library/react').waitFor;

before(async () => {
  const rtl = await import('@testing-library/react');
  cleanup = rtl.cleanup;
  fireEvent = rtl.fireEvent;
  render = rtl.render;
  screen = rtl.screen;
  waitFor = rtl.waitFor;
  const mod = await import('./CreateApiKeyDialog');
  CreateApiKeyDialog = mod.CreateApiKeyDialog;
});

afterEach(() => {
  cleanup();
  // Any clipboard double installed by a test must not leak into the next one.
  delete (window.navigator as { clipboard?: unknown }).clipboard;
});

after(() => {
  cleanup();
});

const SECRET = 'sk_test_0123456789abcdef';

function created(secret = SECRET): CreateApiKeyResponse {
  const now = new Date().toISOString();
  return {
    key: {
      id: 'key-1',
      name: 'Laptop',
      key_prefix: 'sk_test_0123',
      expires_on: null,
      expires_at: null,
      last_used_at: null,
      created_at: now,
    },
    secret,
  };
}

interface OnCreateSpy {
  calls: NewApiKeyInput[];
  onCreate: (input: NewApiKeyInput) => Promise<CreateApiKeyResponse>;
}

function spyOnCreate(
  result: () => Promise<CreateApiKeyResponse> = async () => created(),
): OnCreateSpy {
  const calls: NewApiKeyInput[] = [];
  return {
    calls,
    onCreate: async (input: NewApiKeyInput) => {
      calls.push(input);
      return result();
    },
  };
}

/**
 * A stateful harness: the dialog is genuinely controlled, so Done/Cancel flip
 * `open` and the "Open dialog" button reopens it the way Settings does.
 */
function makeHarness(onCreate: CreateApiKeyDialogProps['onCreate']) {
  function Harness() {
    const [open, setOpen] = useState(true);
    return createElement(
      'div',
      null,
      createElement(
        'button',
        { type: 'button', onClick: () => setOpen(true) },
        'Open dialog',
      ),
      createElement(CreateApiKeyDialog, {
        open,
        onOpenChange: setOpen,
        onCreate,
      }),
    );
  }
  return Harness;
}

function mount(spy: OnCreateSpy) {
  render(createElement(makeHarness(spy.onCreate)));
}

function nameInput(): HTMLInputElement {
  return screen.getByLabelText('Name') as HTMLInputElement;
}

function expirySelect(): HTMLSelectElement {
  return screen.getByLabelText('Expiry') as HTMLSelectElement;
}

function submit(): void {
  fireEvent.click(screen.getByRole('button', { name: 'Create key' }));
}

async function revealSecret(): Promise<HTMLInputElement> {
  return (await screen.findByLabelText('API key secret')) as HTMLInputElement;
}

test('happy path: sends the trimmed name and reveals the secret once', async () => {
  const spy = spyOnCreate();
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: '  Laptop  ' } });
  submit();
  const secretInput = await revealSecret();

  assert.deepEqual(spy.calls, [{ name: 'Laptop', expires_on: null }]);
  assert.equal(secretInput.value, SECRET);
  // Exactly one copy: the curl example uses the placeholder, never the secret.
  assert.equal(screen.getAllByDisplayValue(SECRET).length, 1);
  assert.equal(screen.queryByLabelText('Name'), null);
  assert.ok(!(document.body.textContent ?? '').includes(SECRET));
});

test('30-day preset sends the civil date 30 days out', async () => {
  const spy = spyOnCreate();
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: 'Laptop' } });
  fireEvent.change(expirySelect(), { target: { value: '30' } });
  submit();
  await revealSecret();

  assert.equal(spy.calls.length, 1);
  assert.equal(spy.calls[0].expires_on, addCivilDays(civilToday(), 30));
});

test('rejection: the message is shown and the form stays', async () => {
  const spy = spyOnCreate(async () => {
    throw new Error('too many active api keys');
  });
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: 'Laptop' } });
  submit();

  const alert = await screen.findByRole('alert');
  assert.match(alert.textContent ?? '', /too many active api keys/);
  assert.equal(screen.queryByLabelText('API key secret'), null);
  assert.equal(nameInput().value, 'Laptop');
});

test('reveal guard: Escape cannot dismiss; Done closes', async () => {
  const spy = spyOnCreate();
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: 'Laptop' } });
  submit();
  await revealSecret();

  fireEvent.keyDown(document, { key: 'Escape' });
  assert.equal(
    (screen.getByLabelText('API key secret') as HTMLInputElement).value,
    SECRET,
  );

  fireEvent.click(screen.getByRole('button', { name: 'Done' }));
  await waitFor(() => {
    assert.equal(screen.queryByLabelText('API key secret'), null);
  });
});

test('copy: a denied clipboard falls back to focus + select', async () => {
  Object.defineProperty(window.navigator, 'clipboard', {
    configurable: true,
    value: {
      writeText: () => Promise.reject(new Error('denied')),
    },
  });

  const spy = spyOnCreate();
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: 'Laptop' } });
  submit();
  const secretInput = await revealSecret();

  fireEvent.click(screen.getByRole('button', { name: 'Copy' }));

  await screen.findByText('Press ⌘C / Ctrl+C, or long-press, to copy.');
  assert.equal(document.activeElement, secretInput);
  assert.equal(secretInput.selectionStart, 0);
  assert.equal(secretInput.selectionEnd, SECRET.length);
});

test('reopen after Done resets the form and drops the secret', async () => {
  const spy = spyOnCreate();
  mount(spy);

  fireEvent.change(nameInput(), { target: { value: 'Laptop' } });
  fireEvent.change(expirySelect(), { target: { value: '90' } });
  submit();
  await revealSecret();

  fireEvent.click(screen.getByRole('button', { name: 'Done' }));
  await waitFor(() => {
    assert.equal(screen.queryByLabelText('API key secret'), null);
  });

  fireEvent.click(screen.getByRole('button', { name: 'Open dialog' }));

  assert.equal(nameInput().value, '');
  assert.equal(expirySelect().value, 'never');
  assert.equal(screen.queryByLabelText('API key secret'), null);
});

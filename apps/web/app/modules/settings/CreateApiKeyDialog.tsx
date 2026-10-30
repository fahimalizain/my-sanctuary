// Create API key: a name + expiry form, then a one-time secret reveal.
//
// The secret exists only in this component's state. It is never written to the
// query cache (the mutation has `gcTime: 0`), storage, the URL, or a log, and
// the reveal step refuses every dismissal except its own Done button — a
// stray Escape must not cost the user the only copy of a new key.

import { useEffect, useRef, useState } from 'react';
import { Check, Copy, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import type { CreateApiKeyResponse, NewApiKeyInput } from '@/app/types';
import { addCivilDays, civilToday } from '@/app/modules/routines/rrule-preview';
import { API_BASE_URL } from '@/lib/api/base';
import { cn } from '@/lib/utils';

const INPUT_CLASS =
  'w-full rounded-xl border border-input bg-background px-4 py-3 text-foreground placeholder:text-muted-foreground/60 focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20 transition-all';

// Civil-day presets. The custom option sends the picked `YYYY-MM-DD` as-is.
const PRESET_DAYS = { '30': 30, '90': 90, '365': 365 } as const;
type PresetExpiry = keyof typeof PRESET_DAYS;
type ExpiryChoice = 'never' | 'custom' | PresetExpiry;

const EXPIRY_OPTIONS: { value: ExpiryChoice; label: string }[] = [
  { value: 'never', label: 'Never' },
  { value: '30', label: '30 days' },
  { value: '90', label: '90 days' },
  { value: '365', label: '1 year' },
  { value: 'custom', label: 'Custom date' },
];

const COPIED_LABEL_MS = 2000;

export interface CreateApiKeyDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Creates the key. Rejections are shown inline; the form stays mounted. */
  onCreate: (input: NewApiKeyInput) => Promise<CreateApiKeyResponse>;
}

export function CreateApiKeyDialog({
  open,
  onOpenChange,
  onCreate,
}: CreateApiKeyDialogProps) {
  const [name, setName] = useState('');
  const [expiry, setExpiry] = useState<ExpiryChoice>('never');
  const [customDate, setCustomDate] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [copyFallback, setCopyFallback] = useState(false);
  const secretInputRef = useRef<HTMLInputElement>(null);

  const revealing = secret !== null;

  // A fresh dialog every time it opens: no name, expiry, error, or secret from
  // the previous key — and nothing left behind after Done closes it.
  useEffect(() => {
    if (!open) return;
    setName('');
    setExpiry('never');
    setCustomDate('');
    setSubmitting(false);
    setError(null);
    setSecret(null);
    setCopied(false);
    setCopyFallback(false);
  }, [open]);

  // The "Copied" acknowledgement is a short-lived flash, not a state.
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), COPIED_LABEL_MS);
    return () => window.clearTimeout(timer);
  }, [copied]);

  // Reveal: put the secret in reach of ⌘C straight away.
  useEffect(() => {
    if (secret === null) return;
    const input = secretInputRef.current;
    if (!input) return;
    input.focus();
    input.select();
  }, [secret]);

  const clearTransient = () => {
    setSubmitting(false);
    setError(null);
    setSecret(null);
    setCopied(false);
    setCopyFallback(false);
  };

  // Guarded dismissal: while the one-time secret is on screen, only Done may
  // close (the built-in X and reason-less closes funnel through here), and a
  // create in flight must not be abandoned — its response carries the secret.
  const handleOpenChange = (next: boolean) => {
    if (!next && (revealing || submitting)) return;
    if (!next) clearTransient();
    onOpenChange(next);
  };

  const handleSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (submitting) return;

    const trimmed = name.trim();
    if (trimmed === '') {
      setError('Name is required');
      return;
    }

    const today = civilToday();
    let expiresOn: string | null = null;
    if (expiry === 'custom') {
      if (customDate === '') {
        setError('Expiry date is required');
        return;
      }
      if (customDate < today) {
        setError('Expiry date must be today or later');
        return;
      }
      expiresOn = customDate;
    } else if (expiry !== 'never') {
      expiresOn = addCivilDays(today, PRESET_DAYS[expiry]);
    }

    setError(null);
    setSubmitting(true);
    try {
      const created = await onCreate({ name: trimmed, expires_on: expiresOn });
      setSubmitting(false);
      setSecret(created.secret);
    } catch (err) {
      setSubmitting(false);
      setError(err instanceof Error ? err.message : 'Could not create the key');
    }
  };

  const handleCopy = async () => {
    if (secret === null) return;
    try {
      if (!navigator.clipboard?.writeText) throw new Error('no clipboard');
      await navigator.clipboard.writeText(secret);
      setCopyFallback(false);
      setCopied(true);
      return;
    } catch {
      // Clipboard API absent (insecure context, older browser) or denied.
    }
    setCopied(false);
    setCopyFallback(true);
    const input = secretInputRef.current;
    if (input) {
      input.focus();
      input.select();
    }
  };

  const handleDone = () => {
    // Direct: the guard above exists to block *accidental* closes.
    clearTransient();
    setName('');
    onOpenChange(false);
  };

  const blockDismissal = (event: { preventDefault: () => void }) => {
    if (revealing) event.preventDefault();
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden border-border bg-card p-0 sm:max-w-[520px]"
        onEscapeKeyDown={blockDismissal}
        onPointerDownOutside={blockDismissal}
        onInteractOutside={blockDismissal}
      >
        <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
          <DialogTitle className="text-foreground">
            {revealing ? 'Copy your new key' : 'Create API key'}
          </DialogTitle>
          <DialogDescription>
            {revealing
              ? "Copy this key now. You won't be able to see it again."
              : 'The key is shown once, right after you create it. Store it somewhere safe.'}
          </DialogDescription>
        </DialogHeader>
        <hr className="shrink-0 border-border" />

        {revealing ? (
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 py-4">
              <div className="flex flex-col gap-2 sm:flex-row">
                <input
                  ref={secretInputRef}
                  readOnly
                  aria-label="API key secret"
                  autoComplete="off"
                  spellCheck={false}
                  value={secret ?? ''}
                  onFocus={(event) => event.currentTarget.select()}
                  onClick={(event) => event.currentTarget.select()}
                  className={cn(
                    INPUT_CLASS,
                    'selectable font-mono text-xs sm:flex-1',
                  )}
                />
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => void handleCopy()}
                  className="shrink-0"
                >
                  {copied ? (
                    <Check className="mr-1.5 h-4 w-4" />
                  ) : (
                    <Copy className="mr-1.5 h-4 w-4" />
                  )}
                  {copied ? 'Copied' : 'Copy'}
                </Button>
              </div>
              <span role="status" className="sr-only">
                {copied ? 'Copied to clipboard' : ''}
              </span>
              {copyFallback && (
                <p className="selectable mt-2 text-xs text-muted-foreground">
                  Press ⌘C / Ctrl+C, or long-press, to copy.
                </p>
              )}

              <div className="mt-5 space-y-2">
                <p className="text-sm font-medium text-foreground">
                  Try it from a terminal
                </p>
                <pre className="selectable overflow-x-auto rounded-lg border border-border bg-muted/50 p-3 font-mono text-xs text-muted-foreground">
                  {`curl -H "Authorization: Bearer sk_…" ${API_BASE_URL || window.location.origin}/api/tasks`}
                </pre>
              </div>
            </div>

            <DialogFooter className="shrink-0 gap-2 border-t border-border px-6 py-4 sm:space-x-0">
              <Button onClick={handleDone}>Done</Button>
            </DialogFooter>
          </div>
        ) : (
          <form
            onSubmit={(event) => void handleSubmit(event)}
            className="flex min-h-0 flex-1 flex-col"
          >
            <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 py-4">
              <div className="mb-5 space-y-2">
                <label
                  htmlFor="create-api-key-name"
                  className="text-sm font-medium text-foreground"
                >
                  Name
                </label>
                <input
                  id="create-api-key-name"
                  type="text"
                  value={name}
                  maxLength={64}
                  autoComplete="off"
                  onChange={(event) => setName(event.target.value)}
                  placeholder="e.g. Laptop scripts"
                  className={INPUT_CLASS}
                />
                <p className="text-xs text-muted-foreground">
                  Surrounding spaces are trimmed. Up to 64 characters.
                </p>
              </div>

              <div className="mb-5 space-y-2">
                <label
                  htmlFor="create-api-key-expiry"
                  className="text-sm font-medium text-foreground"
                >
                  Expiry
                </label>
                <select
                  id="create-api-key-expiry"
                  value={expiry}
                  onChange={(event) =>
                    setExpiry(event.target.value as ExpiryChoice)
                  }
                  className={INPUT_CLASS}
                >
                  {EXPIRY_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </div>

              {expiry === 'custom' && (
                <div className="mb-5 space-y-2">
                  <label
                    htmlFor="create-api-key-expiry-date"
                    className="text-sm font-medium text-foreground"
                  >
                    Expiry date
                  </label>
                  <input
                    id="create-api-key-expiry-date"
                    type="date"
                    min={civilToday()}
                    value={customDate}
                    onChange={(event) => setCustomDate(event.target.value)}
                    className={INPUT_CLASS}
                  />
                </div>
              )}

              {error && (
                <p
                  role="alert"
                  className="selectable mb-4 rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive"
                >
                  {error}
                </p>
              )}
            </div>

            <DialogFooter className="shrink-0 gap-2 border-t border-border px-6 py-4 sm:space-x-0">
              <Button
                type="button"
                variant="outline"
                disabled={submitting}
                onClick={() => handleOpenChange(false)}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button type="submit" disabled={submitting}>
                {submitting && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Create key
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

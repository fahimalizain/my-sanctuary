// "Pause everything": a global pause period (every rule) plus the list of
// existing periods, so a stale pause can be deleted without leaving the page.

import { useEffect, useState } from 'react';
import { Loader2, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import type { NewPauseInput, PauseRecord } from '@/app/types';

const INPUT_CLASS =
  'w-full rounded-xl border border-input bg-background px-4 py-3 text-foreground placeholder:text-muted-foreground/60 focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20 transition-all';

export function GlobalPauseDialog({
  open,
  onClose,
  today,
  pauses,
  pausesLoading,
  ruleNames,
  pending,
  onCreate,
  onDelete,
}: {
  open: boolean;
  onClose: () => void;
  /** The server's civil today (`scoreboard.as_of`). */
  today: string;
  pauses: PauseRecord[];
  pausesLoading: boolean;
  /** rule id → name, for the per-rule periods. */
  ruleNames: Map<string, string>;
  pending: boolean;
  onCreate: (input: NewPauseInput) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
}) {
  const [from, setFrom] = useState(today);
  const [to, setTo] = useState(today);
  const [reason, setReason] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setFrom(today);
    setTo(today);
    setReason('');
    setError(null);
  }, [open, today]);

  const rangeValid = from !== '' && to !== '' && to >= from;

  const handleCreate = async () => {
    if (pending) return;
    if (!rangeValid) {
      setError('The end date must not be before the start');
      return;
    }
    setError(null);
    try {
      await onCreate({
        from,
        to,
        ...(reason.trim() === '' ? {} : { reason: reason.trim() }),
      });
      setReason('');
    } catch (err) {
      setError(
        err instanceof Error ? err.message : 'Could not create the pause',
      );
    }
  };

  const handleDelete = async (pause: PauseRecord) => {
    const label = pause.rule_id === null ? 'Everything' : 'this rule';
    if (
      !window.confirm(
        `Delete the pause of ${label} (${pause.from} → ${pause.to})?`,
      )
    ) {
      return;
    }
    setError(null);
    try {
      await onDelete(pause.id);
    } catch (err) {
      setError(
        err instanceof Error ? err.message : 'Could not delete the pause',
      );
    }
  };

  return (
    <Dialog open={open} onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="flex max-h-[90dvh] flex-col gap-0 overflow-hidden border-border bg-card p-0 sm:max-w-[520px]">
        <DialogHeader className="shrink-0 px-6 pt-6 pb-4">
          <DialogTitle className="text-foreground">
            Pause everything
          </DialogTitle>
          <DialogDescription>
            Paused days are never counted and never break a streak.
          </DialogDescription>
        </DialogHeader>
        <hr className="shrink-0 border-border" />
        <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-6 pb-4">
          <div className="mb-5 grid grid-cols-2 gap-3">
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">
                From
              </label>
              <input
                type="date"
                value={from}
                onChange={(event) => setFrom(event.target.value)}
                className={INPUT_CLASS}
              />
            </div>
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">To</label>
              <input
                type="date"
                value={to}
                onChange={(event) => setTo(event.target.value)}
                className={INPUT_CLASS}
              />
            </div>
          </div>
          <div className="mb-5 space-y-2">
            <label className="text-sm font-medium text-foreground">
              Reason{' '}
              <span className="font-normal text-muted-foreground">
                (optional)
              </span>
            </label>
            <input
              type="text"
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              placeholder="e.g. Travel"
              className={INPUT_CLASS}
            />
          </div>
          <Button
            onClick={handleCreate}
            disabled={pending || !rangeValid}
            className="w-full bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {pending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            Pause every rule
          </Button>

          <div className="mt-6">
            <p className="mb-2 text-sm font-medium text-foreground">
              Existing pauses
            </p>
            {pausesLoading && pauses.length === 0 ? (
              <p className="text-xs text-muted-foreground">Loading pauses…</p>
            ) : pauses.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                No pauses yet — every rule is being scored.
              </p>
            ) : (
              <div className="space-y-2">
                {pauses.map((pause) => (
                  <div
                    key={pause.id}
                    className="flex items-center gap-2 rounded-lg border border-border bg-background p-3"
                  >
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm text-foreground">
                        {pause.rule_id === null
                          ? 'Everything'
                          : (ruleNames.get(pause.rule_id) ?? 'Rule')}
                      </p>
                      <p className="text-xs text-muted-foreground">
                        {pause.from} → {pause.to}
                        {pause.reason ? ` · ${pause.reason}` : ''}
                      </p>
                    </div>
                    <button
                      type="button"
                      onClick={() => handleDelete(pause)}
                      disabled={pending}
                      aria-label={`Delete pause ${pause.from} to ${pause.to}`}
                      title="Delete pause"
                      className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive disabled:opacity-40"
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>

          {error && (
            <p className="selectable mt-4 text-sm text-destructive">{error}</p>
          )}
        </div>

        <div className="flex shrink-0 justify-end border-t border-border px-6 py-4">
          <Button
            variant="outline"
            onClick={onClose}
            className="border-input text-foreground hover:bg-muted"
          >
            Close
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

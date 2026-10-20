import { Button } from '@/components/ui/button';

interface DeleteEventToastProps {
  /** Event title at delete time; may be blank or whitespace. */
  title: string;
  onUndo: () => void;
}

/**
 * Undo affordance for a deferred delete. Presentational only: the session owns
 * the grace timer, so this never starts a timeout. Rendering it does not move
 * focus, and its live region announces the delete without stealing the
 * keyboard from the calendar.
 */
export function DeleteEventToast({ title, onUndo }: DeleteEventToastProps) {
  const trimmed = title.trim();
  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-auto fixed bottom-[calc(6rem+env(safe-area-inset-bottom))] left-1/2 z-[70] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5 text-sm text-card-foreground shadow-lg"
    >
      <p className="min-w-0 truncate">
        {trimmed ? `Deleted “${trimmed}”` : 'Deleted untitled event'}
      </p>
      <Button
        type="button"
        variant="default"
        size="sm"
        className="shrink-0"
        onClick={onUndo}
      >
        Undo
      </Button>
    </div>
  );
}

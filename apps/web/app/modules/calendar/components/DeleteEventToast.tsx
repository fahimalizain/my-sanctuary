import { Toast } from '@/components/ui/toast';

interface DeleteEventToastProps {
  /** Event title at delete time; may be blank or whitespace. */
  title: string;
  onUndo: () => void;
}

/**
 * Undo affordance for a deferred delete, as a plain action Toast.
 * Presentational only: the session owns the grace timer, so this never starts
 * a timeout. Rendering it does not move focus, and its live region announces
 * the delete without stealing the keyboard from the calendar.
 */
export function DeleteEventToast({ title, onUndo }: DeleteEventToastProps) {
  const trimmed = title.trim();
  return (
    <Toast actions={[{ label: 'Undo', onClick: onUndo }]}>
      {trimmed ? `Deleted “${trimmed}”` : 'Deleted untitled event'}
    </Toast>
  );
}

import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';

interface DeleteEventDialogProps {
  open: boolean;
  title: string;
  isDeleting?: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

// Above the inspector peek / sheet (z-[60]).
const ABOVE_INSPECTOR = 'z-[70]';

/** Confirm step for the touch delete handle. Cancel keeps the selection. */
export function DeleteEventDialog({
  open,
  title,
  isDeleting = false,
  onCancel,
  onConfirm,
}: DeleteEventDialogProps) {
  return (
    <Dialog open={open} onOpenChange={(next) => !next && onCancel()}>
      <DialogContent
        className={`${ABOVE_INSPECTOR} max-w-sm`}
        overlayClassName={ABOVE_INSPECTOR}
      >
        <DialogHeader>
          <DialogTitle>Delete event?</DialogTitle>
          <DialogDescription className="break-words">
            {title.trim() ? `"${title}"` : 'Untitled event'} will be removed
            from your calendar.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter className="gap-2 sm:gap-0">
          <Button type="button" variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={isDeleting}
            onClick={onConfirm}
          >
            Delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

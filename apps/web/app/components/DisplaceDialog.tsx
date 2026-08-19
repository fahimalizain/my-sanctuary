import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';

interface DisplaceDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The task (A) currently running — the one being parked. */
  runningTitle: string;
  /** The task (B) trying to take the running slot. */
  incomingTitle: string;
  /** Park A at the chosen status (Planned / Done / Discarded). */
  onConfirm: (status: 'PLANNED' | 'COMPLETED' | 'DISCARDED') => void;
}

/** Occupied In Progress conflict dialog (ADR 0002 § UI): pick where the
 *  running task is parked so another task can start. Cancel / overlay close =
 *  no request. Shared by the board's drag-and-drop flow and the task modal's
 *  status pills. */
export function DisplaceDialog({
  open,
  onOpenChange,
  runningTitle,
  incomingTitle,
  onConfirm,
}: DisplaceDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[420px] bg-card border-border">
        <DialogHeader>
          <DialogTitle className="text-foreground">
            A task is already running
          </DialogTitle>
          <DialogDescription>
            “{runningTitle}” is in progress. Where should it be parked so “
            {incomingTitle}” can start?
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-wrap gap-2 pt-2">
          <Button
            onClick={() => onConfirm('PLANNED')}
            className="bg-primary text-primary-foreground hover:bg-primary/90"
          >
            Planned
          </Button>
          <Button
            variant="outline"
            onClick={() => onConfirm('COMPLETED')}
            className="border-input text-foreground hover:bg-muted"
          >
            Done
          </Button>
          <Button
            variant="outline"
            onClick={() => onConfirm('DISCARDED')}
            className="border-input text-destructive hover:bg-destructive/10"
          >
            Discarded
          </Button>
        </div>

        <div className="flex justify-end pt-1">
          <Button
            variant="ghost"
            onClick={() => onOpenChange(false)}
            className="text-muted-foreground hover:bg-muted"
          >
            Cancel
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

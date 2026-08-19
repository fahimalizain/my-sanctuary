import type { ReactNode } from 'react';
import { cn } from '@/lib/utils';

/** A pill/toggle in the filter row — same shape as TaskModal's
 *  priority/difficulty chips; selected pills flip to the primary fill. */
export function FilterPill({
  selected,
  onClick,
  children,
}: {
  selected: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'flex items-center gap-2 rounded-xl border-2 px-3 py-1.5 text-sm font-medium transition-all',
        selected
          ? 'bg-primary border-primary text-primary-foreground'
          : 'bg-background border-input text-muted-foreground hover:border-primary/30',
      )}
    >
      {children}
    </button>
  );
}

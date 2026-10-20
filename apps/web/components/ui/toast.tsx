import type * as React from 'react';

import { Button, type ButtonProps } from '@/components/ui/button';
import { cn } from '@/lib/utils';

export interface ToastAction {
  label: string;
  onClick: () => void;
  /** Defaults to the primary button (brand green), matching Undo today. */
  variant?: ButtonProps['variant'];
  disabled?: boolean;
}

export interface ToastProps {
  /** Announced message. A string is the normal case; a node is allowed. */
  children: React.ReactNode;
  /** Zero or more actions, rendered in order after the message. */
  actions?: readonly ToastAction[];
  /** Defaults to polite. */
  politeness?: 'polite' | 'assertive';
  className?: string;
}

/**
 * Fixed toast with a message and optional action buttons.
 *
 * There is exactly one toast, not a stack, and no provider or queue: the
 * caller mounts it and owns any timer. It never moves focus on render, so it
 * cannot pull the keyboard away from whatever the user is doing.
 *
 * Placement: `z-[70]` clears the nav pill in app/routes/__root.tsx, which is
 * `fixed bottom-6 z-50`, and the sheets/inspector layers at `z-[60]`. The
 * bottom offset also lifts it above the nav pill and reserves the safe-area
 * inset on notched phones.
 */
export function Toast({
  children,
  actions,
  politeness = 'polite',
  className,
}: ToastProps) {
  return (
    <div
      role="status"
      aria-live={politeness}
      className={cn(
        'pointer-events-auto fixed bottom-[calc(6rem+env(safe-area-inset-bottom))] left-1/2 z-[70] flex max-w-[calc(100vw-2rem)] -translate-x-1/2 items-center gap-3 rounded-xl border border-border bg-card px-4 py-2.5 text-sm text-card-foreground shadow-lg',
        className,
      )}
    >
      <p className="min-w-0 truncate">{children}</p>
      {actions?.length ? (
        <div className="flex shrink-0 items-center gap-2">
          {actions.map((action, index) => (
            <Button
              key={index}
              type="button"
              variant={action.variant ?? 'default'}
              size="sm"
              className="shrink-0"
              disabled={action.disabled}
              onClick={action.onClick}
            >
              {action.label}
            </Button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

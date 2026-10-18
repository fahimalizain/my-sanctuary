import * as React from 'react';
import * as DialogPrimitive from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import { cn } from '@/lib/utils';

// A bottom sheet built on the SAME Radix Dialog primitive as `dialog.tsx`,
// but deliberately not a `DialogContent` variant: DialogContent is a centered
// modal (`left-1/2 top-1/2 translate`) with its own card chrome. SheetContent
// is only the POSITIONED FRAME — bottom-anchored, transparent, no padding, no
// rounded corners and no `overflow-hidden`, so the caller can build the
// visible shell (corners + the scroll clip) inside it and let a portaled panel
// grow past the shell's edge.
//
// z-[60], not z-50: the floating nav pill (`app/routes/__root.tsx`) is z-50
// and would sit ON TOP of the sheet along the bottom edge, eating taps.
// EventInspector raises itself the same way.
//
// Animation: tailwindcss-animate (1.0.7). The bare `slide-in-from-bottom`
// class maps to the plugin's DEFAULT translate of 100%, i.e. the full content
// height. No `duration-*` / `ease-*` utilities here — they collide with the
// plugin's own duration/timing utilities and get dropped as ambiguous
// (Tailwind 3.4; same note as the nav pill in `__root.tsx`). The plugin's
// default enter duration is fine.
const Sheet = DialogPrimitive.Root;

const SheetClose = DialogPrimitive.Close;

const SheetContent = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Content>
>(({ className, children, ...props }, ref) => (
  <DialogPrimitive.Portal>
    {/* Same backdrop as DialogContent (bg-black/40), raised to the sheet z. */}
    <DialogPrimitive.Overlay className="fixed inset-0 z-[60] bg-black/40 data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0" />
    <DialogPrimitive.Content
      ref={ref}
      // No SheetDescription: opt out so Radix does not warn about the
      // missing description. The title carries the meaning; `aria-describedby`
      // stays overridable through `props` for a future caller.
      aria-describedby={undefined}
      className={cn(
        'fixed inset-x-0 bottom-0 z-[60] flex max-h-[85dvh] w-full flex-col overflow-visible border-0 bg-transparent p-0 shadow-none outline-none',
        'data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0 data-[state=open]:slide-in-from-bottom data-[state=closed]:slide-out-to-bottom',
        className,
      )}
      {...props}
    >
      {children}
      <DialogPrimitive.Close className="absolute right-4 top-4 rounded-sm opacity-70 ring-offset-background transition-opacity hover:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:pointer-events-none data-[state=open]:bg-accent data-[state=open]:text-muted-foreground">
        <X className="h-4 w-4" />
        <span className="sr-only">Close</span>
      </DialogPrimitive.Close>
    </DialogPrimitive.Content>
  </DialogPrimitive.Portal>
));
SheetContent.displayName = DialogPrimitive.Content.displayName;

// Unlike DialogHeader there is no `sm:text-left` pair: a bottom sheet is a
// panel, never a centered modal, so the alignment/padding stay with the
// caller's `className`.
const SheetHeader = ({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) => (
  <div className={cn('flex flex-col space-y-1.5', className)} {...props} />
);
SheetHeader.displayName = 'SheetHeader';

const SheetTitle = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Title>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Title>
>(({ className, ...props }, ref) => (
  <DialogPrimitive.Title
    ref={ref}
    className={cn(
      'font-heading text-lg font-semibold leading-none tracking-tight',
      className,
    )}
    {...props}
  />
));
SheetTitle.displayName = DialogPrimitive.Title.displayName;

export { Sheet, SheetContent, SheetHeader, SheetTitle, SheetClose };

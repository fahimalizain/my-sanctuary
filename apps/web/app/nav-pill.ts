// Breakpoint above which bottom-nav labels are visible. Must match
// Tailwind's default `md` (768px — not overridden in tailwind.config.js);
// nav-pill.test.ts pins this so the two can't drift apart.
export const NAV_LABEL_MIN_WIDTH_PX = 768;

// Destination box of the shared pill, from constants that do not animate
// (see measure() in routes/__root.tsx for why). Below the md breakpoint the
// label track is 0fr/clipped, so its width must not count: the pill would
// otherwise be wider than the icon-only tab it sits behind.
export function navPillDestination(args: {
  activeIndex: number;
  inactiveWidth: number;
  gap: number;
  labelWidth: number;
  showLabel: boolean;
}): { left: number; width: number } {
  return {
    left: args.activeIndex * (args.inactiveWidth + args.gap),
    width: args.inactiveWidth + (args.showLabel ? args.labelWidth : 0),
  };
}

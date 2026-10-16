import { useEffect, type RefObject } from 'react';
import { setNavHidden } from '@/app/nav-visibility';
import { shouldHideNavForLateHours } from '../lib/nav-late-hours';

/**
 * Fades the floating nav while the 11 PM row is on screen. Recomputes on
 * scroll, scroller resize, hourH (pinch-zoom) and allDayHeight changes, and
 * only touches the store when the boolean flips. Always restores the nav on
 * unmount so other routes never inherit a hidden nav.
 */
export function useHideNavOnLateHours(
  scrollerRef: RefObject<HTMLDivElement | null>,
  hourH: number,
  allDayHeight: number,
): void {
  useEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    let hidden = false;
    const update = () => {
      const next = shouldHideNavForLateHours({
        scrollTop: scroller.scrollTop,
        clientHeight: scroller.clientHeight,
        hourH,
        allDayHeight,
        currentlyHidden: hidden,
      });
      if (next === hidden) return;
      hidden = next;
      setNavHidden(next);
    };
    update();
    scroller.addEventListener('scroll', update, { passive: true });
    const observer = new ResizeObserver(update);
    observer.observe(scroller);
    return () => {
      scroller.removeEventListener('scroll', update);
      observer.disconnect();
      setNavHidden(false);
    };
  }, [scrollerRef, hourH, allDayHeight]);
}

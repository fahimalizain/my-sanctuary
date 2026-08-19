import { Outlet, Link, useLocation } from '@tanstack/react-router';
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';
import '../styles/globals.css';
import { Home, LayoutGrid, CalendarDays, Target, Settings } from 'lucide-react';
import { cn } from '@/lib/utils';
import { ReloadPrompt } from '@/app/components/ReloadPrompt';
declare const __APP_VERSION__: string;

const navItems = [
  { path: '/', label: 'Home', icon: Home },
  { path: '/board', label: 'Board', icon: LayoutGrid },
  { path: '/calendar', label: 'Calendar', icon: CalendarDays },
  { path: '/consistency', label: 'Consistency', icon: Target },
  { path: '/settings', label: 'Settings', icon: Settings },
];

function Navigation() {
  const location = useLocation();
  const rowRef = useRef<HTMLDivElement>(null);
  const labelRefs = useRef(new Map<string, HTMLSpanElement | null>());
  const [pillPosition, setPillPosition] = useState({ left: 0, width: 0 });
  // Until the first measurement the pill may not slide: it would otherwise
  // animate in from 0,0 on first paint.
  const [pillReady, setPillReady] = useState(false);

  // The Board entry also covers the dedicated categories editor.
  const activeIndex = navItems.findIndex((item) =>
    item.path === '/board'
      ? location.pathname === '/board' || location.pathname === '/categories'
      : location.pathname === item.path,
  );

  // Compute the pill's *destination* box from constants that do not animate:
  // - every item before the active one is icon-only, so their widths equal
  //   padding + icon width (read from computed styles of the first link)
  // - label.scrollWidth is the full text + pl-2 width even while the grid
  //   track is 0fr, because scrollWidth ignores the clipping/animated width
  // - the flex gap between items
  // left  = activeIndex * (inactiveWidth + gap)
  // width = inactiveWidth + activeLabel.scrollWidth
  const measure = useCallback(() => {
    const row = rowRef.current;
    if (!row) return;
    const activeItem = activeIndex >= 0 ? navItems[activeIndex] : undefined;
    const label = activeItem
      ? labelRefs.current.get(activeItem.path)
      : undefined;
    const firstLink = row.querySelector('a');
    if (!activeItem || !label || !firstLink) {
      // No tab is active (unmatched path): collapse the pill away.
      setPillPosition({ left: 0, width: 0 });
      setPillReady(false);
      return;
    }

    const linkStyle = getComputedStyle(firstLink);
    const gap = parseFloat(getComputedStyle(row).columnGap) || 0;
    const icon = firstLink.firstElementChild;
    const iconWidth = icon ? icon.getBoundingClientRect().width : 0;
    const inactiveWidth =
      (parseFloat(linkStyle.paddingLeft) || 0) +
      (parseFloat(linkStyle.paddingRight) || 0) +
      iconWidth;

    setPillPosition({
      left: activeIndex * (inactiveWidth + gap),
      width: inactiveWidth + label.scrollWidth,
    });
    setPillReady(true);
  }, [activeIndex, location.pathname]);

  // Measure before first paint (no fly-in from 0,0) and on every tab change.
  useLayoutEffect(() => {
    measure();
  }, [measure]);

  // Kumbh Sans / Sen load after first paint and change label widths.
  useEffect(() => {
    let cancelled = false;
    document.fonts?.ready.then(() => {
      if (!cancelled) {
        measure();
      }
    });
    return () => {
      cancelled = true;
    };
  }, [measure]);

  // Viewport / zoom changes resize the row.
  useEffect(() => {
    const row = rowRef.current;
    if (!row) return;
    const observer = new ResizeObserver(() => measure());
    observer.observe(row);
    return () => observer.disconnect();
  }, [measure]);

  // Don't show nav on login page
  if (location.pathname === '/login') {
    return null;
  }

  return (
    <nav className="fixed bottom-6 left-1/2 -translate-x-1/2 bg-card rounded-full shadow-lg border border-border px-2 py-2 z-50">
      <div ref={rowRef} className="relative flex items-center gap-1">
        {/* Shared pill that slides/resizes between items. */}
        <div
          aria-hidden="true"
          className={cn(
            'absolute inset-y-0 left-0 rounded-full bg-primary pointer-events-none z-0',
            pillReady &&
              // `duration-[240ms]` / `ease-[...]` collide with tailwindcss-animate's
              // duration/ease utilities and get dropped as ambiguous (Tailwind 3.4),
              // so duration & timing function are emitted as arbitrary properties.
              'transition-[transform,width] [transition-duration:240ms] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none',
          )}
          style={{
            transform: `translateX(${pillPosition.left}px)`,
            width: `${pillPosition.width}px`,
          }}
        />
        {navItems.map((item, index) => {
          const Icon = item.icon;
          const isActive = activeIndex === index;

          return (
            <Link
              key={item.path}
              to={item.path}
              aria-label={item.label}
              aria-current={isActive ? 'page' : undefined}
              className={cn(
                'relative z-10 flex items-center px-4 py-2 rounded-full transition-colors [transition-duration:240ms] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none',
                isActive
                  ? 'text-primary-foreground'
                  : 'text-muted-foreground hover:bg-muted',
              )}
            >
              <Icon className="h-5 w-5 shrink-0" />
              <span
                className={cn(
                  'grid transition-[grid-template-columns] [transition-duration:240ms] [transition-timing-function:cubic-bezier(0.22,1,0.36,1)] motion-reduce:transition-none',
                  isActive ? 'grid-cols-[1fr]' : 'grid-cols-[0fr]',
                )}
              >
                <span className="overflow-hidden min-w-0">
                  <span
                    ref={(el) => {
                      labelRefs.current.set(item.path, el);
                    }}
                    className="block text-sm font-medium whitespace-nowrap pl-2"
                  >
                    {item.label}
                  </span>
                </span>
              </span>
            </Link>
          );
        })}
      </div>
    </nav>
  );
}

function VersionBadge() {
  return (
    <div className="fixed bottom-1 left-1/2 -translate-x-1/2 z-40 text-[10px] text-muted-foreground/40 pointer-events-none select-none">
      v{__APP_VERSION__}
    </div>
  );
}

export function RootComponent() {
  return (
    <>
      <Outlet />
      <Navigation />
      <ReloadPrompt />
      <VersionBadge />
    </>
  );
}

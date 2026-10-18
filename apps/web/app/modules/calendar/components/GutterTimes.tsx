import { layoutGutterPills } from '../lib/gutter-time';
import { formatHourLabel } from '../lib/week-layout';

/** Same hex as NOW_LINE_COLOR in DayColumn.tsx (Notion --secondary500). */
const NOW_PILL_COLOR = '#F04842';

const PILL_CLASS =
  'absolute right-0.5 -translate-y-1/2 z-10 whitespace-nowrap rounded-full px-1 py-0.5 text-[10px] font-medium leading-none tabular-nums pointer-events-none';

const HOUR_LABEL_CLASS =
  'absolute right-1 -translate-y-1/2 text-[10px] leading-none text-muted-foreground tabular-nums select-none';

export interface GutterTimesProps {
  hourH: number;
  hourLabels: number[];
  /** Null when today is not in the rendered strip. */
  now: Date | null;
  /** Null when nothing timed is selected and no timed drag preview is active. */
  edges: { start: Date; end: Date } | null;
}

/**
 * Hour labels plus the now / start / end pills, inside the sticky gutter.
 * A pill's `top` is its center Y in the hours area, so the painted pill is
 * vertically centered on its instant.
 *
 * Decorative for now: the chip and the inspector already announce the time,
 * so the pills are `aria-hidden` until they become controls.
 */
export function GutterTimes({
  hourH,
  hourLabels,
  now,
  edges,
}: GutterTimesProps) {
  const { pills, hiddenHours } = layoutGutterPills({
    hourH,
    hourLabels,
    now,
    edges,
  });
  const hidden = new Set(hiddenHours);

  return (
    <>
      {hourLabels.map((h) => {
        const label = formatHourLabel(h);
        if (!label || hidden.has(h)) return null;
        return (
          <div key={h} className={HOUR_LABEL_CLASS} style={{ top: h * hourH }}>
            {label}
          </div>
        );
      })}

      {pills.map((pill) => (
        <div
          key={pill.key}
          aria-hidden
          className={
            pill.key === 'now'
              ? `${PILL_CLASS} text-white`
              : `${PILL_CLASS} bg-foreground text-background shadow-sm`
          }
          style={
            pill.key === 'now'
              ? { top: pill.top, backgroundColor: NOW_PILL_COLOR }
              : { top: pill.top }
          }
        >
          {pill.text}
        </div>
      ))}
    </>
  );
}

import {
  ALLDAY_CHIP,
  ALLDAY_GAP,
  ALLDAY_PAD,
  TIME_GUTTER_W,
} from './week-layout';
import { cn } from '@/lib/utils';

function hexToRgba(hex: string, alpha: number): string {
  const h = hex.replace('#', '');
  const full =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('')
      : h;
  const n = parseInt(full, 16);
  const r = (n >> 16) & 255;
  const g = (n >> 8) & 255;
  const b = n & 255;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export interface AllDayChip {
  id: string;
  title: string;
  /** Inclusive day index 0..6 within the visible week. */
  startDay: number;
  /** Inclusive day index 0..6 within the visible week. */
  endDay: number;
  lane: number;
  color: string;
}

export interface AllDayRowProps {
  height: number;
  chips: AllDayChip[];
  /** Optional: highlight today's column (0–6), or null. */
  todayIndex?: number | null;
}

export function AllDayRow({
  height,
  chips,
  todayIndex = null,
}: AllDayRowProps) {
  return (
    <div
      className="shrink-0 flex border-b border-border bg-cream"
      style={{ height }}
      data-allday-row
    >
      {/* Gutter label */}
      <div
        className="shrink-0 flex items-start justify-end pr-1.5 pt-1 border-r border-border/60"
        style={{ width: TIME_GUTTER_W }}
      >
        <span className="text-[10px] leading-none text-muted-foreground select-none">
          All-day
        </span>
      </div>

      {/* 7-day chip track */}
      <div className="relative flex-1 min-w-0">
        {/* Column hairlines + optional today tint */}
        <div className="absolute inset-0 flex pointer-events-none">
          {Array.from({ length: 7 }, (_, i) => (
            <div
              key={i}
              className={cn(
                'flex-1 min-w-0 border-r border-border/40 last:border-r-0',
                todayIndex === i && 'bg-primary/[0.03]',
              )}
            />
          ))}
        </div>

        {chips.map((chip) => {
          const daySpan = chip.endDay - chip.startDay + 1;
          const leftPct = (chip.startDay / 7) * 100;
          const widthPct = (daySpan / 7) * 100;
          const top =
            (ALLDAY_CHIP + ALLDAY_GAP) * chip.lane + ALLDAY_PAD;

          return (
            <div
              key={chip.id}
              className="absolute overflow-hidden rounded px-1.5 pointer-events-auto"
              style={{
                top,
                height: ALLDAY_CHIP,
                left: `calc(${leftPct}% + 1px)`,
                width: `calc(${widthPct}% - 2px)`,
                backgroundColor: hexToRgba(chip.color, 0.28),
                borderLeft: `3px solid ${chip.color}`,
                color: 'var(--foreground)',
              }}
              title={chip.title}
            >
              <div className="truncate text-[11px] font-medium leading-[19px]">
                {chip.title}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

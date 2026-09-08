import {
  ALLDAY_CHIP,
  ALLDAY_GAP,
  ALLDAY_PAD,
  TIME_GUTTER_W,
  hexToRgba,
} from './week-layout';
import { cn } from '@/lib/utils';

const CHIP_FILL_ALPHA = 0.22;

/** Mon-start week: Sat = 5, Sun = 6. */
function isWeekendIndex(dayIndex: number): boolean {
  return dayIndex === 5 || dayIndex === 6;
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
        {/* Column hairlines + weekend / today wash (today wins) */}
        <div className="absolute inset-0 flex pointer-events-none">
          {Array.from({ length: 7 }, (_, i) => {
            const weekend = isWeekendIndex(i);
            const isToday = todayIndex === i;
            return (
              <div
                key={i}
                className={cn(
                  'flex-1 min-w-0 border-r last:border-r-0',
                  weekend ? 'border-border/60' : 'border-border/40',
                  isToday
                    ? 'bg-primary/[0.03]'
                    : weekend && 'bg-muted/40',
                )}
              />
            );
          })}
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
              className="absolute overflow-hidden rounded-[6px] pointer-events-auto"
              style={{
                top,
                height: ALLDAY_CHIP,
                left: `calc(${leftPct}% + 1px)`,
                width: `calc(${widthPct}% - 2px)`,
                color: 'var(--foreground)',
              }}
              title={chip.title}
            >
              {/* 4px left ribbon */}
              <div
                className="absolute left-0 top-0 bottom-0 w-1 rounded-l-[6px]"
                style={{ backgroundColor: chip.color }}
                aria-hidden
              />
              <div
                className="h-full pl-2 pr-1 py-px"
                style={{
                  backgroundColor: hexToRgba(chip.color, CHIP_FILL_ALPHA),
                }}
              >
                <div className="truncate text-[11px] font-medium leading-[17px]">
                  {chip.title}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

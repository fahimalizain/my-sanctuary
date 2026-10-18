// The expanded card's evidence, hand-rolled SVG (no chart library).
//
// Every chart is a function of the rule's own evidence array: the API already
// decided what to send (paired nights only, weekdays in the window, strip-day
// sums), so nothing here re-scores or re-samples anything. An empty series
// renders one muted line instead of an empty frame.

import type {
  EvidenceDay,
  EvidenceNight,
  EvidencePoint,
  EvidenceWeekday,
  RuleScore,
} from '@/app/types';
import { parseRuleParams, targetHoursFromCondition } from './rules-model';

const PLOT_WIDTH = 280;
const MUTED_FILL = 'fill-muted-foreground';
const TARGET_STROKE = 'stroke-muted-foreground/40';

/** Minutes past noon the bed lane spans (10:00 → 15:00). */
const BED_MIN = 600;
const BED_MAX = 900;
/** Minutes past midnight the wake lane spans (00:00 → 08:00). */
const WAKE_MIN = 0;
const WAKE_MAX = 480;

export function RuleEvidence({ rule }: { rule: RuleScore }) {
  const evidence = rule.evidence;
  switch (evidence.type) {
    case 'category_total':
      return (
        <TotalBars
          name={rule.name}
          points={evidence.points}
          target={targetHoursFromCondition(rule.condition)}
        />
      );
    case 'category_days':
      return <DayDots name={rule.name} days={evidence.days} />;
    case 'category_daily_avg':
      return (
        <WeekdayBars
          name={rule.name}
          weekdays={evidence.weekdays}
          target={dailyAvgTarget(rule)}
        />
      );
    case 'sleep_duration':
    case 'sleep_timing':
      return <SleepLanes name={rule.name} nights={evidence.nights} />;
  }
}

/** `target_hours` from a daily-average rule's params, or null when the stored
 *  params cannot be read (the chart simply skips the line). */
function dailyAvgTarget(rule: RuleScore): number | null {
  if (rule.kind !== 'category_daily_avg') return null;
  try {
    const params = parseRuleParams(rule.kind, rule.params);
    return 'target_hours' in params ? params.target_hours : null;
  } catch {
    return null;
  }
}

function EmptyEvidence() {
  return (
    <p className="text-xs italic text-muted-foreground">
      Not enough history yet.
    </p>
  );
}

function Caption({ children }: { children: React.ReactNode }) {
  return <p className="mb-1 text-xs text-muted-foreground">{children}</p>;
}

// ──────────────────────────────────────────
// category_total — rolling window sums
// ──────────────────────────────────────────

function TotalBars({
  name,
  points,
  target,
}: {
  name: string;
  points: EvidencePoint[];
  target: number | null;
}) {
  const values = points
    .map((point) => point.hours)
    .filter((hours): hours is number => hours !== null);
  if (values.length === 0) return <EmptyEvidence />;

  const height = 84;
  const baseline = height - 14;
  const top = 6;
  // The target joins the scale so its line never leaves the frame.
  const max = Math.max(1, ...values, target ?? 0);
  const slot = PLOT_WIDTH / points.length;
  const barWidth = Math.max(2, slot - 2);
  const y = (hours: number) => baseline - (hours / max) * (baseline - top);

  return (
    <div>
      <Caption>
        Window total — {points.length} days, peak{' '}
        {Math.round(Math.max(...values) * 10) / 10}h
      </Caption>
      <svg
        viewBox={`0 0 ${PLOT_WIDTH} ${height}`}
        role="img"
        className="h-auto w-full max-w-[280px]"
      >
        <title>
          {name}: rolling window hours for the last {points.length} days
          {target === null ? '' : `, target ${target}h`}
        </title>
        <line
          x1={0}
          y1={baseline}
          x2={PLOT_WIDTH}
          y2={baseline}
          className={TARGET_STROKE}
          strokeWidth={1}
        />
        {points.map((point, index) =>
          point.hours === null ? null : (
            <rect
              key={point.date}
              x={index * slot + 1}
              y={y(point.hours)}
              width={barWidth}
              height={baseline - y(point.hours)}
              rx={1}
              className="fill-primary"
            >
              <title>{`${point.date} ${point.hours}h`}</title>
            </rect>
          ),
        )}
        {target !== null && (
          <line
            x1={0}
            y1={y(target)}
            x2={PLOT_WIDTH}
            y2={y(target)}
            className={TARGET_STROKE}
            strokeWidth={1}
            strokeDasharray="4 3"
          />
        )}
      </svg>
    </div>
  );
}

// ──────────────────────────────────────────
// category_days — the counted days
// ──────────────────────────────────────────

function DayDots({ name, days }: { name: string; days: EvidenceDay[] }) {
  if (days.length === 0) return <EmptyEvidence />;

  const cell = 14;
  const radius = 4.5;
  // One wrapping grid, at most 14 columns wide — the array is already capped
  // by the rule's own window, so the grid never invents days.
  const columns = Math.min(days.length, 14) || 1;
  const rows = Math.ceil(days.length / columns);
  const width = columns * cell;
  const height = rows * cell;
  const hits = days.filter((day) => day.hit).length;

  return (
    <div>
      <Caption>
        Days hit — {hits} of {days.length}
      </Caption>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        className="h-auto w-full max-w-[280px]"
      >
        <title>
          {name}: {hits} of {days.length} counted days cleared the minimum
        </title>
        {days.map((day, index) => (
          <circle
            key={day.date}
            cx={(index % columns) * cell + cell / 2}
            cy={Math.floor(index / columns) * cell + cell / 2}
            r={radius}
            fill="currentColor"
            className={day.hit ? 'text-primary' : 'text-destructive/30'}
          >
            <title>{`${day.date} ${day.hit ? 'hit' : 'miss'}`}</title>
          </circle>
        ))}
      </svg>
    </div>
  );
}

// ──────────────────────────────────────────
// category_daily_avg — the selected weekdays
// ──────────────────────────────────────────

function WeekdayBars({
  name,
  weekdays,
  target,
}: {
  name: string;
  weekdays: EvidenceWeekday[];
  target: number | null;
}) {
  if (weekdays.length === 0) return <EmptyEvidence />;

  const height = 84;
  const baseline = height - 14;
  const top = 6;
  const max = Math.max(1, ...weekdays.map((day) => day.hours), target ?? 0);
  const slot = PLOT_WIDTH / weekdays.length;
  const barWidth = Math.max(2, slot - 2);
  const y = (hours: number) => baseline - (hours / max) * (baseline - top);
  const counted = weekdays.filter((day) => !day.excluded);

  return (
    <div>
      <Caption>
        Weekday mean — {counted.length} of {weekdays.length} days counted
        {weekdays.length > counted.length ? ' (travel days skipped)' : ''}
      </Caption>
      <svg
        viewBox={`0 0 ${PLOT_WIDTH} ${height}`}
        role="img"
        className="h-auto w-full max-w-[280px]"
      >
        <title>
          {name}: hours on the last {weekdays.length} selected weekdays
          {target === null ? '' : `, target ${target}h`}
        </title>
        <line
          x1={0}
          y1={baseline}
          x2={PLOT_WIDTH}
          y2={baseline}
          className={TARGET_STROKE}
          strokeWidth={1}
        />
        {weekdays.map((day, index) => (
          <rect
            key={day.date}
            x={index * slot + 1}
            y={y(day.hours)}
            width={barWidth}
            height={Math.max(1, baseline - y(day.hours))}
            rx={1}
            fill="currentColor"
            className={
              day.excluded ? 'text-muted-foreground/30' : 'text-primary'
            }
          >
            <title>
              {`${day.date} ${day.hours}h${day.excluded ? ' excluded' : ''}`}
            </title>
          </rect>
        ))}
        {target !== null && (
          <line
            x1={0}
            y1={y(target)}
            x2={PLOT_WIDTH}
            y2={y(target)}
            className={TARGET_STROKE}
            strokeWidth={1}
            strokeDasharray="4 3"
          />
        )}
      </svg>
    </div>
  );
}

// ──────────────────────────────────────────
// sleep — bedtime and wake, one lane each
// ──────────────────────────────────────────

function SleepLanes({
  name,
  nights,
}: {
  name: string;
  nights: EvidenceNight[];
}) {
  if (nights.length === 0) return <EmptyEvidence />;

  const leftPad = 26;
  const column = 16;
  const laneHeight = 34;
  const laneGap = 8;
  const labelRow = 12;
  const width = leftPad + nights.length * column + 4;
  const bedTop = 0;
  const wakeTop = laneHeight + laneGap;
  const height = wakeTop + laneHeight + labelRow;

  const bedY = (minutes: number) =>
    bedTop +
    ((BED_MAX - clamp(minutes, BED_MIN, BED_MAX)) / (BED_MAX - BED_MIN)) *
      laneHeight;
  const wakeY = (minutes: number) =>
    wakeTop +
    ((clamp(minutes, WAKE_MIN, WAKE_MAX) - WAKE_MIN) / (WAKE_MAX - WAKE_MIN)) *
      laneHeight;
  const x = (index: number) => leftPad + index * column + column / 2;

  return (
    <div>
      <Caption>
        Nights — {nights.length} paired, newest{' '}
        {nights[nights.length - 1].wake_date}
      </Caption>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        className="h-auto w-full max-w-[280px]"
      >
        <title>
          {name}: bedtime and wake for the last {nights.length} paired nights
        </title>
        <line
          x1={0}
          y1={bedTop + laneHeight}
          x2={width}
          y2={bedTop + laneHeight}
          className={TARGET_STROKE}
          strokeWidth={1}
        />
        <line
          x1={0}
          y1={wakeTop + laneHeight}
          x2={width}
          y2={wakeTop + laneHeight}
          className={TARGET_STROKE}
          strokeWidth={1}
        />
        <text
          x={2}
          y={bedTop + laneHeight / 2}
          dominantBaseline="middle"
          fontSize={9}
          className={MUTED_FILL}
        >
          bed
        </text>
        <text
          x={2}
          y={wakeTop + laneHeight / 2}
          dominantBaseline="middle"
          fontSize={9}
          className={MUTED_FILL}
        >
          wake
        </text>
        {nights.map((night, index) => (
          <circle
            key={`${night.wake_date}-bed`}
            cx={x(index)}
            cy={bedY(night.bed_min_past_noon)}
            r={3}
            fill="currentColor"
            className="text-primary"
          >
            <title>{`${night.wake_date} bed ${clockPastNoon(night.bed_min_past_noon)}`}</title>
          </circle>
        ))}
        {nights.map((night, index) => (
          <circle
            key={`${night.wake_date}-wake`}
            cx={x(index)}
            cy={wakeY(night.wake_min_past_midnight)}
            r={3}
            fill="currentColor"
            className="text-work-blue"
          >
            <title>{`${night.wake_date} wake ${clockPastMidnight(night.wake_min_past_midnight)}`}</title>
          </circle>
        ))}
        <text x={leftPad} y={height - 2} fontSize={8} className={MUTED_FILL}>
          {nights[0].wake_date}
        </text>
        <text
          x={width - 2}
          y={height - 2}
          fontSize={8}
          textAnchor="end"
          className={MUTED_FILL}
        >
          {nights[nights.length - 1].wake_date}
        </text>
      </svg>
    </div>
  );
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function clockPastNoon(minutes: number): string {
  return clock(minutes + 12 * 60);
}

function clockPastMidnight(minutes: number): string {
  return clock(minutes);
}

function clock(totalMinutes: number): string {
  const wrapped =
    ((Math.round(totalMinutes) % (24 * 60)) + 24 * 60) % (24 * 60);
  const hours = Math.floor(wrapped / 60);
  return `${String(hours).padStart(2, '0')}:${String(wrapped % 60).padStart(2, '0')}`;
}

// "The Record" (reports slice 5): the descriptive report, collapsed under the
// board. No targets, no red/green — every number is a reading, the delta is
// neutral text, and the only colours are the categories' own.
//
// The section fetches only once it is opened (`enabled: open`) and never
// blocks the scoreboard above it: a load failure renders inside the details
// and the cards stay mounted.

import { useState, type ReactNode } from 'react';
import { Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { useRecordQuery } from '@/app/queries/record';
import type {
  RecordDay,
  RecordHeatmapRow,
  RecordRankingRow,
  RecordReport,
  RecordRangeKey,
  RecordTrendPoint,
  RecordYearRow,
} from '@/app/types';
import {
  clockLabel,
  dayTickIndexes,
  deltaLabel,
  heatmapMax,
  heatmapOpacity,
  hoursLabel,
  hoursText,
  legendEntries,
  medianHoursLabel,
  RECORD_RANGES,
  RECORD_WEEKDAY_LABELS,
  shareLabel,
  shortDate,
  stackDay,
} from './record-model';

/** The shared card chrome inside the details. */
const CARD = 'rounded-xl border border-border bg-card p-4';

export function TheRecord() {
  const [open, setOpen] = useState(false);
  const [range, setRange] = useState<RecordRangeKey>('30d');
  const query = useRecordQuery({ range, enabled: open });
  const report = query.data;
  const loadError =
    query.error instanceof Error
      ? query.error.message
      : query.error
        ? 'Failed to read the record'
        : null;

  return (
    <details
      className="mt-12 overflow-hidden rounded-xl border border-border bg-card/50"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="cursor-pointer select-none px-4 py-3">
        <span className="font-heading text-lg font-semibold text-foreground">
          The Record
        </span>
        <span className="ml-2 text-sm text-muted-foreground">
          What the diary actually contained. No targets.
        </span>
      </summary>

      <div className="space-y-8 border-t border-border p-4">
        <div className="flex flex-wrap items-center gap-2">
          {RECORD_RANGES.map((option) => (
            <Button
              key={option.key}
              variant={option.key === range ? 'default' : 'outline'}
              size="sm"
              onClick={() => setRange(option.key)}
            >
              {option.label}
            </Button>
          ))}
          {report && (
            <span className="ml-1 text-xs text-muted-foreground">
              {report.from} → {report.to} · {report.time_zone}
            </span>
          )}
        </div>

        {query.isLoading && (
          <div className="flex items-center justify-center gap-2 py-16 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Reading the record…
          </div>
        )}

        {loadError && (
          <div className="flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
            <p className="selectable text-sm">{loadError}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void query.refetch();
              }}
            >
              Retry
            </Button>
          </div>
        )}

        {report && (
          <>
            <DaysCard days={report.days} />
            <RankingCard ranking={report.ranking} />
            <TrendCard trend={report.trend} />
            <HeatmapCard heatmap={report.heatmap} />
            <QualityCard report={report} />
            <YearsCard years={report.years} />
          </>
        )}
      </div>
    </details>
  );
}

// ──────────────────────────────────────────
// 1. Days
// ──────────────────────────────────────────

function DaysCard({ days }: { days: RecordDay[] }) {
  const width = 720;
  const height = 132;
  const baseline = height - 16;
  const top = 6;
  const plotHeight = baseline - top;
  const slot = days.length > 0 ? width / days.length : width;
  const barWidth = Math.max(1, slot - 1);
  const ticks = dayTickIndexes(days.length);
  const legend = legendEntries(days);
  const tracked = days.reduce(
    (total, day) =>
      total + day.segments.reduce((sum, segment) => sum + segment.seconds, 0),
    0,
  );

  return (
    <section className={CARD}>
      <Caption>
        Days — {days.length} days, {hoursLabel(tracked)} tracked. A full column
        is a full 24h.
      </Caption>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        className="h-auto w-full"
      >
        <title>
          Tracked time per day, stacked by category against a full day
        </title>
        <line
          x1={0}
          y1={baseline}
          x2={width}
          y2={baseline}
          className="stroke-muted-foreground/40"
          strokeWidth={1}
        />
        {days.map((day, index) => {
          const stacked = stackDay(day, plotHeight);
          const x = index * slot + (slot - barWidth) / 2;
          const total = day.segments.reduce(
            (sum, segment) => sum + segment.seconds,
            0,
          );
          return (
            <g key={day.date}>
              {/* The whole column is the hover target; the segments paint
                  over it so a bar keeps its own tooltip. */}
              <rect
                x={x}
                y={top}
                width={barWidth}
                height={plotHeight}
                fill="transparent"
              >
                <title>
                  {`${day.date} tracked ${hoursLabel(total)}${
                    day.untracked_seconds === null
                      ? ' · today'
                      : ` · untracked ${hoursLabel(day.untracked_seconds)}`
                  }`}
                </title>
              </rect>
              {stacked.map((segment) => (
                <rect
                  key={segment.categoryId}
                  x={x}
                  y={top + segment.y}
                  width={barWidth}
                  height={segment.height}
                  fill={segment.color}
                >
                  <title>{`${day.date} ${segment.title} ${hoursLabel(segment.seconds)}`}</title>
                </rect>
              ))}
            </g>
          );
        })}
        {ticks.map((index) => (
          <text
            key={days[index].date}
            x={index * slot + slot / 2}
            y={height - 4}
            fontSize={9}
            textAnchor="middle"
            className="fill-muted-foreground"
          >
            {shortDate(days[index].date)}
          </text>
        ))}
      </svg>

      {legend.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1">
          {legend.map((entry) => (
            <span
              key={entry.categoryId || 'unclassified'}
              className="flex items-center gap-1.5 text-xs text-muted-foreground"
            >
              <span
                className="h-2.5 w-2.5 rounded-sm"
                style={{ backgroundColor: entry.color }}
              />
              {entry.title}
            </span>
          ))}
          {legend.length >= 12 && (
            <span className="text-xs text-muted-foreground">…and more</span>
          )}
        </div>
      )}
    </section>
  );
}

// ──────────────────────────────────────────
// 2. Ranking
// ──────────────────────────────────────────

function RankingCard({ ranking }: { ranking: RecordRankingRow[] }) {
  if (ranking.length === 0) {
    return (
      <section className={CARD}>
        <Caption>Ranking</Caption>
        <Empty>Nothing tracked in this range.</Empty>
      </section>
    );
  }

  return (
    <section className={CARD}>
      <Caption>
        Ranking — this range, with the window before it beside each row.
      </Caption>
      <div className="space-y-2">
        {ranking.map((row) => (
          <div
            key={row.category_id || 'unclassified'}
            className="flex items-center gap-3"
          >
            <span
              className="h-2.5 w-2.5 flex-shrink-0 rounded-sm"
              style={{ backgroundColor: row.color }}
            />
            <span
              className="w-28 flex-shrink-0 truncate text-sm text-foreground"
              title={row.title}
            >
              {row.title}
            </span>
            <span className="h-2 flex-1 overflow-hidden rounded-sm bg-muted">
              <span
                className="block h-full rounded-sm"
                style={{
                  width: `${Math.max(0, Math.min(1, row.share)) * 100}%`,
                  backgroundColor: row.color,
                }}
              />
            </span>
            <span className="w-14 flex-shrink-0 text-right text-sm text-foreground">
              {hoursLabel(row.seconds)}
            </span>
            <span className="w-10 flex-shrink-0 text-right text-xs text-muted-foreground">
              {shareLabel(row.share)}
            </span>
            <span className="w-14 flex-shrink-0 text-right text-xs text-foreground">
              {deltaLabel(row.seconds, row.previous_seconds)}
            </span>
          </div>
        ))}
      </div>
    </section>
  );
}

// ──────────────────────────────────────────
// 3. Productive trend
// ──────────────────────────────────────────

function TrendCard({ trend }: { trend: RecordTrendPoint[] }) {
  const values = trend.flatMap((point) =>
    [point.ma30, point.ma60].filter((value): value is number => value !== null),
  );
  if (values.length === 0) {
    return (
      <section className={CARD}>
        <Caption>Productive trend</Caption>
        <Empty>Not enough history yet.</Empty>
      </section>
    );
  }

  const width = 720;
  const height = 140;
  const baseline = height - 16;
  const top = 8;
  const max = Math.max(1, ...values);
  const step = trend.length > 1 ? width / (trend.length - 1) : 0;
  const x = (index: number) => (trend.length > 1 ? index * step : width / 2);
  const y = (hours: number) => baseline - (hours / max) * (baseline - top);
  const series = (key: 'ma30' | 'ma60') =>
    trend
      .map((point, index) =>
        point[key] === null ? null : `${x(index)},${y(point[key])}`,
      )
      .filter((pair): pair is string => pair !== null)
      .join(' ');

  return (
    <section className={CARD}>
      <Caption>Productive trend — peak mean {hoursText(max)} per day.</Caption>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        className="h-auto w-full"
      >
        <title>Productive hours per day, 30-day and 60-day means</title>
        <line
          x1={0}
          y1={baseline}
          x2={width}
          y2={baseline}
          className="stroke-muted-foreground/40"
          strokeWidth={1}
        />
        <polyline
          points={series('ma60')}
          fill="none"
          className="stroke-muted-foreground"
          strokeWidth={1.5}
        />
        <polyline
          points={series('ma30')}
          fill="none"
          className="stroke-primary"
          strokeWidth={2}
        />
        {trend.map((point, index) =>
          point.hours > 0 ? (
            <circle
              key={point.date}
              cx={x(index)}
              cy={y(point.hours)}
              r={1.5}
              className="fill-muted-foreground/50"
            >
              <title>{`${point.date} ${hoursText(point.hours)}`}</title>
            </circle>
          ) : null,
        )}
        <text
          x={0}
          y={height - 4}
          fontSize={9}
          className="fill-muted-foreground"
        >
          {shortDate(trend[0].date)}
        </text>
        <text
          x={width}
          y={height - 4}
          fontSize={9}
          textAnchor="end"
          className="fill-muted-foreground"
        >
          {shortDate(trend[trend.length - 1].date)}
        </text>
      </svg>
      <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <span className="h-0.5 w-4 rounded-full bg-primary" />
          30-day
        </span>
        <span className="flex items-center gap-1.5">
          <span className="h-0.5 w-4 rounded-full bg-muted-foreground" />
          60-day
        </span>
      </div>
    </section>
  );
}

// ──────────────────────────────────────────
// 4. Hour × weekday
// ──────────────────────────────────────────

function HeatmapCard({ heatmap }: { heatmap: RecordHeatmapRow[] }) {
  const max = heatmapMax(heatmap);
  return (
    <section className={CARD}>
      <Caption>
        Hour × weekday — mean productive time per clock hour, over every
        occurrence of that weekday in the range.
      </Caption>
      <div className="space-y-1">
        {heatmap.map((row) => (
          <div key={row.weekday} className="flex items-center gap-2">
            <span className="w-8 flex-shrink-0 text-xs text-muted-foreground">
              {RECORD_WEEKDAY_LABELS[row.weekday]}
            </span>
            <div className="flex flex-1 gap-[2px]">
              {row.seconds.map((seconds, hour) => (
                <div
                  key={hour}
                  className={
                    seconds > 0
                      ? 'h-4 flex-1 rounded-sm bg-primary'
                      : 'h-4 flex-1 rounded-sm bg-muted'
                  }
                  style={
                    seconds > 0
                      ? { opacity: heatmapOpacity(seconds, max) }
                      : undefined
                  }
                  title={`${RECORD_WEEKDAY_LABELS[row.weekday]} ${clockLabel(hour)} · ${hoursLabel(seconds)}`}
                />
              ))}
            </div>
          </div>
        ))}
        <div className="flex items-center gap-2">
          <span className="w-8 flex-shrink-0" />
          <div className="flex flex-1 gap-[2px]">
            {Array.from({ length: 24 }, (_, hour) => (
              <span
                key={hour}
                className="flex-1 text-[9px] text-muted-foreground"
              >
                {hour % 6 === 0 ? hour : ''}
              </span>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

// ──────────────────────────────────────────
// 5. Data quality
// ──────────────────────────────────────────

function QualityCard({ report }: { report: RecordReport }) {
  const { estimated_nights: nights, unclassified_titles: titles } =
    report.quality;

  return (
    <section className={CARD}>
      <Caption>Data quality</Caption>
      {nights.length === 0 && titles.length === 0 ? (
        <Empty>Nothing to flag.</Empty>
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div>
            <p className="mb-1 text-sm text-foreground">
              Estimated nights — {nights.length}
            </p>
            {nights.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                Every night had both markers.
              </p>
            ) : (
              <ul className="space-y-0.5 text-xs text-muted-foreground">
                {nights.slice(0, 8).map((night) => (
                  <li key={`${night.wake_date}-${night.source}`}>
                    {night.wake_date} · {night.source}
                  </li>
                ))}
                {nights.length > 8 && <li>…{nights.length - 8} more</li>}
              </ul>
            )}
          </div>
          <div>
            <p className="mb-1 text-sm text-foreground">
              Unclassified titles — {titles.length}
            </p>
            {titles.length === 0 ? (
              <p className="text-xs text-muted-foreground">
                Everything was filed.
              </p>
            ) : (
              <ul className="space-y-0.5 text-xs text-muted-foreground">
                {titles.map((title) => (
                  <li key={title.title} className="selectable">
                    {title.title} · {title.count}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

// ──────────────────────────────────────────
// 6. Years
// ──────────────────────────────────────────

function YearsCard({ years }: { years: RecordYearRow[] }) {
  return (
    <section className={CARD}>
      <Caption>Years — the whole history, not the range above.</Caption>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-left text-xs text-muted-foreground">
              <th className="py-1 pr-3 font-normal">Year</th>
              <th className="py-1 pr-3 font-normal">Productive / day</th>
              <th className="py-1 pr-3 font-normal">Weekday</th>
              <th className="py-1 pr-3 font-normal">Weekend</th>
              <th className="py-1 pr-3 font-normal">Sleep median</th>
              <th className="py-1 font-normal">Paired nights</th>
            </tr>
          </thead>
          <tbody>
            {years.map((year) => (
              <tr key={year.year} className="border-t border-border/60">
                <td className="py-1 pr-3 text-foreground">{year.year}</td>
                <td className="py-1 pr-3 text-foreground">
                  {hoursText(year.productive_hours_per_day)}
                </td>
                <td className="py-1 pr-3 text-muted-foreground">
                  {hoursText(year.weekday_hours)}
                </td>
                <td className="py-1 pr-3 text-muted-foreground">
                  {hoursText(year.weekend_hours)}
                </td>
                <td className="py-1 pr-3 text-muted-foreground">
                  {medianHoursLabel(year.sleep_median_hours)}
                </td>
                <td className="py-1 text-muted-foreground">
                  {year.paired_nights}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

// ──────────────────────────────────────────
// Shared bits
// ──────────────────────────────────────────

function Caption({ children }: { children: ReactNode }) {
  return <p className="mb-3 text-xs text-muted-foreground">{children}</p>;
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="text-xs italic text-muted-foreground">{children}</p>;
}

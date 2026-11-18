// "Consumption" (reports slice 3): the fixed 14-day table of platforms by
// gross logged time, above The Record. Descriptive only — no targets, no
// red/green on the numbers, and the group column reads as words.
//
// Unlike The Record the section starts open, so it owns its `open` state
// (The Record only listens to `onToggle`). It fetches once opened and never
// blocks the board above it: a load failure renders inside the details and
// the cards stay mounted.

import { useState } from 'react';
import { Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { useConsumptionQuery } from '@/app/queries/consumption';
import type { ConsumptionReport } from '@/app/types';
import { hoursLabel } from './record-model';
import {
  consumptionGroupLabel,
  consumptionGroupLine,
  consumptionShareLabel,
  durationLabel,
} from './consumption-model';

export function Consumption() {
  const [open, setOpen] = useState(true);
  const query = useConsumptionQuery({ enabled: open });
  const report = query.data;
  const loadError =
    query.error instanceof Error
      ? query.error.message
      : query.error
        ? 'Failed to read consumption'
        : null;

  return (
    <details
      open={open}
      className="mt-12 overflow-hidden rounded-xl border border-border bg-card/50"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="cursor-pointer select-none px-4 py-3">
        <span className="font-heading text-lg font-semibold text-foreground">
          Consumption
        </span>
        <span className="ml-2 text-sm text-muted-foreground">
          Last 14 days, as logged.
        </span>
      </summary>

      <div className="space-y-4 border-t border-border p-4">
        {query.isLoading && (
          <div className="flex items-center justify-center gap-2 py-16 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Reading consumption…
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
            <p className="text-xs text-muted-foreground">
              {report.from} → {report.to} · {report.time_zone}
            </p>
            {report.rows.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                Nothing logged in the last 14 days.
              </p>
            ) : (
              <ConsumptionTable report={report} />
            )}
            <GroupLine report={report} />
          </>
        )}
      </div>
    </details>
  );
}

function ConsumptionTable({ report }: { report: ConsumptionReport }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="text-left text-xs text-muted-foreground">
            <th className="py-1 pr-3 font-normal">Platform</th>
            <th className="py-1 pr-3 font-normal">Group</th>
            <th className="py-1 pr-3 text-right font-normal">Time</th>
            <th className="py-1 pr-3 text-right font-normal">Share</th>
            <th className="py-1 pr-3 text-right font-normal">Sessions</th>
            <th className="py-1 pr-3 text-right font-normal">Avg</th>
            <th className="py-1 text-right font-normal">Longest</th>
          </tr>
        </thead>
        <tbody>
          {report.rows.map((row) => (
            <tr
              key={`${row.group}-${row.platform}`}
              className="border-t border-border/60"
            >
              <td className="py-1 pr-3 text-foreground">{row.platform}</td>
              <td className="py-1 pr-3 text-muted-foreground">
                {consumptionGroupLabel(row.group)}
              </td>
              <td className="py-1 pr-3 text-right tabular-nums text-foreground">
                {hoursLabel(row.seconds)}
              </td>
              <td className="py-1 pr-3 text-right tabular-nums text-muted-foreground">
                {consumptionShareLabel(row.seconds, report.total_seconds)}
              </td>
              <td className="py-1 pr-3 text-right tabular-nums text-muted-foreground">
                {row.sessions}
              </td>
              <td className="py-1 pr-3 text-right tabular-nums text-muted-foreground">
                {row.sessions > 0
                  ? durationLabel(row.seconds / row.sessions)
                  : '—'}
              </td>
              <td className="py-1 text-right tabular-nums text-muted-foreground">
                {durationLabel(row.longest_seconds)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The group totals in the server's order, one muted line. Absent when the
 *  window has no groups at all. */
function GroupLine({ report }: { report: ConsumptionReport }) {
  const line = consumptionGroupLine(report.groups, report.total_seconds);
  if (!line) return null;
  return <p className="text-xs text-muted-foreground">{line}</p>;
}

import { queryOptions, useQuery } from '@tanstack/react-query';
import { getConsumption } from '@/lib/api';
import { queryKeys } from './keys';

// "Consumption" (reports slice 3). One fixed window, so one key and no range:
// changing The Record's picker must never refetch this. Nothing here
// invalidates anything else — the report is descriptive and never feeds the
// board.

export function consumptionQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.consumption.all,
    queryFn: () => getConsumption(),
  });
}

/** The report read. `enabled` keeps it off until the section is opened — and
 *  it is never polled: the window only moves when the server re-assembles it. */
export function useConsumptionQuery(opts: { enabled: boolean }) {
  return useQuery({
    ...consumptionQueryOptions(),
    enabled: opts.enabled,
  });
}

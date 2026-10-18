import { queryOptions, useQuery } from '@tanstack/react-query';
import { getRecord } from '@/lib/api';
import type { RecordRangeKey } from '@/app/types';
import { queryKeys } from './keys';

// "The Record" (reports slice 5). One read per range; the range is part of the
// key, so switching the picker is a new query and switching back is a cache
// hit. Nothing here invalidates anything else — the report is descriptive and
// never feeds the board.

export function recordQueryOptions(range: RecordRangeKey) {
  return queryOptions({
    queryKey: queryKeys.record.range(range),
    queryFn: () => getRecord(range),
  });
}

/** The report read. `enabled` keeps it off until the section is opened. */
export function useRecordQuery(opts: {
  range: RecordRangeKey;
  enabled: boolean;
}) {
  return useQuery({
    ...recordQueryOptions(opts.range),
    enabled: opts.enabled,
  });
}

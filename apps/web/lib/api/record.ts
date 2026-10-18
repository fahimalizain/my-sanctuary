import { fetchJson } from './client';
import type { RecordRangeKey, RecordReport } from '@/app/types';

// `/api/reports/record` (reports slice 5) — "The Record". Descriptive only:
// the server sends integer seconds and the client divides by 3600 for hours.
// `range` is required in practice (the server defaults to `30d` when it is
// absent), so the client always names one.

export function getRecord(range: RecordRangeKey): Promise<RecordReport> {
  return fetchJson<RecordReport>(`/api/reports/record?range=${range}`);
}

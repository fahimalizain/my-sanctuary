import { fetchJson } from './client';
import type { ConsumptionReport } from '@/app/types';

// `/api/reports/consumption` (reports slice 3) — the fixed 14-day window.
// No range parameter: the server owns the window and echoes `from`/`to`, and
// the client only formats the integer seconds and event counts it sends.

export function getConsumption(): Promise<ConsumptionReport> {
  return fetchJson<ConsumptionReport>('/api/reports/consumption');
}

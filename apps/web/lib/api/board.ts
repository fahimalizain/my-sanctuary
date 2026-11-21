import { fetchJson } from './client';
import type { PlannedBoardResponse } from '@/app/types';

/** The Planned Board's date-grouped read (ADR 0014): every living `PLANNED`
 *  task with its agenda row and the server-computed `undated` flag. The
 *  browser never derives "today" or the 10-year horizon itself. */
export function getPlannedBoard(): Promise<PlannedBoardResponse> {
  return fetchJson<PlannedBoardResponse>('/api/board/planned');
}

import { fetchJson } from './client';
import type {
  ArchiveRulesResponse,
  DeletePauseResponse,
  NewPauseInput,
  NewRuleInput,
  PauseResponse,
  PausesResponse,
  ReorderRulesResponse,
  RuleScore,
  ScoreboardResponse,
  UpdateRuleInput,
} from '@/app/types';

// `/api/rules/*` and `/api/pauses/*` (reports slice 4). Every mutating rule
// endpoint answers with the same `RuleScore` card the scoreboard uses; the
// callers invalidate anyway, so the card is only a convenience.

export function getScoreboard(): Promise<ScoreboardResponse> {
  return fetchJson<ScoreboardResponse>('/api/rules/scoreboard');
}

export function listArchiveRules(): Promise<ArchiveRulesResponse> {
  return fetchJson<ArchiveRulesResponse>('/api/rules/archive');
}

export function createRule(input: NewRuleInput): Promise<RuleScore> {
  return fetchJson<RuleScore>('/api/rules', { method: 'POST', body: input });
}

export function updateRule(
  id: string,
  input: UpdateRuleInput,
): Promise<RuleScore> {
  return fetchJson<RuleScore>(`/api/rules/${id}`, {
    method: 'PATCH',
    body: input,
  });
}

/** No body → an open-ended pause (`status = paused`). */
export function pauseRule(id: string): Promise<RuleScore> {
  return fetchJson<RuleScore>(`/api/rules/${id}/pause`, {
    method: 'POST',
    body: {},
  });
}

export function resumeRule(id: string): Promise<RuleScore> {
  return fetchJson<RuleScore>(`/api/rules/${id}/resume`, { method: 'POST' });
}

export function archiveRule(id: string): Promise<RuleScore> {
  return fetchJson<RuleScore>(`/api/rules/${id}/archive`, { method: 'POST' });
}

export function restoreRule(id: string): Promise<RuleScore> {
  return fetchJson<RuleScore>(`/api/rules/${id}/restore`, { method: 'POST' });
}

/** The body must list every living rule id exactly once (400 otherwise). */
export function reorderRules(ids: string[]): Promise<ReorderRulesResponse> {
  return fetchJson<ReorderRulesResponse>('/api/rules/reorder', {
    method: 'POST',
    body: { ids },
  });
}

export function listPauses(): Promise<PausesResponse> {
  return fetchJson<PausesResponse>('/api/pauses');
}

export function createGlobalPause(
  input: NewPauseInput,
): Promise<PauseResponse> {
  return fetchJson<PauseResponse>('/api/pauses', {
    method: 'POST',
    body: input,
  });
}

export function deletePause(id: string): Promise<DeletePauseResponse> {
  return fetchJson<DeletePauseResponse>(`/api/pauses/${id}`, {
    method: 'DELETE',
  });
}

import { queryOptions, useMutation, useQuery } from '@tanstack/react-query';
import {
  archiveRule,
  createGlobalPause,
  createRule,
  deletePause,
  getScoreboard,
  listArchiveRules,
  listPauses,
  pauseRule,
  reorderRules,
  restoreRule,
  resumeRule,
  updateRule,
} from '@/lib/api';
import type { NewPauseInput, NewRuleInput, UpdateRuleInput } from '@/app/types';
import { queryKeys } from './keys';
import { queryClient } from '@/lib/queryClient';

// `queryOptions` keeps the factories React-free so the key/queryFn pair can be
// reused and tested without a component. Hooks below wrap them.
//
// Every rule mutation invalidates all three reads (`rules.all` is the prefix of
// scoreboard / archive / pauses): a pause, an archive, or a reorder changes the
// board, the archive list, and the pause list at once.

export function scoreboardQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.rules.scoreboard(),
    queryFn: getScoreboard,
  });
}

/** The board read. Polls every 4s while the digest is reclassifying or
 *  backfilling, so the strips fill in without a manual refresh. */
export function useScoreboardQuery() {
  return useQuery({
    ...scoreboardQueryOptions(),
    refetchInterval: (query) => {
      const board = query.state.data;
      return board && (board.reclassifying || board.backfilling) ? 4000 : false;
    },
  });
}

export function archiveQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.rules.archive(),
    queryFn: listArchiveRules,
  });
}

/** The archive read. Always on: reorder needs the archive ids to send a
 *  complete id list, and the Archive view renders straight from it. */
export function useArchiveQuery() {
  return useQuery(archiveQueryOptions());
}

export function pausesQueryOptions() {
  return queryOptions({
    queryKey: queryKeys.rules.pauses(),
    queryFn: listPauses,
  });
}

export function usePausesQuery(opts?: { enabled?: boolean }) {
  return useQuery({
    ...pausesQueryOptions(),
    enabled: opts?.enabled ?? true,
  });
}

function invalidateRules() {
  void queryClient.invalidateQueries({ queryKey: queryKeys.rules.all });
}

export function useCreateRule() {
  return useMutation({
    mutationFn: (input: NewRuleInput) => createRule(input),
    onSuccess: invalidateRules,
  });
}

export function useUpdateRule() {
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: UpdateRuleInput }) =>
      updateRule(id, input),
    onSuccess: invalidateRules,
  });
}

export function usePauseRule() {
  return useMutation({ mutationFn: pauseRule, onSuccess: invalidateRules });
}

export function useResumeRule() {
  return useMutation({ mutationFn: resumeRule, onSuccess: invalidateRules });
}

export function useArchiveRule() {
  return useMutation({ mutationFn: archiveRule, onSuccess: invalidateRules });
}

export function useRestoreRule() {
  return useMutation({ mutationFn: restoreRule, onSuccess: invalidateRules });
}

export function useReorderRules() {
  return useMutation({
    mutationFn: (ids: string[]) => reorderRules(ids),
    onSuccess: invalidateRules,
  });
}

export function useCreateGlobalPause() {
  return useMutation({
    mutationFn: (input: NewPauseInput) => createGlobalPause(input),
    onSuccess: invalidateRules,
  });
}

export function useDeletePause() {
  return useMutation({ mutationFn: deletePause, onSuccess: invalidateRules });
}

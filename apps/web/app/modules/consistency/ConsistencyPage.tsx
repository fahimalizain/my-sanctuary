// The Consistency board (reports slice 4): the live scoreboard from
// `/api/rules/scoreboard` — headline, rule cards with 14-day strips and
// expandable evidence, plus create / edit / pause / archive / reorder.
//
// The client never scores anything: every number, label, and strip cell comes
// from the API. The page owns the mutations and the errors; the cards, the
// dialogs, and the evidence charts are presentational.

import { useMemo, useState } from 'react';
import { Loader2, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  useArchiveQuery,
  useArchiveRule,
  useCreateGlobalPause,
  useCreateRule,
  useDeletePause,
  usePausesQuery,
  usePauseRule,
  useReorderRules,
  useRestoreRule,
  useResumeRule,
  useScoreboardQuery,
  useUpdateRule,
} from '@/app/queries/rules';
import type { RuleScore } from '@/app/types';
import { GlobalPauseDialog } from './GlobalPauseDialog';
import { ArchiveRuleRow, RuleCard } from './RuleCard';
import { RuleDialog } from './RuleDialog';
import type { RuleSubmitPayload } from './RuleDialog';
import { reorderIds, swapNeighbors } from './rules-model';

export function ConsistencyPage() {
  const boardQuery = useScoreboardQuery();
  const archiveQuery = useArchiveQuery();
  const board = boardQuery.data;
  const rules = board?.rules ?? [];
  const archiveRules = archiveQuery.data?.rules ?? [];

  const [view, setView] = useState<'board' | 'archive'>('board');
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [formRule, setFormRule] = useState<RuleScore | null>(null);
  const [pauseOpen, setPauseOpen] = useState(false);
  // Action failures (a 400, a reorder refused) render above the still-mounted
  // board — an action error never unmounts the cards.
  const [actionError, setActionError] = useState<string | null>(null);

  const createRule = useCreateRule();
  const updateRule = useUpdateRule();
  const pauseRule = usePauseRule();
  const resumeRule = useResumeRule();
  const archiveRule = useArchiveRule();
  const restoreRule = useRestoreRule();
  const reorderRules = useReorderRules();
  const createPause = useCreateGlobalPause();
  const deletePause = useDeletePause();
  // Read only while the pause dialog is open (same gating as the calendars
  // picker); the mutations invalidate it either way.
  const pausesQuery = usePausesQuery({ enabled: pauseOpen });

  const loadError =
    boardQuery.error instanceof Error
      ? boardQuery.error.message
      : boardQuery.error
        ? 'Failed to load the scoreboard'
        : null;
  const isLoading = boardQuery.isLoading;
  const busy =
    createRule.isPending ||
    updateRule.isPending ||
    pauseRule.isPending ||
    resumeRule.isPending ||
    archiveRule.isPending ||
    restoreRule.isPending ||
    reorderRules.isPending;
  // Reorder sends every living rule id, so it stays disabled until the
  // archive list is actually loaded — a partial list is a 400.
  const canReorder = archiveQuery.isSuccess && !reorderRules.isPending;

  const pausedCount = rules.filter((rule) => rule.phase === 'paused').length;
  const ruleNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const rule of board?.rules ?? []) names.set(rule.id, rule.name);
    for (const rule of archiveQuery.data?.rules ?? []) {
      names.set(rule.id, rule.name);
    }
    return names;
  }, [board, archiveQuery.data]);

  const runAction = async (
    fallback: string,
    action: () => Promise<unknown>,
  ) => {
    setActionError(null);
    try {
      await action();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : fallback);
    }
  };

  const openCreate = () => {
    setFormRule(null);
    setFormOpen(true);
  };

  const openEdit = (rule: RuleScore) => {
    setFormRule(rule);
    setFormOpen(true);
  };

  const closeForm = () => {
    setFormOpen(false);
    setFormRule(null);
  };

  const handleSubmit = async (payload: RuleSubmitPayload) => {
    const base = {
      name: payload.name,
      icon: payload.icon,
      params: payload.params,
    };
    if (formRule === null) {
      await createRule.mutateAsync({
        ...base,
        kind: payload.kind,
        ...(payload.endsOn === '' ? {} : { ends_on: payload.endsOn }),
      });
    } else {
      await updateRule.mutateAsync({
        id: formRule.id,
        input: {
          ...base,
          // `null` (key present) clears the challenge end.
          ends_on: payload.endsOn === '' ? null : payload.endsOn,
        },
      });
    }
    closeForm();
  };

  const handleMove = (index: number, direction: -1 | 1) => {
    const archive = archiveQuery.data;
    if (archive === undefined) {
      setActionError('Archive list unavailable — reorder is disabled.');
      return;
    }
    const swapped = swapNeighbors(
      rules.map((rule) => rule.id),
      index,
      direction,
    );
    if (swapped === null) return;
    void runAction('Reorder failed', () =>
      reorderRules.mutateAsync(reorderIds(swapped, archive.rules)),
    );
  };

  const handleArchive = (rule: RuleScore) => {
    if (!window.confirm(`Archive "${rule.name}"?`)) return;
    void runAction('Archive failed', () => archiveRule.mutateAsync(rule.id));
  };

  const handleRestore = (rule: RuleScore) => {
    void runAction('Restore failed', () => restoreRule.mutateAsync(rule.id));
  };

  const handleReopen = (rule: RuleScore) => {
    void runAction('Reopen failed', () =>
      updateRule.mutateAsync({ id: rule.id, input: { ends_on: null } }),
    );
  };

  const toggleExpanded = (id: string) =>
    setExpandedId((current) => (current === id ? null : id));

  return (
    <div className="min-h-screen bg-cream">
      <div className="mx-auto max-w-7xl px-6 pt-8 pb-28">
        <header className="mb-8 flex flex-wrap items-center justify-between gap-4">
          <div>
            <h1 className="mb-2 font-heading text-3xl font-bold text-foreground">
              Consistency
            </h1>
            <p className="text-muted-foreground">
              What you owe yourself, from the diary.
            </p>
          </div>
          <div className="flex flex-wrap items-center gap-3">
            <Button
              variant="outline"
              onClick={() => setPauseOpen(true)}
              className="border-input text-foreground hover:bg-muted"
            >
              Pause everything
            </Button>
            <button
              type="button"
              onClick={() =>
                setView((current) =>
                  current === 'board' ? 'archive' : 'board',
                )
              }
              className="text-sm text-muted-foreground underline-offset-4 transition-colors hover:text-foreground hover:underline"
            >
              {view === 'board' ? 'Archive' : 'Board'}
            </button>
            <Button onClick={openCreate} className="flex-shrink-0">
              <Plus className="mr-1 h-4 w-4" />
              New rule
            </Button>
          </div>
        </header>

        {/* Load failure — the banner replaces nothing; the board below stays
            mounted when it has rows */}
        {loadError && (
          <div className="mb-6 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
            <p className="selectable text-sm">{loadError}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void boardQuery.refetch();
              }}
            >
              Retry
            </Button>
          </div>
        )}

        {/* Action failure */}
        {actionError && (
          <div className="mb-6 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
            <p className="selectable text-sm">{actionError}</p>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setActionError(null)}
            >
              Dismiss
            </Button>
          </div>
        )}

        {/* Digest job flags — the query polls every 4s while either is true */}
        {(board?.reclassifying || board?.backfilling) && (
          <div className="mb-6 flex flex-wrap items-center gap-3 rounded-xl border border-border bg-card px-4 py-3 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {board?.reclassifying && <span>Reclassifying your events…</span>}
            {board?.backfilling && <span>Building history…</span>}
          </div>
        )}

        {/* Headline */}
        {board && (
          <section className="mb-6">
            {board.active_rules === 0 && pausedCount === 0 ? (
              <p className="font-heading text-3xl font-bold text-foreground">
                No rules yet
              </p>
            ) : (
              <>
                <p className="font-heading text-4xl font-bold text-foreground">
                  <span className="text-primary">{board.on_track}</span> /{' '}
                  {board.active_rules} on track
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  all-pass streak {board.all_pass_streak} · best{' '}
                  {board.best_all_pass_streak}
                </p>
              </>
            )}
          </section>
        )}

        {isLoading && rules.length === 0 && (
          <div className="flex items-center justify-center gap-2 py-24 text-muted-foreground">
            <Loader2 className="h-5 w-5 animate-spin" />
            Loading the scoreboard…
          </div>
        )}

        {view === 'board' ? (
          <>
            {!isLoading && !loadError && rules.length === 0 && (
              <div className="rounded-xl border border-dashed border-border bg-card/50 p-10 text-center">
                <p className="mb-1 font-heading text-lg font-semibold text-foreground">
                  No rules yet
                </p>
                <p className="text-sm text-muted-foreground">
                  A rule scores itself from your diary every day — start with
                  one you already keep.
                </p>
              </div>
            )}

            {rules.length > 0 && (
              <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
                {rules.map((rule, index) => (
                  <RuleCard
                    key={rule.id}
                    rule={rule}
                    index={index}
                    total={rules.length}
                    expanded={expandedId === rule.id}
                    onToggle={() => toggleExpanded(rule.id)}
                    reorderEnabled={canReorder}
                    pending={busy}
                    onMove={(direction) => handleMove(index, direction)}
                    onPause={() =>
                      void runAction('Pause failed', () =>
                        pauseRule.mutateAsync(rule.id),
                      )
                    }
                    onResume={() =>
                      void runAction('Resume failed', () =>
                        resumeRule.mutateAsync(rule.id),
                      )
                    }
                    onEdit={() => openEdit(rule)}
                    onArchive={() => handleArchive(rule)}
                  />
                ))}
              </div>
            )}
          </>
        ) : (
          <>
            {archiveQuery.isLoading && archiveRules.length === 0 && (
              <div className="flex items-center justify-center gap-2 py-24 text-muted-foreground">
                <Loader2 className="h-5 w-5 animate-spin" />
                Loading the archive…
              </div>
            )}

            {archiveQuery.error instanceof Error &&
              archiveRules.length === 0 && (
                <div className="mb-6 flex items-center justify-between gap-4 rounded-xl bg-destructive/10 px-4 py-3 text-destructive">
                  <p className="selectable text-sm">
                    {archiveQuery.error.message}
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      void archiveQuery.refetch();
                    }}
                  >
                    Retry
                  </Button>
                </div>
              )}

            {!archiveQuery.isLoading &&
              !archiveQuery.error &&
              archiveRules.length === 0 && (
                <div className="rounded-xl border border-dashed border-border bg-card/50 p-10 text-center">
                  <p className="mb-1 font-heading text-lg font-semibold text-foreground">
                    Nothing archived
                  </p>
                  <p className="text-sm text-muted-foreground">
                    Finished challenges and archived rules land here.
                  </p>
                </div>
              )}

            {archiveRules.length > 0 && (
              <div className="space-y-4">
                {archiveRules.map((rule) => (
                  <ArchiveRuleRow
                    key={rule.id}
                    rule={rule}
                    expanded={expandedId === rule.id}
                    onToggle={() => toggleExpanded(rule.id)}
                    pending={busy}
                    onRestore={() => handleRestore(rule)}
                    onReopen={() => handleReopen(rule)}
                  />
                ))}
              </div>
            )}
          </>
        )}
      </div>

      <RuleDialog
        open={formOpen}
        rule={formRule}
        pending={createRule.isPending || updateRule.isPending}
        onClose={closeForm}
        onSubmit={handleSubmit}
      />

      <GlobalPauseDialog
        open={pauseOpen}
        onClose={() => setPauseOpen(false)}
        today={board?.as_of ?? ''}
        pauses={pausesQuery.data?.pauses ?? []}
        pausesLoading={pausesQuery.isLoading}
        ruleNames={ruleNames}
        pending={createPause.isPending || deletePause.isPending}
        onCreate={async (input) => {
          await createPause.mutateAsync(input);
        }}
        onDelete={async (id) => {
          await deletePause.mutateAsync(id);
        }}
      />
    </div>
  );
}

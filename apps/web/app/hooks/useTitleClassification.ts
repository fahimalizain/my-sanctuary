import { useEffect, useRef, useState } from 'react';
import { API_BASE_URL } from '@/lib/api';
import { buildClassifyUrl } from './classify-url';
import type { ClassifyResponse, TaskCategorySummary } from '@/app/types';

// The classify preview status + the chrome it computed. `matched` is a
// guaranteed filing target; `nomatch`/`conflict` still carry the chrome the
// server reported (a locked category outside the identity case previews its
// affixes even when the fill does not file). `persistTitle` is what a save
// would store and `displayTitle` the hole the server split off.
export type ClassifyStatus =
  | { state: 'idle' } // neutral (no result yet / cleared / degraded)
  | { state: 'loading' } // request in flight
  | {
      state: 'matched';
      category: TaskCategorySummary;
      prefix: string;
      suffix: string;
      persistTitle: string;
      displayTitle: string;
    } // "Files to ● X" / "Will refile to ● X"
  | {
      state: 'nomatch';
      prefix: string;
      suffix: string;
      persistTitle: string;
      displayTitle: string;
    } // "No category matches — Save will fail"
  | {
      state: 'conflict';
      categories: TaskCategorySummary[];
      prefix: string;
      suffix: string;
      persistTitle: string;
      displayTitle: string;
    }; // "Matches A and B — be more specific"

export interface UseTitleClassificationArgs {
  /** Live title value — the visible hole. Used to drop responses that landed
   *  after the user moved past the classified snapshot. */
  title: string;
  /** The title snapshot to classify. The modal sets it on every blur (the
   *  hole), on edit-open (the stored FULL `task.title`, so the first classify
   *  is identity — classifying the hole would fill and canonicalize the
   *  affixes), and on lock change (the current hole). `null` until there is
   *  something to classify. */
  classifyTitle: string | null;
  /** Category lock; `null` = title-only classify. Empty titles only fire when
   *  locked (the server 400s an empty unlocked classify). */
  categoryId: string | null;
  /** Edit mode: the task's stored full title. Identifies the edit-open seed
   *  so its response is never dropped as stale — the live title is the hole,
   *  which legitimately differs from the full stored string. Omit in create. */
  initialTitle?: string;
  /** `false` when the modal is closed. */
  active: boolean;
  /** Changes on (modal open/close) and (task.id change in edit mode) — forces
   *  a full reset (abort in-flight, clear cache, status→idle). The modal
   *  passes `${open}:${task?.id ?? 'new'}`. */
  resetKey: string;
}

export function useTitleClassification({
  title,
  classifyTitle,
  categoryId,
  initialTitle,
  active,
  resetKey,
}: UseTitleClassificationArgs): ClassifyStatus {
  const [status, setStatus] = useState<ClassifyStatus>({ state: 'idle' });
  const abortRef = useRef<AbortController | null>(null);
  // Suppression cache keyed by `lock:title`: only MATCHED outcomes suppress a
  // re-fire of the exact same request. Negative (nomatch/conflict) results
  // re-fire on every trigger so a taxonomy change elsewhere can unblock the
  // user without forcing them to edit the title text.
  const lastMatchedKey = useRef<string | null>(null);
  // Latest live title, read at request resolution so a response for a
  // classified snapshot never repaints a stale result after the user kept
  // typing (the seed is exempt — see the fire effect).
  const titleRef = useRef(title);
  // Guards setState after unmount (React 18/19 dev warning). TaskModal stays
  // mounted with `open=false`, but callers could unmount it while a classify
  // is in flight — the catch below must not set state on a dead component.
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    titleRef.current = title;
  }, [title]);

  // Reset on close or task switch: abort + idle + clear cache.
  useEffect(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    setStatus({ state: 'idle' });
    lastMatchedKey.current = null;
  }, [resetKey]);

  // A lock CHANGE must always re-classify — even reverting to a title that
  // previously matched unlocked — because the lock changes both the outcome
  // and the displayed chrome. Without this, clearing a lock could hit the
  // match cache for a re-issue of the same unlocked request and leave the
  // PREVIOUS lock's chrome (e.g. " | Work") on screen.
  const prevCategoryRef = useRef(categoryId);
  useEffect(() => {
    if (prevCategoryRef.current !== categoryId) {
      lastMatchedKey.current = null;
      prevCategoryRef.current = categoryId;
    }
  }, [categoryId]);

  // Fire on (classifyTitle | categoryId) change: a title blur sends the hole,
  // edit-open sends the stored full title once, and a picker change sends the
  // current hole with the new lock (empty titles are fine when locked).
  useEffect(() => {
    if (!active || classifyTitle === null) return;
    const trimmed = classifyTitle.trim();
    if (trimmed === '' && categoryId === null) return; // empty + no lock → idle
    const key = `${categoryId ?? ''}:${trimmed}`;
    if (lastMatchedKey.current === key) return; // exact request already matched

    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setStatus({ state: 'loading' });

    // Edit-open seed: classify the hole's FULL stored title (identity) so the
    // response is never dropped — the live title (the hole) differs by design.
    const isSeed =
      initialTitle !== undefined && trimmed === initialTitle.trim();
    const url = `${API_BASE_URL}${buildClassifyUrl(trimmed, categoryId)}`;

    (async () => {
      try {
        const res = await fetch(url, {
          credentials: 'include',
          signal: controller.signal,
        });
        if (!mountedRef.current) return;
        if (!res.ok) {
          setStatus({ state: 'idle' });
          return; // silent degrade (the modal never fires the 400 cases)
        }
        const data = (await res.json()) as ClassifyResponse;
        if (!mountedRef.current) return;
        // The snap shape: the modal collapsed the input to the hole
        // (`display_title`) while this request was in flight for the full
        // string (`persist_title`) — the live title legitimately differs from
        // the classified snapshot, as with the edit-open seed, so the
        // response must not be dropped as stale.
        const result = 'Matched' in data ? data.Matched : data.Untracked;
        const liveIsSnappedHole =
          titleRef.current.trim() === result.display_title.trim() &&
          trimmed === result.persist_title.trim();
        // The user typed past the snapshot we classified — drop the stale
        // result instead of repainting it (exempt the seed and the snapped
        // hole, where the live input differs from the classified string by
        // design).
        if (
          !isSeed &&
          !liveIsSnappedHole &&
          titleRef.current.trim() !== trimmed
        ) {
          setStatus({ state: 'idle' });
          return;
        }
        if (!mountedRef.current) return;
        if ('Matched' in data) {
          lastMatchedKey.current = key;
          setStatus({
            state: 'matched',
            category: data.Matched.category,
            prefix: data.Matched.prefix,
            suffix: data.Matched.suffix,
            persistTitle: data.Matched.persist_title,
            displayTitle: data.Matched.display_title,
          });
        } else if (data.Untracked.conflict) {
          setStatus({
            state: 'conflict',
            categories: data.Untracked.categories,
            prefix: data.Untracked.prefix,
            suffix: data.Untracked.suffix,
            persistTitle: data.Untracked.persist_title,
            displayTitle: data.Untracked.display_title,
          });
        } else {
          setStatus({
            state: 'nomatch',
            prefix: data.Untracked.prefix,
            suffix: data.Untracked.suffix,
            persistTitle: data.Untracked.persist_title,
            displayTitle: data.Untracked.display_title,
          });
        }
      } catch {
        // AbortError and network failures both land here → silent degrade.
        if (mountedRef.current) setStatus({ state: 'idle' });
      }
    })();
    // NOTE: do NOT abort in the cleanup of this effect — the [resetKey]
    // effect is the single owner of abort-on-reset. Aborting here would
    // cancel the in-flight request on every re-render of the fire effect.
  }, [classifyTitle, categoryId, active, initialTitle]);

  return status;
}

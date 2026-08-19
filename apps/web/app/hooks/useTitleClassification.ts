import { useEffect, useRef, useState } from 'react';
import { API_BASE_URL } from '@/lib/api';
import type { ClassifyResponse, TaskCategorySummary } from '@/app/types';

export type ClassifyStatus =
  | { state: 'idle' } // neutral (no result yet / cleared / degraded)
  | { state: 'loading' } // request in flight
  | { state: 'matched'; category: TaskCategorySummary } // "Files to ● X" / "Will refile to ● X"
  | { state: 'nomatch' } // "No category matches — Save will fail"
  | { state: 'conflict'; categories: TaskCategorySummary[] }; // "Matches A and B — be more specific"

export interface UseTitleClassificationArgs {
  /** Live title value — a divergence from `blurredTitle` clears a stale result. */
  title: string;
  /** The title snapshot at the moment the input blurred (modal sets this in
   *  onBlur). `null` until the first blur. */
  blurredTitle: string | null;
  /** Edit mode: the task's stored title. The hook never fires when
   *  `blurredTitle` equals this (no point re-classifying the unchanged title
   *  the server already filed). Omit in create mode. */
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
  blurredTitle,
  initialTitle,
  active,
  resetKey,
}: UseTitleClassificationArgs): ClassifyStatus {
  const [status, setStatus] = useState<ClassifyStatus>({ state: 'idle' });
  const abortRef = useRef<AbortController | null>(null);
  // Suppression cache: only MATCHED outcomes suppress a re-fire of the same
  // title. Negative (nomatch/conflict) results re-fire on every re-blur so a
  // taxonomy change elsewhere can unblock the user without forcing them to
  // edit the title text.
  const lastMatchedTitle = useRef<string | null>(null);
  // Latest live title, read at request resolution so a response for a blurred
  // snapshot never resurrects a status after the user kept typing (the
  // clear-on-change effect only fires on title *changes* — a late response
  // landing after it would otherwise repaint a stale result).
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
    lastMatchedTitle.current = null;
  }, [resetKey]);

  // Clear-on-change: the moment the live title diverges from the blurred
  // snapshot, the result is stale → back to idle (re-renders the neutral hint).
  useEffect(() => {
    if (!active || blurredTitle === null) return;
    if (title.trim() !== blurredTitle.trim()) {
      setStatus({ state: 'idle' });
    }
  }, [title, blurredTitle, active]);

  // Fire on blur.
  useEffect(() => {
    if (!active || blurredTitle === null) return;
    const trimmed = blurredTitle.trim();
    if (trimmed === '') return; // blank → neutral
    if (initialTitle !== undefined && trimmed === initialTitle.trim()) return; // edit unchanged
    if (lastMatchedTitle.current === trimmed) return; // unchanged+matched → cache hit

    abortRef.current?.abort();
    const controller = new AbortController();
    abortRef.current = controller;
    setStatus({ state: 'loading' });

    (async () => {
      try {
        const res = await fetch(
          `${API_BASE_URL}/api/tasks/classify?title=${encodeURIComponent(trimmed)}`,
          { credentials: 'include', signal: controller.signal },
        );
        if (!mountedRef.current) return;
        if (!res.ok) {
          setStatus({ state: 'idle' });
          return; // silent degrade
        }
        const data = (await res.json()) as ClassifyResponse;
        // The user kept typing after this blur — the live title no longer
        // matches the snapshot we classified, so drop the result.
        if (titleRef.current.trim() !== trimmed) return;
        if (!mountedRef.current) return;
        if ('Matched' in data) {
          lastMatchedTitle.current = trimmed;
          setStatus({ state: 'matched', category: data.Matched.category });
        } else if (data.Untracked.conflict) {
          setStatus({
            state: 'conflict',
            categories: data.Untracked.categories,
          });
        } else {
          setStatus({ state: 'nomatch' });
        }
      } catch {
        // AbortError and network failures both land here → silent degrade.
        if (mountedRef.current) setStatus({ state: 'idle' });
      }
    })();
    // NOTE: do NOT abort in the cleanup of this effect — the [resetKey]
    // effect is the single owner of abort-on-reset. Aborting here would
    // cancel the in-flight request on every re-render of the blur effect.
  }, [blurredTitle, active, initialTitle]);

  return status;
}

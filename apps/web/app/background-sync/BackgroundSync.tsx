import { useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useAuth } from '@/lib/auth';
import { getBoardTasksBusy, subscribeBoardTasksBusy } from './board-tasks-busy';
import { startBackgroundSync } from './start-background-sync';

/** Headless observer host: keeps the session's shared queries warm above the
 *  router, independent of the active route. Renders nothing and never
 *  re-renders on query traffic — only auth changes re-run the effect. */
export function BackgroundSync(): null {
  const { user, isLoading } = useAuth();
  const queryClient = useQueryClient();

  useEffect(() => {
    if (isLoading || !user) return;

    const session = startBackgroundSync(queryClient);
    // Apply the current flag at start, then follow only real changes. The
    // flag must not enter the deps: a drag would otherwise destroy and
    // recreate every observer (cancelling in-flight fetches) on each toggle.
    session.setTasksBusy(getBoardTasksBusy());
    const unsubscribe = subscribeBoardTasksBusy(() => {
      session.setTasksBusy(getBoardTasksBusy());
    });

    return () => {
      unsubscribe();
      session.stop();
    };
  }, [user, isLoading, queryClient]);

  return null;
}

import { useState } from 'react';
import { Loader2, Trash2 } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { useApiKeysQuery, useRevokeApiKey } from '@/app/queries/api-keys';
import type { ApiKey } from '@/app/types';
import { SettingsSection } from './SettingsSection';
import { expiryLabel, isExpired, lastUsedLabel } from './api-key-format';

/**
 * Settings → API keys: the list of live keys and their revoke action.
 *
 * Keys are only ever read here — the one-time secret reveal lives in the
 * create dialog, and the server never returns a secret again.
 */
export function ApiKeysSection() {
  const apiKeysQuery = useApiKeysQuery();
  const revoke = useRevokeApiKey();

  const apiKeys = apiKeysQuery.data?.keys ?? [];
  const isLoading = apiKeysQuery.isLoading;
  const loadError =
    apiKeysQuery.error instanceof Error
      ? apiKeysQuery.error.message
      : apiKeysQuery.error
        ? 'Failed to load API keys'
        : null;
  const [actionError, setActionError] = useState<string | null>(null);

  const handleRevoke = (apiKey: ApiKey) => {
    const message = `Revoke "${apiKey.name}"? Scripts using it will stop working immediately.`;
    if (!window.confirm(message)) return;

    setActionError(null);
    // Revoke is immediate and not restorable — no undo affordance.
    revoke.mutate(apiKey.id, {
      onError: (err) => {
        setActionError(err instanceof Error ? err.message : 'Revoke failed');
      },
    });
  };

  const now = Date.now();

  return (
    <SettingsSection title="API keys">
      <p className="mb-3 text-sm text-muted-foreground">
        Use a key to call the Sanctuary API from scripts, Shortcuts, or other
        clients.
      </p>

      {/* Load error — rows stay mounted underneath when a refresh fails */}
      {loadError && (
        <div className="mb-3 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-lg px-4 py-3">
          <p className="selectable text-sm">{loadError}</p>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void apiKeysQuery.refetch();
            }}
          >
            Retry
          </Button>
        </div>
      )}

      {/* Action error (a failed revoke) — the list stays mounted */}
      {actionError && (
        <div className="mb-3 flex items-center justify-between gap-4 bg-destructive/10 text-destructive rounded-lg px-4 py-3">
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

      {/* First load (or a retry after a hard error) only */}
      {isLoading && apiKeys.length === 0 && (
        <div className="flex items-center justify-center gap-2 py-8 text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading keys…
        </div>
      )}

      {!isLoading && !loadError && apiKeys.length === 0 && (
        <div className="rounded-lg border border-dashed border-border bg-card/50 p-6 text-center">
          <p className="text-sm text-muted-foreground">No API keys yet.</p>
        </div>
      )}

      {apiKeys.length > 0 && (
        <div className="space-y-2">
          {apiKeys.map((apiKey) => {
            const rowPending =
              revoke.isPending && revoke.variables === apiKey.id;
            const expired = isExpired(apiKey.expires_at, now);

            return (
              <div
                key={apiKey.id}
                className="rounded-lg border border-border bg-background p-3"
              >
                <div className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
                    {apiKey.name}
                  </span>
                  {expired && <Badge variant="destructive">Expired</Badge>}
                  <button
                    type="button"
                    onClick={() => handleRevoke(apiKey)}
                    disabled={rowPending}
                    className="p-1.5 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors disabled:opacity-50"
                    aria-label={`Revoke ${apiKey.name}`}
                    title="Revoke"
                  >
                    {rowPending ? (
                      <Loader2 className="h-3.5 w-3.5 animate-spin" />
                    ) : (
                      <Trash2 className="h-3.5 w-3.5 text-muted-foreground" />
                    )}
                  </button>
                </div>
                <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                  <code className="selectable font-mono text-xs">
                    {apiKey.key_prefix}…
                  </code>
                  <span>{expiryLabel(apiKey, now)}</span>
                  <span>{lastUsedLabel(apiKey.last_used_at)}</span>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </SettingsSection>
  );
}

import { RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { API_BASE_URL } from '@/lib/api';
import type { SyncHealthBanner } from '../lib/sync-health';

export interface CalendarSyncBannerProps {
  banner: SyncHealthBanner;
  onRetry: () => void;
  isRefreshing: boolean;
}

export function CalendarSyncBanner({
  banner,
  onRetry,
  isRefreshing,
}: CalendarSyncBannerProps) {
  if (banner.kind === 'none') return null;

  const retryLabel = banner.kind === 'fetch_error' ? 'Retry' : 'Try again';

  return (
    <div className="shrink-0 flex items-center justify-between gap-3 border-b border-border bg-muted/50 px-4 py-2 text-sm">
      <p className="text-muted-foreground truncate">{banner.message}</p>
      <div className="flex items-center gap-2 shrink-0">
        {banner.showRetry && (
          <Button
            variant="outline"
            size="sm"
            onClick={onRetry}
            disabled={isRefreshing}
          >
            <RefreshCw className="h-3.5 w-3.5 mr-1.5" />
            {retryLabel}
          </Button>
        )}
        {banner.showReconnect && (
          <a
            href={`${API_BASE_URL}/auth/google`}
            className="inline-flex items-center justify-center h-8 px-3 rounded-md border border-input bg-background text-foreground hover:bg-muted text-sm font-medium transition-colors"
          >
            Reconnect Google
          </a>
        )}
      </div>
    </div>
  );
}

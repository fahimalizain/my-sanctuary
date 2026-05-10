import { useRegisterSW } from 'virtual:pwa-register/react';
import { useEffect } from 'react';

export function ReloadPrompt() {
  const {
    offlineReady: [offlineReady, setOfflineReady],
    needRefresh: [needRefresh, setNeedRefresh],
    updateServiceWorker,
  } = useRegisterSW({
    onRegistered(r: ServiceWorkerRegistration | undefined) {
      // eslint-disable-next-line no-console
      console.log('SW Registered: ' + r);
    },
    onRegisterError(error: Error) {
      // eslint-disable-next-line no-console
      console.log('SW registration error', error);
    },
  });

  const close = () => {
    setOfflineReady(false);
    setNeedRefresh(false);
  };

  useEffect(() => {
    if (offlineReady) {
      const timer = setTimeout(close, 3000);
      return () => clearTimeout(timer);
    }
  }, [offlineReady]);

  if (!offlineReady && !needRefresh) return null;

  return (
    <div className="fixed top-4 right-4 z-[100] max-w-sm rounded-xl border border-border bg-card p-4 shadow-lg">
      <div className="flex items-start gap-3">
        <div className="flex-1">
          {offlineReady && (
            <p className="text-sm text-foreground">
              App ready to work offline
            </p>
          )}
          {needRefresh && (
            <p className="text-sm text-foreground">
              New content available, click reload to update.
            </p>
          )}
        </div>
        <div className="flex gap-2">
          {needRefresh && (
            <button
              onClick={() => updateServiceWorker(true)}
              className="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:opacity-90"
            >
              Reload
            </button>
          )}
          <button
            onClick={close}
            className="rounded-lg bg-muted px-3 py-1.5 text-xs font-medium text-muted-foreground hover:bg-muted/80"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}

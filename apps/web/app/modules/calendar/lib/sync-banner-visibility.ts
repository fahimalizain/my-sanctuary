import type { SyncHealthBanner } from './sync-health';

/** Which controls the sync banner renders for one user. */
export interface SyncBannerControls {
  /** Show the Retry / Try again button. */
  retry: boolean;
  /** Show the "Reconnect Google" link. */
  reconnect: boolean;
}

/**
 * The banner's controls.
 *
 * A standalone user has no Google grant to reconnect, so the link is hidden —
 * but the banner itself (and its Retry) still shows: a real `fetch_error` is
 * a local read failure the user can retry, not a Google problem (ADR 0012).
 */
export function syncBannerControls(
  banner: SyncHealthBanner,
  standalone: boolean,
): SyncBannerControls {
  return {
    retry: banner.showRetry,
    reconnect: banner.showReconnect && !standalone,
  };
}

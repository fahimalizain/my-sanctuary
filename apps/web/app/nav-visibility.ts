import { useSyncExternalStore } from 'react';

// Tiny app-level store: pages that need the floating nav out of the way call
// setNavHidden(true) and must reset it to false on unmount. A module store
// (not context) because the page and the root layout's <Navigation /> are
// siblings and only the boolean flip should re-render the nav.
let hidden = false;
const listeners = new Set<() => void>();

export function setNavHidden(next: boolean): void {
  if (hidden === next) return;
  hidden = next;
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useNavHidden(): boolean {
  return useSyncExternalStore(
    subscribe,
    () => hidden,
    () => false,
  );
}

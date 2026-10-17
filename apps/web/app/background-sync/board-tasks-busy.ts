// Module-level board busy flag — not React context and not query data. The
// background tasks observer must pause its poll while a drag / move / focus is
// in flight, and Board publishes that here instead of lifting the whole
// board's state above the router. Listeners fire only on a real change, so a
// repeated set never wakes background sync.

let busy = false;
const listeners = new Set<() => void>();

export function getBoardTasksBusy(): boolean {
  return busy;
}

export function setBoardTasksBusy(next: boolean): void {
  if (next === busy) return;
  busy = next;
  for (const listener of listeners) listener();
}

export function subscribeBoardTasksBusy(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

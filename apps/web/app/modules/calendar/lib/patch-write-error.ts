// One listener slot for failed calendar writes. The patch queue lives above
// the router and outlives the calendar page, so a late failure needs a way to
// reach the mounted page's banner — and unmount clears the slot so nothing
// setStates on a dead page. The reconcile of overlay/cache is separate and
// still runs.

type PatchWriteErrorListener = (base: string, err: unknown) => void;

let listener: PatchWriteErrorListener | null = null;

/** Register the single banner listener, or clear it with `null`. */
export function setPatchWriteErrorListener(
  fn: PatchWriteErrorListener | null,
): void {
  listener = fn;
}

/** Report a failed PATCH to the listener, if one is registered. */
export function reportPatchWriteError(base: string, err: unknown): void {
  listener?.(base, err);
}

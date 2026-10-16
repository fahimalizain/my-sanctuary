// Best-effort tactile tick for touch drags. Always fails silently.

const MIN_GAP_MS = 35;
const VIBRATE_MS = 10;

let lastTickAt = -Infinity;
let switchLabel: HTMLLabelElement | null = null;

/**
 * iOS Safari has no navigator.vibrate, but toggling an
 * `<input type="checkbox" switch>` fires the system haptic on iOS 18+.
 * Best-effort only: it works just inside user-gesture handling (pointer
 * events qualify) and WebKit may change or ignore it. Not feature-gated:
 * a hidden switch is harmless where it does nothing. The label stays in the
 * DOM (not display:none) so the click is not swallowed.
 */
function iosSwitchTick(): void {
  if (!switchLabel) {
    const label = document.createElement('label');
    label.setAttribute('aria-hidden', 'true');
    label.style.cssText =
      'position:fixed;left:-9999px;top:0;width:1px;height:1px;opacity:0;pointer-events:none;';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.setAttribute('switch', '');
    input.setAttribute('aria-hidden', 'true');
    input.tabIndex = -1;
    label.appendChild(input);
    document.body.appendChild(label);
    switchLabel = label;
  }
  switchLabel.click();
}

export function tick(): void {
  if (typeof navigator === 'undefined' || typeof document === 'undefined') {
    return;
  }
  const now = performance.now();
  if (now - lastTickAt < MIN_GAP_MS) return;
  lastTickAt = now;
  try {
    if (typeof navigator.vibrate === 'function') {
      navigator.vibrate(VIBRATE_MS);
    } else {
      iosSwitchTick();
    }
  } catch {
    // Haptics are cosmetic.
  }
}

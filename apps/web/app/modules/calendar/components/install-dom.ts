// Install a JSDOM environment before any React component that touches
// window/document on first render is imported.
// Call installDom() at the top of EventInspector.test.ts, then dynamic-import
// the component (static ESM imports hoist and can race the DOM setup).

import { JSDOM } from 'jsdom';

let installed = false;

export function installDom(): void {
  if (installed) return;
  installed = true;

  const dom = new JSDOM('<!DOCTYPE html><html><body></body></html>', {
    url: 'http://localhost/',
    pretendToBeVisual: true,
  });

  const { window } = dom;

  // Assign every browser global React / Radix / Testing Library need.
  // Prefer defineProperty so we can overwrite Node's native Event/CustomEvent
  // (Node 18+) — jsdom's EventTarget rejects non-jsdom Event instances.
  const assign = (key: string, value: unknown) => {
    Object.defineProperty(globalThis, key, {
      configurable: true,
      enumerable: true,
      writable: true,
      value,
    });
  };

  assign('window', window);
  assign('self', window);
  assign('document', window.document);
  assign('HTMLElement', window.HTMLElement);
  assign('Element', window.Element);
  assign('Node', window.Node);
  assign('DocumentFragment', window.DocumentFragment);
  assign('Document', window.Document);
  assign('Text', window.Text);
  assign('MutationObserver', window.MutationObserver);
  assign('Event', window.Event);
  assign('CustomEvent', window.CustomEvent);
  assign('MouseEvent', window.MouseEvent);
  assign('KeyboardEvent', window.KeyboardEvent);
  assign('FocusEvent', window.FocusEvent);
  assign(
    'PointerEvent',
    (window as unknown as { PointerEvent?: typeof window.MouseEvent })
      .PointerEvent ?? window.MouseEvent,
  );
  assign('DOMRect', window.DOMRect);
  assign('getComputedStyle', window.getComputedStyle.bind(window));
  assign(
    'requestAnimationFrame',
    window.requestAnimationFrame?.bind(window) ??
      ((cb: FrameRequestCallback) =>
        setTimeout(() => cb(Date.now()), 16) as unknown as number),
  );
  assign(
    'cancelAnimationFrame',
    window.cancelAnimationFrame?.bind(window) ??
      ((id: number) => clearTimeout(id)),
  );

  // navigator is often non-configurable on Node — best-effort.
  try {
    assign('navigator', window.navigator);
  } catch {
    /* ignore */
  }

  assign(
    'ResizeObserver',
    class ResizeObserver {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );

  // Radix focus/dismiss layers + pointer capture (not fully in jsdom).
  const htmlProto = window.HTMLElement.prototype as HTMLElement & {
    hasPointerCapture?: (id: number) => boolean;
    setPointerCapture?: (id: number) => void;
    releasePointerCapture?: (id: number) => void;
    scrollIntoView?: () => void;
  };
  htmlProto.hasPointerCapture = () => false;
  htmlProto.setPointerCapture = () => {};
  htmlProto.releasePointerCapture = () => {};
  htmlProto.scrollIntoView = () => {};

  Object.defineProperty(window, 'IS_REACT_ACT_ENVIRONMENT', {
    configurable: true,
    writable: true,
    value: true,
  });

  // Default: mobile (no 768 match). Tests can override per-case.
  window.matchMedia = (query: string): MediaQueryList => ({
    matches: false,
    media: query,
    onchange: null,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
    dispatchEvent() {
      return false;
    },
  });
}

/** Override matchMedia for a single test (desktop vs mobile). */
export function setMatchMediaDesktop(isDesktop: boolean): void {
  window.matchMedia = (query: string): MediaQueryList => {
    const matches = isDesktop ? query.includes('768') : false;
    return {
      matches,
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {
        return false;
      },
    };
  };
}

/** The focus pin's visibility classes (task-focus, slice 4 — the locked UX):
 *  - focused: always visible;
 *  - unfocused: hidden until the card is hovered (fine pointers), and
 *    ALWAYS visible on coarse / no-hover pointers, where a hover affordance
 *    is not discoverable (the user would never find the hidden pin).
 *  `group-hover` relies on `group` being present on the card root (TaskCard). */
export function focusPinVisibility(focused: boolean): string {
  return focused
    ? 'opacity-100'
    : 'opacity-0 group-hover:opacity-100 [@media(hover:none)]:opacity-100';
}

// Pure calendar keyboard keymap. No React and no KeyboardEvent: the page-level
// listener passes the key fields in, so chords, repeats, and editing-target
// rules can be tested without constructing events.

/** The subset of a keydown the keymap needs. */
export interface CalendarShortcutInput {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  repeat: boolean;
}

export type CalendarShortcutAction =
  | { type: 'setPeriod'; length: number }
  | { type: 'today' }
  | { type: 'shiftPeriod'; delta: -1 | 1 }
  | { type: 'create' }
  | { type: 'requestDelete' }
  | { type: 'copy' }
  | { type: 'cut' }
  | { type: 'paste' }
  | { type: 'duplicate' };

/**
 * True when keystrokes belong to a field rather than the calendar: text
 * inputs, textareas, selects, and contentEditable regions. Plain buttons and
 * divs are not editing targets, so shortcuts still fire while one is focused.
 */
export function isTextEditingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT';
}

/**
 * Resolve a keydown to a calendar action, or null when the key is not ours.
 *
 * Alt is never ours, so AltGr-style chords reach the OS untouched. Cmd/Ctrl
 * are ours for exactly four clipboard chords (copy, cut, paste, duplicate):
 * everything else — Cmd+T is a new tab, Cmd+[ is Back, Ctrl+W closes the tab,
 * Cmd+1–6 change tabs — must fall through rather than land in the bare-key
 * switch below. Shift chords (Cmd+Shift+D) and auto-repeat stay with the
 * browser too, so a held Cmd+D cannot stack up clones.
 *
 * Bare keys are unchanged: `key` is already the shifted character, so `T` is
 * Shift+t and Shift is not a blocker; holding `]` keeps paging, while create
 * and delete drop repeats because a held key must not open a stack of drafts
 * or dialogs.
 */
export function resolveCalendarShortcut(
  input: CalendarShortcutInput,
): CalendarShortcutAction | null {
  // Alt is never ours, and a chord that holds both modifiers is ambiguous.
  if (input.altKey) return null;
  if (input.metaKey && input.ctrlKey) return null;

  if (input.metaKey || input.ctrlKey) {
    if (input.shiftKey || input.repeat) return null;

    switch (input.key) {
      case 'c':
      case 'C':
        return { type: 'copy' };
      case 'x':
      case 'X':
        return { type: 'cut' };
      case 'v':
      case 'V':
        return { type: 'paste' };
      case 'd':
      case 'D':
        return { type: 'duplicate' };
      default:
        // Cmd+T / Cmd+[ / Cmd+1 / Ctrl+W belong to the browser: never fall
        // through to the bare-key switch.
        return null;
    }
  }

  if (
    input.repeat &&
    (input.key === 'c' ||
      input.key === 'C' ||
      input.key === 'Backspace' ||
      input.key === 'Delete')
  ) {
    return null;
  }

  switch (input.key) {
    case '1':
    case 'd':
    case 'D':
      return { type: 'setPeriod', length: 1 };
    case '2':
    case '3':
    case '4':
    case '5':
    case '6':
      return { type: 'setPeriod', length: Number(input.key) };
    case 'w':
    case 'W':
    case '0':
      return { type: 'setPeriod', length: 7 };
    case 't':
    case 'T':
      return { type: 'today' };
    case '[':
      return { type: 'shiftPeriod', delta: -1 };
    case ']':
      return { type: 'shiftPeriod', delta: 1 };
    case 'c':
    case 'C':
      return { type: 'create' };
    case 'Backspace':
    case 'Delete':
      return { type: 'requestDelete' };
    default:
      return null;
  }
}

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
  | { type: 'requestDelete' };

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
 * Chords are deliberately unhandled: Cmd/Ctrl/Alt belong to the browser and
 * the OS (Cmd+T is a new tab, Cmd+[ is Back, Cmd+C is copy), so those events
 * fall through untouched. Shift is not a blocker either — `key` is already the
 * shifted character, so `T` is Shift+t. Auto-repeat is allowed for navigation:
 * holding `]` to step periods is useful, and Today is idempotent. Create and
 * delete are different: a held key must not open a stack of drafts or dialogs,
 * so their repeats are dropped.
 */
export function resolveCalendarShortcut(
  input: CalendarShortcutInput,
): CalendarShortcutAction | null {
  if (input.metaKey || input.ctrlKey || input.altKey) return null;

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

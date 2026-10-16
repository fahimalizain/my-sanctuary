/** md+ shows the inspector as an anchored popover; below it, a bottom drawer. */
export const DESKTOP_MQ = '(min-width: 768px)';

/**
 * Build a CSS attribute selector for an event chip's `data-event-id`.
 * Escapes characters that would break a double-quoted attribute selector
 * so ids with quotes/backslashes still match.
 */
export function escapeAttrValue(value: string): string {
  return value.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
}

export function eventChipSelector(eventId: string): string {
  return `[data-event-id="${escapeAttrValue(eventId)}"]`;
}

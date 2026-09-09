import { queryKeys } from '@/app/queries/keys';

export type RealtimeMessage = {
  type: 'calendar.changed';
  calendar_id?: string;
};

/**
 * Parse a UserHub WebSocket text frame into a known RealtimeMessage.
 * Unknown type / invalid JSON → null.
 */
export function parseRealtimeMessage(raw: string): RealtimeMessage | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }

  if (parsed === null || typeof parsed !== 'object') {
    return null;
  }

  const obj = parsed as Record<string, unknown>;
  if (obj.type !== 'calendar.changed') {
    return null;
  }

  const message: RealtimeMessage = { type: 'calendar.changed' };
  if (typeof obj.calendar_id === 'string') {
    message.calendar_id = obj.calendar_id;
  }
  return message;
}

/** Every successful socket open, including first connect and reconnect. */
export function shouldInvalidateCalendarOnOpen(): boolean {
  return true;
}

export function shouldInvalidateCalendarOnMessage(
  msg: RealtimeMessage | null,
): boolean {
  return msg?.type === 'calendar.changed';
}

/** Query-key prefixes to catch up. Events required; calendars is cheap. */
export function calendarCatchupQueryKeys(): {
  events: readonly unknown[];
  calendars: readonly unknown[];
} {
  return {
    events: [...queryKeys.calendar.all, 'events'],
    calendars: queryKeys.calendar.calendars(),
  };
}

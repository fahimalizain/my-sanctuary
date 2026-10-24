import type {
  CalendarEvent,
  NewCalendarEventInput,
  PatchCalendarEventInput,
} from '@/app/types';

/**
 * Pending field edits for one event. Same shape as the PATCH body, except
 * `start_time_zone: ''` means "cleared" (stripped from the wire request).
 */
export type EventPatch = PatchCalendarEventInput;

function isEmpty(patch: EventPatch): boolean {
  return Object.keys(patch).length === 0;
}

/**
 * Merge a newer patch over an older pending one (last write wins per field).
 * All-day events carry civil dates, so a pending zone is dropped when the
 * merged result is all-day.
 */
export function mergeEventPatch(
  older: EventPatch,
  newer: EventPatch,
): EventPatch {
  const merged: EventPatch = { ...older, ...newer };
  if (merged.is_all_day === true) delete merged.start_time_zone;
  return merged;
}

/** Wire body: drop an empty (cleared) zone — empty zone means omit. */
export function toWirePatch(patch: EventPatch): EventPatch {
  const wire = { ...patch };
  if (wire.start_time_zone === '') delete wire.start_time_zone;
  return wire;
}

export interface EventPatchQueueHandlers {
  /** Perform one PATCH and resolve with the authoritative server event. */
  send(id: string, input: EventPatch): Promise<CalendarEvent>;
  /**
   * A request succeeded. `idle` is true when nothing else is pending for this
   * id, so the caller may drop its overlay if it matches the server row.
   */
  onSuccess(
    id: string,
    sent: EventPatch,
    event: CalendarEvent,
    idle: boolean,
  ): void;
  /** A request failed. `idle` as above. */
  onError(id: string, sent: EventPatch, err: unknown, idle: boolean): void;
}

/**
 * Per-event PATCH serializer. At most one request per event id is in flight;
 * edits made meanwhile are merged into a single pending patch and sent next.
 * Responses therefore arrive in edit order and a slow earlier response can
 * never land after (and clobber) a later one. Different events run in
 * parallel. `calendar_id` is exclusive on the server, so it goes in its own
 * request, after the field request.
 */
export class EventPatchQueue {
  private pending = new Map<string, EventPatch>();
  private running = new Set<string>();
  // Ids whose in-flight request result must be ignored (event was deleted).
  private cancelled = new Set<string>();

  constructor(private handlers: EventPatchQueueHandlers) {}

  /** Queue a patch for an event and start draining if idle. */
  enqueue(id: string, patch: EventPatch): void {
    if (isEmpty(patch)) return;
    this.pending.set(id, mergeEventPatch(this.pending.get(id) ?? {}, patch));
    if (!this.running.has(id)) void this.drain(id);
  }

  /**
   * Drop pending edits and ignore the in-flight request's result (no
   * callbacks), e.g. after the event is deleted. The mark applies to that one
   * request only, so a later enqueue for the same id works normally.
   */
  cancel(id: string): void {
    this.pending.delete(id);
    if (this.running.has(id)) this.cancelled.add(id);
  }

  /**
   * Cancel every id: drop all pending edits and ignore every in-flight
   * result (no callbacks). Used when the account changes, so a response that
   * resumes after logout cannot paint the previous account's event back onto
   * the overlay. Same per-id meaning as `cancel`, so a later enqueue for any
   * id still works normally.
   */
  cancelAll(): void {
    this.pending.clear();
    for (const id of this.running) this.cancelled.add(id);
  }

  /** True while a request is in flight or edits are waiting for this id. */
  isBusy(id: string): boolean {
    return this.running.has(id) || this.pending.has(id);
  }

  private take(id: string): EventPatch | null {
    const all = this.pending.get(id);
    if (!all) return null;
    const { calendar_id: move, ...fields } = all;
    if (!isEmpty(fields)) {
      if (move !== undefined) this.pending.set(id, { calendar_id: move });
      else this.pending.delete(id);
      return fields;
    }
    this.pending.delete(id);
    return { calendar_id: move };
  }

  private async drain(id: string): Promise<void> {
    this.running.add(id);
    try {
      for (let sent = this.take(id); sent; sent = this.take(id)) {
        let event: CalendarEvent | null = null;
        let failure: { err: unknown } | null = null;
        try {
          event = await this.handlers.send(id, toWirePatch(sent));
        } catch (err) {
          failure = { err };
        }
        if (this.cancelled.delete(id)) continue;
        const idle = !this.pending.has(id);
        // A throwing handler must not wedge the chain.
        try {
          if (failure) this.handlers.onError(id, sent, failure.err, idle);
          else this.handlers.onSuccess(id, sent, event!, idle);
        } catch {
          /* next request still goes out */
        }
      }
    } finally {
      this.running.delete(id);
    }
  }
}

/** True when the overlay still holds the value this patch key would write. */
function overlayHolds(
  ev: CalendarEvent,
  key: keyof EventPatch,
  patch: EventPatch,
): boolean {
  switch (key) {
    case 'summary':
      return ev.title === patch.summary;
    case 'description':
      return (ev.description ?? '') === patch.description;
    case 'start':
      return ev.start_time === patch.start;
    case 'end':
      return ev.end_time === patch.end;
    case 'is_all_day':
      return Boolean(ev.is_all_day) === patch.is_all_day;
    case 'start_time_zone':
      return (ev.start_time_zone?.trim() || '') === patch.start_time_zone;
    case 'calendar_id':
      return ev.calendar_id === patch.calendar_id;
  }
}

/**
 * Revert the fields of a failed patch on the overlay, using `base` (last known
 * server row). Each field reverts only while the overlay still holds the
 * failed value; a newer edit to that field is left alone.
 */
export function revertFailedPatch(
  overlay: CalendarEvent,
  base: CalendarEvent,
  failed: EventPatch,
): CalendarEvent {
  const next = { ...overlay };
  const revert = (key: keyof EventPatch, apply: () => void) => {
    if (failed[key] === undefined) return;
    if (overlayHolds(overlay, key, failed)) apply();
  };
  revert('summary', () => {
    next.title = base.title;
  });
  revert('description', () => {
    next.description = base.description;
  });
  revert('start', () => {
    next.start_time = base.start_time;
  });
  revert('end', () => {
    next.end_time = base.end_time;
  });
  revert('is_all_day', () => {
    next.is_all_day = base.is_all_day;
  });
  revert('start_time_zone', () => {
    next.start_time_zone = base.start_time_zone;
    next.end_time_zone = base.end_time_zone;
  });
  revert('calendar_id', () => {
    next.calendar_id = base.calendar_id;
    next.color = base.color;
  });
  return next;
}

export interface CreateFlushInput {
  /** Values the create POST carried, i.e. what the server already stored. */
  posted: PostedEventFields;
  /** Overlay (local) event as it stands when create returns. */
  local: CalendarEvent;
  /** Calendar the server created the event in. */
  serverCalendarId: string;
}

/**
 * The fields a create POST carried, used as the flush baseline. A draft always
 * posts a timed, zoneless row; a paste posts whatever the clipboard item was,
 * so all-day and time-zone writes are part of the baseline too.
 */
export interface PostedEventFields {
  summary: string;
  description: string;
  start: string;
  end: string;
  /** All-day flag the POST carried; absent/false means a timed row. */
  isAllDay?: boolean;
  /** Zone the POST carried (timed only); '' / absent means none. */
  startTimeZone?: string;
}

/** Baseline for a create body: exactly the values the server received. */
export function postedFieldsFromCreate(
  input: NewCalendarEventInput,
): PostedEventFields {
  return {
    summary: input.summary,
    description: input.description ?? '',
    start: input.start,
    end: input.end,
    isAllDay: input.is_all_day === true,
    startTimeZone: input.start_time_zone ?? '',
  };
}

/**
 * Fields the user changed on a temp-id event while its create POST was in
 * flight. Null when nothing needs flushing.
 *
 * The diff is against what the POST actually carried — a paste may have
 * created an all-day or zoned row, and re-sending those fields for an
 * untouched copy would be a redundant (and for `is_all_day`, wrong) write.
 * Times travel with the flag / zone whenever any of the three changed: the
 * server reads them together, and a flag flip needs both edges to be
 * meaningful.
 */
export function buildCreateFlushPatch(
  input: CreateFlushInput,
): EventPatch | null {
  const { posted, local, serverCalendarId } = input;
  const allDay = Boolean(local.is_all_day);
  const postedAllDay = Boolean(posted.isAllDay);
  // An all-day row carries civil dates; a zone on either side is ignored.
  const zone = allDay ? '' : local.start_time_zone?.trim() || '';
  const postedZone = postedAllDay ? '' : posted.startTimeZone?.trim() || '';

  const patch: EventPatch = {};
  if (local.title !== posted.summary) patch.summary = local.title;
  if ((local.description ?? '') !== posted.description) {
    patch.description = local.description ?? '';
  }
  // All-day conversion always needs start/end + flag together.
  if (
    allDay !== postedAllDay ||
    zone !== postedZone ||
    local.start_time !== posted.start ||
    local.end_time !== posted.end
  ) {
    patch.start = local.start_time;
    patch.end = local.end_time;
    // An all-day row must keep its flag on any patch that carries times or
    // the server would store it as timed; a flip back needs the explicit
    // false for the same reason.
    if (allDay) {
      patch.is_all_day = true;
    } else if (postedAllDay) {
      patch.is_all_day = false;
    }
    // '' is the queue's "cleared" marker (stripped from the wire request):
    // it supersedes a zone the create carried.
    if (!allDay && zone !== postedZone) patch.start_time_zone = zone;
  }
  // The queue sends the field request first, then the (exclusive) move.
  if (local.calendar_id !== serverCalendarId) {
    patch.calendar_id = local.calendar_id;
  }
  return isEmpty(patch) ? null : patch;
}

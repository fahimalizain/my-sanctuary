// One per-event local change layer: what this client wants an event to look
// like, kept independent of whatever the server last sent back.
//
// The provider paints `server row ⊕ local intent` instead of trusting the
// cache, so the gap-8 race — a GET that started before a PATCH but resolves
// after it, overwriting the cached row — cannot repaint the old value: the
// intent stays painted over the stale row.
//
// Intent is retired field by field, and only by a *server read* (an events GET,
// never our own PATCH / create response):
//
//   1. the field must have been acknowledged by a successful write, and
//   2. the painted row must come from a read that STARTED after that ack
//      (read and ack stamps share one monotonic clock).
//
// A read that started before the ack is stale: it retires nothing, and it
// cannot repaint over the intent either. A read that started after the ack
// retires the field whether it agrees (display is unchanged, the server now
// holds the same value) or disagrees (the server won, e.g. an edit from
// another device). Retiring a disagreement is what keeps a one-off mismatch
// from masking every later server change forever.
//
// React-free: the provider owns the state and wires the cache lookup.

import type { CalendarEvent, PatchCalendarEventInput } from '@/app/types';
import { calendarServerReadStamp } from './calendar-server-reads';
import { applyEventOverlays, type EventOverlay } from './event-overlays';
import { mergeEventPatch, type EventPatch } from './event-patch-queue';
import { colorForCalendar } from './week-layout';

/** PATCH field names are the local-intent vocabulary (`summary` is `title`). */
export type EventIntentField = keyof PatchCalendarEventInput;

export const EVENT_INTENT_FIELDS: readonly EventIntentField[] = [
  'summary',
  'description',
  'start',
  'end',
  'is_all_day',
  'start_time_zone',
  'calendar_id',
];

/** A patch value as it compares on a display row. */
type IntentValue = string | boolean;

/** The value a field writes, read off a display row. */
export function intentFieldValue(
  event: CalendarEvent,
  field: EventIntentField,
): IntentValue {
  switch (field) {
    case 'summary':
      return event.title;
    case 'description':
      return event.description ?? '';
    case 'start':
      return event.start_time;
    case 'end':
      return event.end_time;
    case 'is_all_day':
      return event.is_all_day === true;
    case 'start_time_zone':
      return (event.start_time_zone ?? '').trim();
    case 'calendar_id':
      return event.calendar_id;
  }
}

/** The value a patch writes for a field, or undefined when it says nothing. */
function patchValue(
  patch: EventPatch,
  field: EventIntentField,
): IntentValue | undefined {
  switch (field) {
    case 'summary':
      return patch.summary;
    case 'description':
      return patch.description;
    case 'start':
      return patch.start;
    case 'end':
      return patch.end;
    case 'is_all_day':
      return patch.is_all_day;
    case 'start_time_zone':
      return patch.start_time_zone === undefined
        ? undefined
        : patch.start_time_zone.trim();
    case 'calendar_id':
      return patch.calendar_id;
  }
}

/**
 * Write `patch[field] = value` through a generic so the union of field names
 * still type-checks (TS cannot narrow an indexed write by a union key).
 */
function assignPatchField<K extends EventIntentField>(
  patch: EventPatch,
  field: K,
  value: EventPatch[K],
): void {
  patch[field] = value;
}

/** Every field of `event`, in patch vocabulary. */
function patchFromEvent(event: CalendarEvent): EventPatch {
  const patch: EventPatch = {};
  for (const field of EVENT_INTENT_FIELDS) {
    assignPatchField(patch, field, intentFieldValue(event, field));
  }
  return patch;
}

/**
 * Paint local intent over a row. The row is only cloned when the intent says
 * something, so an untouched entry keeps the server object identity.
 */
export function applyEventIntent(
  row: CalendarEvent,
  intent: EventPatch,
): CalendarEvent {
  const fields = EVENT_INTENT_FIELDS.filter(
    (field) => patchValue(intent, field) !== undefined,
  );
  if (fields.length === 0) return row;

  const next = { ...row };
  for (const field of fields) {
    switch (field) {
      case 'summary':
        if (intent.summary !== undefined) next.title = intent.summary;
        break;
      case 'description':
        if (intent.description !== undefined) {
          next.description = intent.description;
        }
        break;
      case 'start':
        if (intent.start !== undefined) next.start_time = intent.start;
        break;
      case 'end':
        if (intent.end !== undefined) next.end_time = intent.end;
        break;
      case 'is_all_day':
        next.is_all_day = intent.is_all_day === true;
        // All-day rows carry civil dates; a zone read off the old row would
        // contradict the flag until the server's answer lands.
        if (intent.is_all_day) {
          next.start_time_zone = '';
          next.end_time_zone = '';
        }
        break;
      case 'start_time_zone': {
        const zone = intent.start_time_zone ?? '';
        next.start_time_zone = zone;
        next.end_time_zone = zone;
        break;
      }
      case 'calendar_id':
        if (intent.calendar_id !== undefined) {
          next.calendar_id = intent.calendar_id;
          // The chip color follows the destination, exactly as the immediate
          // paint did before the layer owned it.
          next.color = colorForCalendar(intent.calendar_id);
        }
        break;
    }
  }
  return next;
}

/** A successful write for one field: the accepted value and its ack stamp. */
interface FieldAck {
  value: IntentValue;
  stamp: number;
}

/**
 * Everything this client holds for one event id: field intent, create metadata
 * for a local-only row, or delete intent. Field / ack / deleted are the
 * change layer proper; the rest is the local create lifecycle (slice 3).
 */
interface ChangeEntry {
  /** Local-only row for a temp create id: there is no server row yet. */
  localEvent?: CalendarEvent;
  /** Field intent painted over the server row (server ids only). */
  fields: EventPatch;
  /** Field value the server accepted, stamped with the ack clock. */
  acked: Map<EventIntentField, FieldAck>;
  /** A write for this event failed; its intent is not on the server yet. */
  failed: boolean;
  /** Delete intent owns the id (the row is hidden). */
  deleted: boolean;
  /** The one open, unposted draft among the temp rows. */
  draft: boolean;
  /** The inspector calendar pick belongs to this create. */
  calendarPicked: boolean;
}

function emptyEntry(): ChangeEntry {
  return {
    fields: {},
    acked: new Map(),
    failed: false,
    deleted: false,
    draft: false,
    calendarPicked: false,
  };
}

export interface PreparedEventPatch {
  /** The body worth sending: no-op fields dropped, time edges coupled. */
  patch: EventPatch;
  /** Fields dropped because the server row already holds them. */
  skipped: EventPatch;
}

/** True when this patch must carry start + end to be meaningful. */
function needsTimeEdges(patch: EventPatch): boolean {
  return patch.is_all_day !== undefined || patch.start_time_zone !== undefined;
}

/**
 * An all-day or zone write is read together with its two edges by the API, so
 * a squashed patch that carries the flag / zone must carry start and end too.
 * The values come from the local intent, then the known server row.
 */
function addTimeEdges(
  patch: EventPatch,
  intent: EventPatch | undefined,
  serverRow: CalendarEvent | undefined,
): void {
  if (!needsTimeEdges(patch)) return;
  if (patch.start === undefined) {
    patch.start = intent?.start ?? serverRow?.start_time;
  }
  if (patch.end === undefined) {
    patch.end = intent?.end ?? serverRow?.end_time;
  }
}

export class EventChangeLayer {
  private byId = new Map<string, ChangeEntry>();

  constructor(
    /** Latest known server row for an id (the range-keyed events cache). */
    private readonly findServerRow: (
      id: string,
    ) => CalendarEvent | undefined = () => undefined,
  ) {}

  // ── Recording ──────────────────────────────────────────────────────────

  /** Paint a local-only create row wholesale (temp id, no server row yet). */
  setLocalEvent(id: string, event: CalendarEvent): void {
    const entry = this.entry(id);
    entry.localEvent = event;
    entry.deleted = false;
  }

  /**
   * Record field intent. Last write wins per field, with the same all-day
   * rules the PATCH queue used (a pending zone is dropped once the row is
   * all-day). Returns the entry's unsent fields for the caller to enqueue.
   */
  edit(id: string, patch: EventPatch): void {
    if (Object.keys(patch).length === 0) return;
    const entry = this.entry(id);
    entry.deleted = false;
    entry.fields = mergeEventPatch(entry.fields, patch);
  }

  /**
   * Fold a caller's full display row into field intent by diffing against the
   * row currently painted. Only fields the caller actually changed become
   * intent, so a paint can never mask a remote edit by accident.
   */
  recordRow(id: string, event: CalendarEvent): boolean {
    // With no entry yet the paint was the server row itself; with one, the
    // paint is `base ⊕ intent`. Diffing against it means only the fields the
    // caller actually changed become intent.
    const display = this.byId.has(id)
      ? this.displayFor(id)
      : this.findServerRow(id);
    if (!display) {
      // No base to diff against: keep the whole row rather than guess which
      // fields changed. The send step still drops what the server has.
      this.edit(id, patchFromEvent(event));
      return true;
    }
    const patch: EventPatch = {};
    for (const field of EVENT_INTENT_FIELDS) {
      if (intentFieldValue(event, field) === intentFieldValue(display, field)) {
        continue;
      }
      assignPatchField(patch, field, intentFieldValue(event, field));
    }
    this.edit(id, patch);
    return Object.keys(patch).length > 0;
  }

  /** Overlay a delete for this id. It replaces any pending paint. */
  remove(id: string): void {
    const entry = emptyEntry();
    entry.deleted = true;
    // A delete owns the id: paint, ack and local-create metadata all go with
    // it, exactly like the old last-write-wins overlay.
    this.byId.set(id, entry);
  }

  /** Drop everything this client holds for the id (settled / discarded). */
  clear(id: string): void {
    this.byId.delete(id);
  }

  /** Drop every record (logout / account switch). */
  reset(): void {
    this.byId.clear();
  }

  ids(): string[] {
    return Array.from(this.byId.keys());
  }

  get size(): number {
    return this.byId.size;
  }

  // ── Display ────────────────────────────────────────────────────────────

  /** The row this client paints for `id`, or undefined when it has nothing. */
  displayFor(id: string): CalendarEvent | undefined {
    const entry = this.byId.get(id);
    if (!entry || entry.deleted) return undefined;
    if (entry.localEvent) {
      return applyEventIntent(entry.localEvent, entry.fields);
    }
    const base = this.findServerRow(id);
    if (!base) return undefined;
    return applyEventIntent(base, entry.fields);
  }

  /** Snapshot of the layer as one overlay, for `getOverlay` consumers. */
  getOverlay(id: string): EventOverlay | undefined {
    const entry = this.byId.get(id);
    if (!entry) return undefined;
    if (entry.deleted) return { op: 'delete', id };
    const event = this.displayFor(id);
    if (!event) return undefined;
    return { op: 'upsert', event };
  }

  /**
   * Paint the visible server list over the local layer, retiring acknowledged
   * fields whose painted row comes from a read that started after their ack.
   * Retirement runs here because this is the only place the painted row and
   * its read stamp meet: a field is never retired by a row we are not showing.
   */
  paint(server: CalendarEvent[]): CalendarEvent[] {
    if (this.byId.size === 0) return server;

    const overlays: EventOverlay[] = [];
    const inServerList = new Set<string>();

    for (const row of server) {
      const entry = this.byId.get(row.id);
      if (!entry) continue;
      inServerList.add(row.id);
      if (entry.deleted) {
        overlays.push({ op: 'delete', id: row.id });
        continue;
      }
      const live = this.retireForRow(row.id, row, entry);
      if (!live) continue;
      overlays.push({
        op: 'upsert',
        event: applyEventIntent(row, live.fields),
      });
    }

    for (const [id, entry] of this.byId) {
      if (inServerList.has(id) || entry.deleted || !entry.localEvent) continue;
      overlays.push({
        op: 'upsert',
        event: applyEventIntent(entry.localEvent, entry.fields),
      });
    }

    return applyEventOverlays(server, overlays);
  }

  /**
   * Retire the fields of `entry` that a server read has settled. A read that
   * started before the ack is stale and retires nothing (and the caller keeps
   * painting the intent over it).
   */
  private retireForRow(
    id: string,
    row: CalendarEvent,
    entry: ChangeEntry,
  ): ChangeEntry | undefined {
    const readStamp = calendarServerReadStamp(row);
    if (readStamp === 0 || entry.acked.size === 0) return entry;

    for (const [field, ack] of entry.acked) {
      const intentValue = patchValue(entry.fields, field);
      if (intentValue === undefined) {
        // The field was retired / superseded meanwhile; the ack is moot.
        entry.acked.delete(field);
        continue;
      }
      // A newer local value is not acknowledged yet: it owns the field.
      if (intentValue !== ack.value) continue;
      // The read started before the write was acknowledged: its row predates
      // the write, so it can neither agree nor disagree.
      if (readStamp <= ack.stamp) continue;
      delete entry.fields[field];
      entry.acked.delete(field);
    }

    return this.cleanup(id, entry);
  }

  // ── Sending ────────────────────────────────────────────────────────────

  /** Fields this client still needs the server to accept. */
  unsentPatch(id: string): EventPatch | null {
    const entry = this.byId.get(id);
    if (!entry || entry.deleted) return null;
    const patch: EventPatch = {};
    let any = false;
    for (const field of EVENT_INTENT_FIELDS) {
      const value = patchValue(entry.fields, field);
      if (value === undefined) continue;
      const ack = entry.acked.get(field);
      if (ack && ack.value === value) continue;
      assignPatchField(patch, field, entry.fields[field]);
      any = true;
    }
    return any ? patch : null;
  }

  /**
   * What of `patch` is worth a request now: fields the latest known server row
   * already holds are dropped (`skipped`, so the caller can retire them), and
   * a surviving all-day / zone write keeps its time edges.
   */
  prepareSend(
    id: string,
    patch: EventPatch,
    serverRow: CalendarEvent | undefined,
  ): PreparedEventPatch {
    const entry = this.byId.get(id);
    const prepared: EventPatch = { ...patch };
    addTimeEdges(prepared, entry?.fields, serverRow);

    const skipped: EventPatch = {};
    if (serverRow) {
      for (const field of EVENT_INTENT_FIELDS) {
        const value = patchValue(prepared, field);
        if (value === undefined) continue;
        if (intentFieldValue(serverRow, field) !== value) continue;
        assignPatchField(skipped, field, prepared[field]);
        delete prepared[field];
      }
    }

    if (needsTimeEdges(prepared)) {
      addTimeEdges(prepared, entry?.fields, serverRow);
      // An edge re-added for the flag's sake is being sent after all: it must
      // not be reported as skipped, or a failure would lose it.
      for (const field of ['start', 'end'] as const) {
        if (prepared[field] !== undefined) delete skipped[field];
      }
    }

    return { patch: prepared, skipped };
  }

  /**
   * The server already holds these values (they were dropped as no-ops), so
   * the intent behind them is settled without a request. A newer local value
   * for the same field owns it and stays.
   */
  retireSkipped(id: string, skipped: EventPatch): void {
    const entry = this.byId.get(id);
    if (!entry) return;
    for (const field of EVENT_INTENT_FIELDS) {
      const value = patchValue(skipped, field);
      if (value === undefined) continue;
      if (patchValue(entry.fields, field) !== value) continue;
      delete entry.fields[field];
      entry.acked.delete(field);
    }
    this.cleanup(id, entry);
  }

  /** A PATCH succeeded: remember the values the server accepted. */
  acknowledge(id: string, sent: EventPatch, stamp: number): void {
    const entry = this.byId.get(id);
    if (!entry) return;
    let acknowledged = false;
    for (const field of EVENT_INTENT_FIELDS) {
      const value = patchValue(sent, field);
      if (value === undefined) continue;
      entry.acked.set(field, { value, stamp });
      acknowledged = true;
    }
    // A failure is only cleared once nothing is left unsent: another field may
    // still be waiting for the retry this flag advertises.
    if (acknowledged && !this.unsentPatch(id)) entry.failed = false;
    this.cleanup(id, entry);
  }

  /**
   * A PATCH failed. The intent is kept — a failed edit is the user's data, and
   * reverting it silently loses work. It stays painted and unacknowledged;
   * `retryFailedPatches` (or the next edit on this event) re-sends it.
   */
  noteFailure(id: string): void {
    const entry = this.byId.get(id);
    if (entry) entry.failed = true;
  }

  /** True while some failed write still has intent the server does not have. */
  hasFailedWrites(): boolean {
    for (const [id, entry] of this.byId) {
      if (entry.failed && this.unsentPatch(id)) return true;
    }
    return false;
  }

  // ── Local create lifecycle ─────────────────────────────────────────────

  /** Mark this temp row as the one open, unposted draft. */
  markDraft(id: string): void {
    this.entry(id).draft = true;
  }

  /** The draft id, if one is open. */
  getDraftId(): string | null {
    for (const [id, entry] of this.byId) {
      if (entry.draft) return id;
    }
    return null;
  }

  /** The draft was sent (or discarded): it is no longer a local-only draft. */
  releaseDraft(id: string): void {
    const entry = this.byId.get(id);
    if (!entry?.draft) return;
    entry.draft = false;
    this.cleanup(id, entry);
  }

  /** Remember an inspector calendar pick made for this create. */
  markCalendarPicked(id: string): void {
    this.entry(id).calendarPicked = true;
  }

  isCalendarPicked(id: string): boolean {
    return this.byId.get(id)?.calendarPicked === true;
  }

  /** A create returned: the temp row it painted has no local row any more. */
  settleCreate(tempId: string): void {
    this.byId.delete(tempId);
  }

  // ── Internals ──────────────────────────────────────────────────────────

  private entry(id: string): ChangeEntry {
    const existing = this.byId.get(id);
    if (existing) return existing;
    const created = emptyEntry();
    this.byId.set(id, created);
    return created;
  }

  /**
   * Forget an entry that holds nothing. Delete intent and any local create
   * metadata keep it alive; a bare field entry that retired its last field
   * does not.
   */
  private cleanup(id: string, entry: ChangeEntry): ChangeEntry | undefined {
    const holds =
      entry.deleted ||
      entry.localEvent !== undefined ||
      entry.draft ||
      entry.calendarPicked ||
      Object.keys(entry.fields).length > 0;
    if (holds) return entry;
    if (this.byId.get(id) === entry) this.byId.delete(id);
    return undefined;
  }
}

// The row contract HomePage owns and the day/warehouse sections render
// against — every mutation stays in HomePage; the sections only hand back
// these callbacks.

import type { AgendaItemRecord, TaskRecord } from '@/app/types';
import { isAgendaItemPending } from './agenda-helpers';

export interface AgendaRowHandlers {
  onReschedule: (item: AgendaItemRecord, date: string) => void;
  onMoveToBacklog: (item: AgendaItemRecord) => void;
  /** Park a dated `PLANNED` row in the Planned warehouse (`{ unschedule: true
   *  }`, ADR 0010) — never a status change. Task rows only. */
  onUnschedule: (item: AgendaItemRecord) => void;
  onCompleteTask: (item: AgendaItemRecord) => void;
  onStartTask: (item: AgendaItemRecord) => void;
  onPauseTask: (item: AgendaItemRecord) => void;
  onOpenTask: (task: TaskRecord) => void;
  onCompleteOccurrence: (item: AgendaItemRecord) => void;
  onSkipOccurrence: (item: AgendaItemRecord) => void;
  onStartOccurrence: (item: AgendaItemRecord) => void;
  onPauseOccurrence: (item: AgendaItemRecord) => void;
  onRenameOccurrence: (item: AgendaItemRecord) => void;
  /** The slot whose request is in flight for this row, or null — feeds
   *  `AgendaItemRow`'s `pending` spinner/disabled contract. */
  pendingOf: (itemId: string) => RowBusySlot | null;
}

/** The one control on a row that shows the spinner while its mutation is in
 *  flight. Check covers the circle (complete / undo done / undo discard); play
 *  covers Play and Pause; calendar is the row menu's move trigger; grip is a
 *  drag-driven reorder. */
export type RowBusySlot = 'check' | 'play' | 'calendar' | 'grip';

/** A successful move's transient confirmation. `itemId` lets Show scroll
 *  straight to the exact row (the day header is only a fallback). */
export interface ActionNotice {
  message: string;
  itemId: string;
  /** Set for a date move; omitted for a warehouse move. */
  date?: string;
  /** Set by the warehouse moves so Show falls back to the RIGHT section header
   *  when the row itself is not mounted (a Planned move never scrolls to the
   *  Backlog header, and vice versa). */
  section?: 'planned' | 'backlog';
}

/** A past day's leftovers that `move-to-today` will actually sweep: waiting
 *  tasks and `pending` occurrences. An `in_progress` OCCURRENCE stays put
 *  (its timer is still running — the API keeps it too); an `in_progress` task
 *  moves, exactly like the API. */
export function canMoveToToday(item: AgendaItemRecord): boolean {
  return isAgendaItemPending(item) && item.occurrence?.status !== 'in_progress';
}

import { useMemo } from 'react';
import { useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import type { AgendaItemRecord } from '@/app/types';
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import {
  agendaDateLabel,
  BACKLOG_DROPPABLE_ID,
  groupItemsByDate,
  isAgendaRowDraggable,
} from './agenda-helpers';
import { backlogDayElementId } from './agenda-dom';
import type { AgendaRowHandlers } from './agenda-row-contract';

/** The Backlog (ADR 0007): everything past the day stream, in the two
 *  stretches the server returns — dated rows grouped by their own day (a
 *  same-day drop reorders through the agenda `/move`; a drop onto another
 *  dated day reschedules onto that date, which the server accepts because every
 *  one of them is `>= today`), then the `OPEN` rows as ONE Board-ordered list
 *  (reordering them is the Board's task `/move`, never the agenda). The section
 *  itself is the droppable that sends a row back to the backlog, so an empty
 *  section still accepts a drop. Warehoused Planned rows (ADR 0010) live in
 *  their own section above; dated Planned rows still render in their day
 *  groups here. */
export function BacklogSection({
  serverToday,
  dated,
  open,
  handlers,
  highlightedItemId,
}: {
  serverToday: string;
  dated: AgendaItemRecord[];
  open: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  highlightedItemId: string | null;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: BACKLOG_DROPPABLE_ID });
  // Server order already puts the dated stretch first; grouping in encounter
  // order keeps each day's rows in `sort_order`, and never alphabetizes.
  const datedGroups = useMemo(() => groupItemsByDate(dated), [dated]);
  const empty = dated.length === 0 && open.length === 0;

  const renderRow = (entry: AgendaItemRecord) =>
    isAgendaRowDraggable(entry) ? (
      <SortableAgendaItemRow
        key={entry.id}
        item={entry}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        pending={handlers.pendingOf(entry.id)}
        {...handlers}
      />
    ) : (
      <AgendaItemRow
        key={entry.id}
        item={entry}
        isDragging={false}
        showHandle={false}
        serverToday={serverToday}
        highlighted={highlightedItemId === entry.id}
        pending={handlers.pendingOf(entry.id)}
        {...handlers}
      />
    );

  return (
    <section ref={setNodeRef} id={BACKLOG_DROPPABLE_ID} aria-label="Backlog">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur">
        <h2 className="font-heading text-sm font-semibold text-foreground">
          Backlog
        </h2>
      </header>

      <div
        className={
          isOver
            ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
            : 'min-h-10 px-1 py-2 transition-colors'
        }
      >
        {empty && (
          <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
            Nothing in the backlog
          </p>
        )}
        {[...datedGroups.entries()].map(([date, rows]) => (
          <div
            key={date}
            id={backlogDayElementId(date)}
            className="space-y-2 pb-2"
          >
            <h3 className="px-1 text-xs font-medium tracking-wide text-muted-foreground">
              {agendaDateLabel(date, serverToday)}
            </h3>
            <SortableContext
              items={rows.filter(isAgendaRowDraggable).map((entry) => entry.id)}
              strategy={verticalListSortingStrategy}
            >
              <div className="space-y-2">{rows.map(renderRow)}</div>
            </SortableContext>
          </div>
        ))}
        {open.length > 0 && (
          <SortableContext
            items={open.filter(isAgendaRowDraggable).map((entry) => entry.id)}
            strategy={verticalListSortingStrategy}
          >
            <div className="space-y-2 pt-1">{open.map(renderRow)}</div>
          </SortableContext>
        )}
      </div>
    </section>
  );
}

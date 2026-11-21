import { useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import type { AgendaItemRecord } from '@/app/types';
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import { isAgendaRowDraggable, PLANNED_DROPPABLE_ID } from './agenda-helpers';
import type { AgendaRowHandlers } from './agenda-row-contract';

/** The Planned warehouse (ADR 0010): every `PLANNED` row parked at/after the
 *  10-year horizon, in the Planned column's Board order — the server already
 *  sorted it, so it is never re-sorted here. Always visible, so an empty
 *  section still accepts a drop; a drop ON the section means "the end of the
 *  Planned list", exactly like a day container. Reordering is the Board's
 *  `POST /api/tasks/:id/move { status: 'PLANNED', sort_order }`, never the
 *  agenda's `/move`. It sits immediately ABOVE the Backlog and owns nothing
 *  else: dated `PLANNED` rows stay in their day groups in the stream. */
export function PlannedSection({
  serverToday,
  planned,
  handlers,
  highlightedItemId,
}: {
  serverToday: string;
  planned: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  highlightedItemId: string | null;
}) {
  const { setNodeRef, isOver } = useDroppable({ id: PLANNED_DROPPABLE_ID });

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
    <section ref={setNodeRef} id={PLANNED_DROPPABLE_ID} aria-label="Planned">
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur">
        <h2 className="font-heading text-sm font-semibold text-foreground">
          Planned
        </h2>
      </header>

      <div
        className={
          isOver
            ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
            : 'min-h-10 px-1 py-2 transition-colors'
        }
      >
        {planned.length === 0 && (
          <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
            Nothing planned
          </p>
        )}
        {planned.length > 0 && (
          <SortableContext
            items={planned
              .filter(isAgendaRowDraggable)
              .map((entry) => entry.id)}
            strategy={verticalListSortingStrategy}
          >
            <div className="space-y-2">{planned.map(renderRow)}</div>
          </SortableContext>
        )}
      </div>
    </section>
  );
}

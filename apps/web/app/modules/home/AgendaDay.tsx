import { useEffect, useRef, useState } from 'react';
import { useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { AgendaItemRecord } from '@/app/types';
import { AgendaItemRow, SortableAgendaItemRow } from './AgendaItemRow';
import {
  agendaDateLabel,
  isAgendaItemPending,
  isAgendaRowDraggable,
  partitionAgendaItems,
  shouldShowWaitingSection,
  splitWaitingRows,
  waitingSectionLabel,
} from './agenda-helpers';
import { dayElementId, waitingDayElementId } from './agenda-dom';
import { canMoveToToday, type AgendaRowHandlers } from './agenda-row-contract';

/** One day of the stream: sticky header (with the past-day catch-up), the
 *  Waiting section (ADR 0011 — the day's living `WAITING` rows, between the
 *  header and the main pile), the living pile as its own droppable + sortable
 *  list (past days are not drop targets), then the Completed dump — never
 *  draggable, never a drop target. */
export function AgendaDay({
  day,
  serverToday,
  items,
  handlers,
  onMoveDayToToday,
  loadingHistory,
  highlightedItemId,
  dragging,
}: {
  day: string;
  serverToday: string;
  items: AgendaItemRecord[];
  handlers: AgendaRowHandlers;
  onMoveDayToToday: (day: string) => void;
  loadingHistory: boolean;
  highlightedItemId: string | null;
  /** True while a task row (not a routine occurrence) is being dragged. An
   *  empty Waiting section only exists during such a drag — it is a drop
   *  target, not permanent copy. */
  dragging: boolean;
}) {
  const { living, completed } = partitionAgendaItems(items);
  // ADR 0011: the day's WAITING rows get their own section between the header
  // and the main pile — `rest` is what the main list renders, so a waiting row
  // is never painted twice. Each pile gets its own SortableContext; a drop on
  // the Waiting section is a hand-off (or a waiting in-section move), never a
  // normal day reschedule.
  const { waiting, rest } = splitWaitingRows(living);
  const pendingCount = items.filter(isAgendaItemPending).length;
  // The count above includes a running occurrence, but the sweep leaves it
  // behind — the button may only appear when something would actually move.
  const canMoveDay = items.some(canMoveToToday);
  const isPast = !!serverToday && day < serverToday;
  const draggable = rest.filter(isAgendaRowDraggable);
  const waitingDraggable = waiting.filter(isAgendaRowDraggable);
  // An empty Waiting section exists only while a drag is running: then it is
  // a drop target for a hand-off, and it renders as one. With no drag, a day
  // that has nothing waiting shows nothing at all. Past days only render the
  // section when they actually hold waiting rows, and accept only those rows'
  // in-section reorder.
  const showWaitingSection = shouldShowWaitingSection(
    waiting.length,
    isPast,
    dragging,
  );
  // The section is collapsed by default — a day with a long waiting list would
  // otherwise push the actual plan below the fold. The count in the header is
  // the whole collapsed state; clicking slides the rows open. Keep the body
  // mounted (with `inert`) so the grid-rows transition can close again and
  // Show can still find a highlighted row before the open state commits.
  const [waitingOpen, setWaitingOpen] = useState(false);
  const waitingBodyId = `waiting-body-${day}`;
  const waitingCountLabel = waitingSectionLabel(waiting.length);
  const highlightedWaiting =
    highlightedItemId !== null &&
    waiting.some((entry) => entry.id === highlightedItemId);
  const waitingExpanded = waitingOpen || highlightedWaiting;
  useEffect(() => {
    if (highlightedWaiting) setWaitingOpen(true);
  }, [highlightedWaiting]);
  // A row handed into a previously empty section should reveal itself; the
  // count would otherwise be the only feedback once the drop zone unmounts.
  const previousWaitingCount = useRef(waiting.length);
  useEffect(() => {
    if (waiting.length > previousWaitingCount.current) setWaitingOpen(true);
    previousWaitingCount.current = waiting.length;
  }, [waiting.length]);
  // Leaving zero behind must not keep the next arrival open by default.
  useEffect(() => {
    if (waiting.length === 0) setWaitingOpen(false);
  }, [waiting.length]);
  const { setNodeRef, isOver } = useDroppable({
    id: dayElementId(day),
    data: { day },
    // The API rejects a reschedule into the past — a past day never accepts
    // a drop.
    disabled: isPast,
  });
  const { setNodeRef: setWaitingNodeRef, isOver: waitingIsOver } = useDroppable(
    {
      id: waitingDayElementId(day),
      // `empty` lets Home's collision strategy keep the transient strip
      // pointer-only: `closestCenter` must still prefer the day pile just
      // below the header for drops aimed at the top of the pile.
      data: { day, waiting: true, empty: waiting.length === 0 },
      // A past section only accepts its OWN waiting rows' in-section reorder;
      // `resolveAgendaDrop` refuses every cross-day/date-changing drop there.
    },
  );
  const empty = items.length === 0;
  // While the history row is above the stream, the day headers stick below
  // it (the row is 2rem tall) instead of sliding underneath it.
  const stickyTop = loadingHistory ? 'top-8' : 'top-0';

  return (
    <section id={dayElementId(day)}>
      <header
        className={
          empty
            ? `sticky ${stickyTop} z-10 flex items-center gap-2 border-b border-border/50 bg-cream/95 px-1 py-1.5 backdrop-blur`
            : `sticky ${stickyTop} z-10 flex items-center gap-2 border-b border-border/60 bg-cream/95 px-1 py-2 backdrop-blur`
        }
      >
        <h2
          className={
            empty
              ? 'font-heading text-xs font-medium text-muted-foreground'
              : 'font-heading text-sm font-semibold text-foreground'
          }
        >
          {agendaDateLabel(day, serverToday)}
        </h2>
        {isPast && pendingCount > 0 && (
          <>
            <span className="flex-shrink-0 rounded-full bg-amber-400/20 px-1.5 py-0.5 text-[10px] font-semibold text-amber-700">
              {pendingCount} pending
            </span>
            {canMoveDay && (
              <button
                type="button"
                onClick={() => onMoveDayToToday(day)}
                aria-label={`Move all pending items from ${day} to today`}
                className="ml-auto flex-shrink-0 rounded-full bg-amber-400/15 px-2 py-1 text-[11px] font-medium text-amber-700 transition-colors hover:bg-amber-400/25"
              >
                Move all to today
              </button>
            )}
          </>
        )}
        {/* An empty day's Waiting target only exists during a drag. Keep it
            inside the sticky header's own box: it must not take layout space
            (or every day below it would shift the moment the drag starts) and
            it must not cover the pile below (or it would steal ordinary
            drops). The date stays in the copy so cross-day drags can still be
            aimed. */}
        {waiting.length === 0 && showWaitingSection && (
          <div
            ref={setWaitingNodeRef}
            aria-hidden
            className={cn(
              'absolute inset-0 z-20 flex items-center justify-center border border-dashed bg-cream/95 px-1 text-center text-[11px] font-medium tracking-wide shadow-sm backdrop-blur transition-colors',
              waitingIsOver
                ? 'border-amber-400/70 text-amber-800 ring-1 ring-amber-400/30'
                : 'border-amber-400/40 text-amber-700/70',
            )}
          >
            <span className="truncate">
              {agendaDateLabel(day, serverToday)} · Drop here to move it to
              Waiting
            </span>
          </div>
        )}
      </header>

      {showWaitingSection && waiting.length > 0 && (
        <SortableContext
          items={
            waitingExpanded ? waitingDraggable.map((entry) => entry.id) : []
          }
          strategy={verticalListSortingStrategy}
        >
          <section
            ref={setWaitingNodeRef}
            className="px-1 pb-2"
            aria-label={`Waiting · ${agendaDateLabel(day, serverToday)}`}
          >
            <button
              type="button"
              onClick={() => setWaitingOpen((open) => !open)}
              aria-expanded={waitingExpanded}
              aria-controls={waitingBodyId}
              className={cn(
                'mt-2 flex w-full items-center gap-1.5 rounded-lg px-1 py-1 text-left text-xs font-medium tracking-wide transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-amber-400/40',
                waitingIsOver
                  ? 'bg-amber-400/10 text-amber-700 ring-1 ring-amber-400/30'
                  : 'text-amber-700/80 hover:bg-amber-400/10',
              )}
            >
              <ChevronDown
                className={cn(
                  'h-3.5 w-3.5 shrink-0 transition-transform [transition-duration:200ms] motion-reduce:transition-none',
                  waitingExpanded ? 'rotate-0' : '-rotate-90',
                )}
                aria-hidden
              />
              <span>{waitingCountLabel}</span>
            </button>
            <div
              id={waitingBodyId}
              aria-hidden={!waitingExpanded}
              inert={!waitingExpanded}
              className={cn(
                'grid transition-[grid-template-rows] [transition-duration:200ms] [transition-timing-function:ease-out] motion-reduce:transition-none',
                waitingExpanded ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]',
              )}
            >
              <div className="min-h-0 overflow-clip">
                <div className="pt-2">
                  <div
                    className={
                      waitingIsOver
                        ? 'min-h-10 rounded-lg bg-amber-400/10 px-1 py-1 ring-1 ring-amber-400/30 transition-colors'
                        : 'min-h-10 px-1 py-1 transition-colors'
                    }
                  >
                    <div className="space-y-2">
                      {waiting.map((entry) =>
                        isAgendaRowDraggable(entry) ? (
                          <SortableAgendaItemRow
                            key={entry.id}
                            item={entry}
                            disabled={!waitingExpanded}
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
                        ),
                      )}
                    </div>
                  </div>
                </div>
                <hr
                  className={cn(
                    'mx-1 mt-2 border-border/60 transition-opacity [transition-duration:200ms] motion-reduce:transition-none',
                    waitingExpanded ? 'opacity-100' : 'opacity-0',
                  )}
                />
              </div>
            </div>
          </section>
        </SortableContext>
      )}

      <SortableContext
        items={draggable.map((entry) => entry.id)}
        strategy={verticalListSortingStrategy}
      >
        <div
          ref={setNodeRef}
          className={
            isOver
              ? 'min-h-10 rounded-lg bg-primary/5 px-1 py-2 ring-1 ring-primary/20 transition-colors'
              : 'min-h-10 px-1 py-2 transition-colors'
          }
        >
          {rest.length === 0 ? (
            <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
              {waiting.length > 0 ? 'Nothing else planned' : 'Nothing planned'}
            </p>
          ) : (
            <div className="space-y-2">
              {rest.map((entry) =>
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
                ),
              )}
            </div>
          )}
        </div>
      </SortableContext>

      {completed.length > 0 && (
        <section className="space-y-2 px-1 pb-2">
          <h3 className="px-1 text-xs font-medium tracking-wide text-muted-foreground">
            Completed
          </h3>
          <div className="space-y-2">
            {completed.map((entry) => (
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
            ))}
          </div>
        </section>
      )}
    </section>
  );
}

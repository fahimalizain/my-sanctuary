import { useDndContext, useDroppable } from '@dnd-kit/core';
import {
  SortableContext,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { useMemo, type ReactNode } from 'react';
import { cn } from '@/lib/utils';
import type { TaskRecord } from '@/app/types';
import { agendaDateLabel } from '../home/agenda-helpers';
import {
  visiblePlannedPreviewIndex,
  visiblePlannedTaskIds,
} from './planned-board';
import type { PlannedBoardGroup } from './planned-board';
import { SortableTaskCard } from './TaskCard';

/** One date group inside the Board's Planned column (ADR 0014): a sticky day
 *  header, then a droppable body holding that day's `agenda_sort_order` pile.
 *  The WHOLE section (header + body + the padding that replaces the column's
 *  inter-group gap) is the droppable (`planned-group:<date>`), so a drop on a
 *  header or a gap still targets that day and can never fall through to the
 *  column body (which means Undated).
 *
 *  While a drag hovers the group, the page paints the dragged card as a dashed
 *  placeholder at `previewIndex` (clamped to the visible pile) instead of
 *  splicing it into the flat Planned preview — the grouped render owns the
 *  visible order. The lifted card itself is hidden here (its committed group
 *  stays mounted, so even a single-card day keeps its droppable body); an
 *  empty visible pile keeps a minimum height and still accepts a drop. */
export function PlannedGroupSection({
  group,
  tasks,
  today,
  previewIndex,
  hiddenTaskId,
  movingIds,
  onEditTask,
  onToggleFocus,
  focusDisabled,
}: {
  group: PlannedBoardGroup;
  /** The group's displayed tasks, resolved from `group.taskIds`. */
  tasks: TaskRecord[];
  /** Server-computed civil today, for the day label. */
  today: string;
  /** The page's placeholder index when the dragover targets THIS group. */
  previewIndex: number | null;
  /** The card dnd-kit is lifting. Its committed group stays rendered so this
   *  body remains a drop target, but the card itself is hidden here. */
  hiddenTaskId: string | null;
  movingIds: Set<string>;
  onEditTask: (task: TaskRecord) => void;
  onToggleFocus?: (task: TaskRecord) => void;
  focusDisabled?: boolean;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: group.id,
    data: { type: 'planned-group', groupId: group.id },
  });
  const visibleTaskIds = visiblePlannedTaskIds(group, hiddenTaskId);
  // `isOver` only wins when the group BODY is the collision; hovering one of
  // its cards must still light the group, like the flat column's ring.
  const { over } = useDndContext();
  const overThisGroup =
    isOver || (over != null && visibleTaskIds.includes(String(over.id)));

  const byId = useMemo(
    () => new Map(tasks.map((task) => [task.id, task])),
    [tasks],
  );
  // `previewIndex` is computed against the COMMITTED group and already names
  // the visible insertion slot (downward moves land after the hovered card;
  // removing the hidden mover shifts the card into the same index). Clamp it
  // to the visible pile.
  const placeholderIndex =
    visiblePlannedPreviewIndex(group, hiddenTaskId, previewIndex) ?? -1;

  const cards: ReactNode[] = [];
  visibleTaskIds.forEach((taskId, index) => {
    if (index === placeholderIndex) {
      cards.push(
        <div
          key="planned-group-placeholder"
          aria-hidden
          className="h-12 rounded-lg border border-dashed border-primary/50 bg-primary/5"
        />,
      );
    }
    const task = byId.get(taskId);
    if (task) {
      cards.push(
        <SortableTaskCard
          key={task.id}
          task={task}
          columnStatus="PLANNED"
          onEdit={onEditTask}
          onToggleFocus={onToggleFocus}
          focusDisabled={focusDisabled}
          disabled={movingIds.has(task.id)}
        />,
      );
    }
  });
  if (placeholderIndex === visibleTaskIds.length) {
    cards.push(
      <div
        key="planned-group-placeholder"
        aria-hidden
        className="h-12 rounded-lg border border-dashed border-primary/50 bg-primary/5"
      />,
    );
  }

  return (
    <section
      ref={setNodeRef}
      data-planned-group={group.key}
      className={cn(
        'rounded-lg pb-2 transition-colors',
        overThisGroup && 'bg-muted/40 ring-2 ring-primary/20',
      )}
    >
      <header className="sticky top-0 z-10 flex items-center justify-between gap-2 border-b border-border/60 bg-card/95 px-2 py-2 backdrop-blur">
        <h4 className="min-w-0 truncate font-heading text-xs font-semibold text-foreground">
          {group.undated ? 'Undated' : agendaDateLabel(group.date ?? '', today)}
        </h4>
        <span
          className="flex-shrink-0 rounded-full bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground"
          aria-label={`${visibleTaskIds.length} tasks`}
        >
          {visibleTaskIds.length}
        </span>
      </header>
      <div className="min-h-10 rounded-lg p-1">
        {group.undated && visibleTaskIds.length === 0 && cards.length === 0 && (
          <p className="px-1 py-1.5 text-sm italic text-muted-foreground/70">
            Nothing undated — drop here
          </p>
        )}
        <SortableContext
          items={visibleTaskIds}
          strategy={verticalListSortingStrategy}
        >
          <div className="space-y-2">{cards}</div>
        </SortableContext>
      </div>
    </section>
  );
}

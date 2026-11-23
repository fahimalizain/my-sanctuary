import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { GripVertical, Pencil, Trash2 } from 'lucide-react';
import type { RoutineRecord } from '@/app/types';
import { cn } from '@/lib/utils';
import { markdownPreview } from './description-preview';
import {
  addCivilDays,
  civilToday,
  occurrenceDates,
  rruleSummary,
} from './rrule-preview';

// One sortable card on /routines. It is a component rather than an inline
// `.map` body because `useSortable` is a hook: one call per row, always at the
// top level of a component.
//
// The grip is the ONLY drag activator. `attributes`/`listeners` are spread on
// the grip button and nowhere else, so the edit and delete buttons stay plain
// click targets that can never start a drag.

interface SortableRoutineRowProps {
  routine: RoutineRecord;
  /** True while a reorder is in flight — no second drag may start. */
  disabled?: boolean;
  onEdit: (routine: RoutineRecord) => void;
  onDelete: (routine: RoutineRecord) => void;
}

export function SortableRoutineRow({
  routine,
  disabled = false,
  onEdit,
  onDelete,
}: SortableRoutineRowProps) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: routine.id, disabled });

  // The same 30-day expansion the list showed before the rows became sortable.
  const from = civilToday();
  const nextDates = occurrenceDates({
    rrule: routine.rrule,
    from,
    to: addCivilDays(from, 30),
  });
  const descriptionPreview = markdownPreview(routine.description);

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className="rounded-lg border border-border bg-background p-3"
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          aria-label={`Reorder ${routine.title}`}
          title="Reorder"
          disabled={disabled}
          className={cn(
            'flex-shrink-0 touch-none rounded-md p-1 text-muted-foreground/50 hover:bg-muted hover:text-muted-foreground',
            isDragging ? 'cursor-grabbing' : 'cursor-grab',
          )}
          {...(!disabled ? attributes : {})}
          {...(!disabled ? listeners : {})}
        >
          <GripVertical className="h-3.5 w-3.5 text-muted-foreground/50" />
        </button>
        <span
          className="h-2.5 w-2.5 rounded-full flex-shrink-0"
          style={{ backgroundColor: routine.category.color }}
        />
        <span className="flex-1 min-w-0 text-sm font-medium text-foreground truncate">
          {routine.title}
        </span>
        {/* The category pill carries the color; the name is hidden for the
            untracked sink */}
        {!routine.category.is_untracked && (
          <span className="flex-shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
            {routine.category.title}
          </span>
        )}
        <div className="flex flex-shrink-0 gap-1">
          <button
            type="button"
            onClick={() => onEdit(routine)}
            className="p-1.5 rounded-md hover:bg-muted transition-colors"
            aria-label={`Edit ${routine.title}`}
            title="Edit"
          >
            <Pencil className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
          <button
            type="button"
            onClick={() => onDelete(routine)}
            className="p-1.5 rounded-md hover:bg-destructive/10 hover:text-destructive transition-colors"
            aria-label={`Delete ${routine.title}`}
            title="Delete"
          >
            <Trash2 className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
        </div>
      </div>
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
        <span>{rruleSummary(routine.rrule)}</span>
        <span>{routine.estimated_minutes} min</span>
        {nextDates.length > 0 && (
          <span className="text-muted-foreground/80">
            next: {nextDates.slice(0, 3).join(' · ')}
          </span>
        )}
      </div>
      {descriptionPreview && (
        <p className="mt-1.5 line-clamp-2 text-xs text-muted-foreground/90">
          {descriptionPreview}
        </p>
      )}
    </div>
  );
}

import { cn } from '@/lib/utils';
import type { Task } from '../types';

interface TaskRowProps {
  task: Task;
  onClick?: (task: Task) => void;
  variant?: 'in-block' | 'standalone';
  startTime?: string;
  endTime?: string;
}

function PriorityDot({ priority }: { priority: Task['priority'] }) {
  const colors = {
    high: 'bg-priority-high',
    medium: 'bg-priority-medium',
    low: 'bg-priority-low',
  };
  return (
    <span
      className={cn('h-2 w-2 rounded-full flex-shrink-0', colors[priority])}
    />
  );
}

export function TaskRow({
  task,
  onClick,
  variant = 'standalone',
  startTime,
  endTime,
}: TaskRowProps) {
  const isInBlock = variant === 'in-block';

  return (
    <button
      onClick={() => onClick?.(task)}
      className={cn(
        'flex items-center gap-3 py-1.5 w-full text-left rounded-md transition-colors',
        onClick && 'hover:bg-primary-foreground/10 cursor-pointer',
      )}
    >
      <PriorityDot priority={task.priority} />
      <span
        className={cn(
          'flex-1 text-sm truncate',
          isInBlock
            ? task.completed
              ? 'text-primary-foreground/50 line-through'
              : 'text-primary-foreground'
            : task.completed
              ? 'text-foreground/50 line-through'
              : 'text-foreground',
        )}
      >
        {task.title}
      </span>
      <div
        className={cn(
          'flex items-center gap-2 text-xs',
          isInBlock ? 'text-primary-foreground/70' : 'text-muted-foreground',
        )}
      >
        {startTime && endTime && (
          <span className="tabular-nums">
            {startTime}–{endTime}
          </span>
        )}
        <span className="tabular-nums">{task.duration}m</span>
      </div>
    </button>
  );
}

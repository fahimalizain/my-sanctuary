import { MoreHorizontal, Clock } from 'lucide-react'
import { cn } from '@/lib/utils'
import type { Task, TimeBlock as TimeBlockType } from '../types'

interface TimeBlockProps {
  block: TimeBlockType
  className?: string
}

function PriorityDot({ priority }: { priority: Task['priority'] }) {
  const colors = {
    high: 'bg-red-500',
    medium: 'bg-amber-500',
    low: 'bg-blue-400',
  }
  return (
    <span
      className={cn(
        'h-2 w-2 rounded-full',
        colors[priority]
      )}
    />
  )
}

function TaskRow({ task }: { task: Task }) {
  return (
    <div className="flex items-center gap-3 py-1.5">
      <PriorityDot priority={task.priority} />
      <span className="flex-1 text-sm text-white/90 truncate">{task.title}</span>
      <span className="text-xs text-white/60">{task.duration}m</span>
    </div>
  )
}

export function TimeBlock({ block, className }: TimeBlockProps) {
  const hasMultipleTasks = block.tasks.length > 1
  const displayTasks = hasMultipleTasks ? block.tasks.slice(0, 6) : block.tasks
  const remainingCount = block.tasks.length - displayTasks.length

  return (
    <div
      className={cn('relative rounded-xl overflow-hidden', className)}
      style={{ backgroundColor: block.streamColor }}
    >
      {/* Header */}
      <div className="flex items-start justify-between p-4 pb-2">
        <div>
          <h3 className="font-heading text-lg font-semibold text-white">
            {block.streamName}
          </h3>
          <div className="flex items-center gap-2 text-sm text-white/70 mt-1">
            <Clock className="h-3.5 w-3.5" />
            <span>{block.startTime} - {block.endTime}</span>
          </div>
        </div>
        <button className="p-1 rounded-md hover:bg-white/10 transition-colors">
          <MoreHorizontal className="h-5 w-5 text-white/70" />
        </button>
      </div>

      {/* Tasks */}
      <div className={cn('px-4', hasMultipleTasks ? 'pb-3' : 'pb-4')}> 
        {hasMultipleTasks ? (
          <div className="bg-black/20 rounded-lg p-3 space-y-0.5">
            {displayTasks.map((task) => (
              <TaskRow key={task.id} task={task} />
            ))}
            {remainingCount > 0 && (
              <div className="flex items-center gap-3 py-1.5 text-sm text-white/60">
                <span className="h-2 w-2" />
                <span>... {remainingCount} more</span>
              </div>
            )}
          </div>
        ) : (
          <div className="flex items-center gap-3">
            <PriorityDot priority={block.tasks[0].priority} />
            <span className="text-sm text-white/90">{block.tasks[0].title}</span>
          </div>
        )}
      </div>
    </div>
  )
}

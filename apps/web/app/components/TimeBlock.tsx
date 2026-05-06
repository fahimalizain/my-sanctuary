import { MoreHorizontal, Clock } from 'lucide-react'
import { cn } from '@/lib/utils'
import { TaskRow } from './TaskRow'
import type { Task, TimeBlock as TimeBlockType } from '../types'

interface TimeBlockProps {
  block: TimeBlockType
  className?: string
  onTaskClick?: (task: Task) => void
}

// Parse time string to minutes since midnight
function parseTimeToMinutes(time: string): number {
  const [timePart, meridian] = time.split(' ')
  let [h, m] = timePart.split(':').map(Number)
  if (meridian === 'PM' && h !== 12) h += 12
  if (meridian === 'AM' && h === 12) h = 0
  return h * 60 + m
}

// Format minutes to time string
function formatMinutesToTime(minutes: number): string {
  let h = Math.floor(minutes / 60)
  const m = minutes % 60
  const meridian = h >= 12 ? 'PM' : 'AM'
  if (h === 0) h = 12
  if (h > 12) h -= 12
  return `${h}:${m.toString().padStart(2, '0')} ${meridian}`
}

// Calculate task times within a block
function calculateTaskTimes(block: TimeBlockType): Array<{ task: Task; startTime: string; endTime: string }> {
  const blockStart = parseTimeToMinutes(block.startTime)
  const blockEnd = parseTimeToMinutes(block.endTime)
  const totalDuration = blockEnd - blockStart
  const totalTaskDuration = block.tasks.reduce((sum, t) => sum + t.duration, 0)

  // Scale factor if tasks don't fill the entire block
  const scaleFactor = totalTaskDuration > 0 ? totalDuration / totalTaskDuration : 1

  let currentTime = blockStart

  return block.tasks.map((task) => {
    const taskDuration = Math.round(task.duration * scaleFactor)
    const startTime = formatMinutesToTime(currentTime)
    currentTime += taskDuration
    const endTime = formatMinutesToTime(currentTime)

    return { task, startTime, endTime }
  })
}

export function TimeBlock({ block, className, onTaskClick }: TimeBlockProps) {
  const hasMultipleTasks = block.tasks.length > 1
  const displayTasks = hasMultipleTasks ? block.tasks.slice(0, 6) : block.tasks
  const remainingCount = block.tasks.length - displayTasks.length

  // Calculate times for each task
  const taskTimes = calculateTaskTimes(block)
  const displayTaskTimes = hasMultipleTasks ? taskTimes.slice(0, 6) : taskTimes

  return (
    <div
      className={cn('relative rounded-xl overflow-hidden', className)}
      style={{ backgroundColor: block.streamColor }}
    >
      {/* Header */}
      <div className="flex items-start justify-between p-4 pb-2">
        <div>
          <h3 className="font-heading text-lg font-semibold text-primary-foreground">
            {block.streamName}
          </h3>
          <div className="flex items-center gap-2 text-sm text-primary-foreground/70 mt-1">
            <Clock className="h-3.5 w-3.5" />
            <span>{block.startTime} - {block.endTime}</span>
          </div>
        </div>
        <button className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors">
          <MoreHorizontal className="h-5 w-5 text-primary-foreground/70" />
        </button>
      </div>

      {/* Tasks */}
      <div className={cn('px-4', hasMultipleTasks ? 'pb-3' : 'pb-4')}>
        {hasMultipleTasks ? (
          <div className="bg-black/20 rounded-lg p-3 space-y-0.5">
            {displayTaskTimes.map(({ task, startTime, endTime }) => (
              <TaskRow 
                key={task.id} 
                task={task} 
                onClick={onTaskClick} 
                variant="in-block" 
                startTime={startTime}
                endTime={endTime}
              />
            ))}
            {remainingCount > 0 && (
              <div className="flex items-center gap-3 py-1.5 text-sm text-primary-foreground/60">
                <span className="h-2 w-2" />
                <span>... {remainingCount} more</span>
              </div>
            )}
          </div>
        ) : (
          <TaskRow 
            task={taskTimes[0].task} 
            onClick={onTaskClick} 
            variant="in-block"
            startTime={taskTimes[0].startTime}
            endTime={taskTimes[0].endTime}
          />
        )}
      </div>
    </div>
  )
}

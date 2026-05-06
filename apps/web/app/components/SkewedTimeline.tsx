import { TimeBlock } from './TimeBlock'
import type { TimeBlock as TimeBlockType } from '../types'
import { cn } from '@/lib/utils'

interface SkewedTimelineProps {
  blocks: TimeBlockType[]
  className?: string
}

function calculateBlockPosition(blocks: TimeBlockType[], index: number): number {
  let position = 0
  for (let i = 0; i < index; i++) {
    position += getBlockHeight(blocks[i])
  }
  return position + 16 // Add gap spacing
}

function getBlockHeight(block: TimeBlockType): number {
  // Base height for header + padding
  const baseHeight = 80
  
  if (block.tasks.length === 1) {
    // Single task block - compact
    return baseHeight + 24
  } else {
    // Multi-task block - expands based on task count
    const taskListHeight = Math.min(block.tasks.length, 6) * 28 + 24
    return baseHeight + taskListHeight + 16
  }
}

export function SkewedTimeline({ blocks, className }: SkewedTimelineProps) {
  // Calculate time labels based on block positions
  const timeLabels = [
    { label: '9 AM', blockIndex: 0 },
    { label: '10', blockIndex: 0, offset: 0.33 },
    { label: '11', blockIndex: 0, offset: 0.66 },
    { label: '12', blockIndex: 1 },
    { label: '2 PM', blockIndex: 2 },
    { label: '4 PM', blockIndex: 3 },
    { label: '6 PM', blockIndex: 3, afterBlock: true },
  ]

  return (
    <div className={cn('relative', className)}>
      {/* Time labels column */}
      <div className="absolute left-0 top-0 w-16">
        {timeLabels.map((timeLabel, index) => {
          let topPosition = calculateBlockPosition(blocks, timeLabel.blockIndex)
          
          if (timeLabel.offset) {
            // Offset within the first block
            const blockHeight = getBlockHeight(blocks[timeLabel.blockIndex])
            topPosition += blockHeight * timeLabel.offset
          }
          
          if (timeLabel.afterBlock) {
            // Position after the block
            topPosition += getBlockHeight(blocks[timeLabel.blockIndex])
          }

          return (
            <div
              key={index}
              className="absolute text-sm text-muted-foreground font-medium"
              style={{ top: `${topPosition}px` }}
            >
              {timeLabel.label}
            </div>
          )
        })}
      </div>

      {/* Blocks column */}
      <div className="ml-16 space-y-4">
        {blocks.map((block) => (
          <TimeBlock key={block.id} block={block} />
        ))}
      </div>
    </div>
  )
}

import { useState } from 'react'
import { Plus, MoreHorizontal, Clock, Calendar as CalendarIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { TaskModal } from '@/app/components/TaskModal'
import { streams, todayItems } from '@/app/mock-data'
import type { Task, TimeBlock } from '@/app/types'
import { cn } from '@/lib/utils'

// Filter for TimeBlocks only (standalone tasks don't belong to streams)
const timeBlocks = todayItems.filter((item): item is TimeBlock => 'tasks' in item)

interface StreamCardProps {
  stream: typeof streams[0]
  tasks: Task[]
  onAddTask: (streamId: string) => void
  onEditTask: (task: Task) => void
}

function StreamCard({ stream, tasks, onAddTask, onEditTask }: StreamCardProps) {
  return (
    <div 
      className="rounded-xl overflow-hidden"
      style={{ backgroundColor: stream.color }}
    >
      <div className="p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-heading text-lg font-semibold text-primary-foreground">
            {stream.name}
          </h3>
          <button className="p-1 rounded-md hover:bg-primary-foreground/10 transition-colors">
            <MoreHorizontal className="h-5 w-5 text-primary-foreground/70" />
          </button>
        </div>
        
        <div className="text-sm text-primary-foreground/70 mb-3">
          {tasks.length} tasks
        </div>

        {/* Add Task Button */}
        <button
          onClick={() => onAddTask(stream.id)}
          className="flex items-center gap-2 text-sm text-primary-foreground/80 hover:text-primary-foreground bg-primary-foreground/10 hover:bg-primary-foreground/20 rounded-lg px-3 py-2 transition-all mb-3"
        >
          <Plus className="h-4 w-4" />
          Add Task
        </button>
      </div>
      
      <div className="bg-black/20 p-3 space-y-2">
        {tasks.slice(0, 4).map((task) => (
          <button
            key={task.id}
            onClick={() => onEditTask(task)}
            className="flex items-center gap-3 text-sm w-full text-left hover:bg-primary-foreground/5 rounded-lg p-1 -mx-1 transition-colors"
          >
            <span className={cn(
              'h-2 w-2 rounded-full flex-shrink-0',
              task.priority === 'high' && 'bg-priority-high',
              task.priority === 'medium' && 'bg-priority-medium',
              task.priority === 'low' && 'bg-priority-low'
            )} />
            <span className={cn(
              "text-primary-foreground/90 flex-1 truncate",
              task.completed && "line-through text-primary-foreground/50"
            )}>
              {task.title}
            </span>
            <span className="text-primary-foreground/60 text-xs">{task.duration}m</span>
          </button>
        ))}
        {tasks.length > 4 && (
          <p className="text-xs text-primary-foreground/50 pl-5">
            +{tasks.length - 4} more tasks
          </p>
        )}
      </div>
    </div>
  )
}

export function StreamsPage() {
  const [modalOpen, setModalOpen] = useState(false)
  const [selectedTask, setSelectedTask] = useState<Task | undefined>()
  const [selectedStreamId, setSelectedStreamId] = useState<string | undefined>()

  // Distribute tasks among streams for mock data
  const allTasks = timeBlocks.flatMap(b => b.tasks)
  const [streamTasks, setStreamTasks] = useState<Record<string, Task[]>>({
    work: allTasks.slice(0, 6),
    gym: allTasks.slice(6, 8),
    family: allTasks.slice(8, 10),
    relax: allTasks.slice(10, 12),
  })

  const handleAddTask = (streamId: string) => {
    setSelectedTask(undefined)
    setSelectedStreamId(streamId)
    setModalOpen(true)
  }

  const handleEditTask = (task: Task) => {
    setSelectedTask(task)
    setSelectedStreamId(undefined)
    setModalOpen(true)
  }

  const handleSaveTask = (task: Partial<Task>) => {
    if (!task.id) return

    setStreamTasks(prev => {
      const newTasks = { ...prev }
      
      // If editing, remove from old location first
      if (selectedTask) {
        Object.keys(newTasks).forEach(key => {
          newTasks[key] = newTasks[key].filter(t => t.id !== task.id)
        })
      }

      // Add to selected stream or keep in original
      const targetStream = selectedStreamId || 'work'
      const taskData = task as Task
      
      if (selectedTask) {
        // Update existing
        newTasks[targetStream] = newTasks[targetStream].map(t => 
          t.id === task.id ? taskData : t
        )
      } else {
        // Add new
        newTasks[targetStream] = [...(newTasks[targetStream] || []), taskData]
      }

      return newTasks
    })
  }

  const handleDeleteTask = (taskId: string) => {
    setStreamTasks(prev => {
      const newTasks = { ...prev }
      Object.keys(newTasks).forEach(key => {
        newTasks[key] = newTasks[key].filter(t => t.id !== taskId)
      })
      return newTasks
    })
  }

  const totalTasks = Object.values(streamTasks).flat().length

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              My Streams
            </h1>
            <p className="text-muted-foreground">
              Manage your life domains and their tasks
            </p>
          </div>
          <Button className="bg-sanctuary-green hover:bg-sanctuary-green/90">
            <Plus className="h-4 w-4 mr-2" />
            New Stream
          </Button>
        </header>

        {/* Streams Grid */}
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-6">
          {streams.map((stream) => (
            <StreamCard 
              key={stream.id} 
              stream={stream} 
              tasks={streamTasks[stream.id] || []}
              onAddTask={handleAddTask}
              onEditTask={handleEditTask}
            />
          ))}
        </div>

        {/* Quick Stats */}
        <div className="mt-12 grid grid-cols-1 md:grid-cols-3 gap-6">
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-primary/10 rounded-lg">
                <Clock className="h-5 w-5 text-primary" />
              </div>
              <h3 className="font-heading font-semibold">Total Tasks</h3>
            </div>
            <p className="text-3xl font-bold text-foreground">{totalTasks}</p>
            <p className="text-sm text-muted-foreground mt-1">Across all streams</p>
          </div>
          
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-work-blue/10 rounded-lg">
                <CalendarIcon className="h-5 w-5 text-work-blue" />
              </div>
              <h3 className="font-heading font-semibold">Scheduled Today</h3>
            </div>
            <p className="text-3xl font-bold text-foreground">{timeBlocks.length}</p>
            <p className="text-sm text-muted-foreground mt-1">Time blocks</p>
          </div>
          
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-gym-terracotta/10 rounded-lg">
                <Clock className="h-5 w-5 text-gym-terracotta" />
              </div>
              <h3 className="font-heading font-semibold">Focus Hours</h3>
            </div>
            <p className="text-3xl font-bold text-foreground">6.5</p>
            <p className="text-sm text-muted-foreground mt-1">Hours tracked</p>
          </div>
        </div>
      </div>

      {/* Task Modal */}
      <TaskModal
        open={modalOpen}
        onOpenChange={setModalOpen}
        task={selectedTask}
        onSave={handleSaveTask}
        onDelete={handleDeleteTask}
      />
    </div>
  )
}

import { useState } from 'react'
import { Clock, Flag, Tag, CheckCircle2, Circle, AlertCircle } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { cn } from '@/lib/utils'
import type { Task } from '@/app/types'

type Priority = 'low' | 'medium' | 'high'

interface TaskModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  task?: Task
  onSave?: (task: Partial<Task>) => void
  onDelete?: (taskId: string) => void
}

const priorityConfig: Record<Priority, { label: string; icon: React.ReactNode }> = {
  low: {
    label: 'Low',
    icon: <Circle className="h-4 w-4" />,
  },
  medium: {
    label: 'Medium',
    icon: <AlertCircle className="h-4 w-4" />,
  },
  high: {
    label: 'High',
    icon: <AlertCircle className="h-4 w-4" />,
  },
}

export function TaskModal({ open, onOpenChange, task, onSave, onDelete }: TaskModalProps) {
  const isEditing = !!task
  const [title, setTitle] = useState(task?.title || '')
  const [description, setDescription] = useState('')
  const [priority, setPriority] = useState<Priority>(task?.priority || 'medium')
  const [duration, setDuration] = useState(task?.duration?.toString() || '30')
  const [completed, setCompleted] = useState(task?.completed || false)

  const handleSave = () => {
    onSave?.({
      id: task?.id || crypto.randomUUID(),
      title,
      priority,
      duration: parseInt(duration) || 30,
      completed,
    })
    onOpenChange(false)
  }

  const handleDelete = () => {
    if (task?.id) {
      onDelete?.(task.id)
      onOpenChange(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[480px] p-0 gap-0 overflow-hidden bg-card border-border">
        {/* Header accent bar */}
        <div className="h-2 bg-primary" />

        <div className="p-6">
          <DialogHeader className="mb-6">
            <DialogTitle className="text-foreground">
              {isEditing ? 'Edit Task' : 'New Task'}
            </DialogTitle>
            <DialogDescription>
              {isEditing
                ? 'Update the details of your task below.'
                : 'Create a new task to add to your stream.'}
            </DialogDescription>
          </DialogHeader>

          {/* Task Title */}
          <div className="space-y-2 mb-5">
            <label className="text-sm font-medium text-foreground">
              Task Name
            </label>
            <input
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="What needs to be done?"
              className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
            />
          </div>

          {/* Description */}
          <div className="space-y-2 mb-5">
            <label className="text-sm font-medium text-foreground">
              Description <span className="text-muted-foreground/60 font-normal">(optional)</span>
            </label>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Add more details about this task..."
              rows={3}
              className="w-full px-4 py-3 rounded-xl border border-input bg-background text-foreground placeholder:text-muted-foreground/60 focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all resize-none"
            />
          </div>

          {/* Priority Selection */}
          <div className="space-y-2 mb-5">
            <label className="text-sm font-medium text-foreground flex items-center gap-2">
              <Flag className="h-4 w-4 text-muted-foreground" />
              Priority
            </label>
            <div className="flex gap-2">
              {(Object.keys(priorityConfig) as Priority[]).map((p) => (
                <button
                  key={p}
                  onClick={() => setPriority(p)}
                  className={cn(
                    'flex items-center gap-2 px-4 py-2 rounded-xl border-2 text-sm font-medium transition-all',
                    priority === p
                      ? 'bg-primary/10 border-primary text-primary'
                      : 'bg-background border-input text-muted-foreground hover:border-primary/30'
                  )}
                >
                  {priorityConfig[p].icon}
                  {priorityConfig[p].label}
                </button>
              ))}
            </div>
          </div>

          {/* Duration and Status Row */}
          <div className="grid grid-cols-2 gap-4 mb-6">
            {/* Duration */}
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground flex items-center gap-2">
                <Clock className="h-4 w-4 text-muted-foreground" />
                Duration
              </label>
              <div className="relative">
                <input
                  type="number"
                  value={duration}
                  onChange={(e) => setDuration(e.target.value)}
                  min="5"
                  step="5"
                  className="w-full px-4 py-3 pr-12 rounded-xl border border-input bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-primary/20 focus:border-primary transition-all"
                />
                <span className="absolute right-4 top-1/2 -translate-y-1/2 text-sm text-muted-foreground">
                  min
                </span>
              </div>
            </div>

            {/* Status Toggle */}
            <div className="space-y-2">
              <label className="text-sm font-medium text-foreground">Status</label>
              <button
                onClick={() => setCompleted(!completed)}
                className={cn(
                  'w-full flex items-center gap-2 px-4 py-3 rounded-xl border-2 text-sm font-medium transition-all',
                  completed
                    ? 'bg-primary/10 border-primary text-primary'
                    : 'bg-background border-input text-muted-foreground hover:border-primary/30'
                )}
              >
                {completed ? (
                  <CheckCircle2 className="h-4 w-4" />
                ) : (
                  <Circle className="h-4 w-4" />
                )}
                {completed ? 'Completed' : 'Pending'}
              </button>
            </div>
          </div>

          {/* Stream Tag */}
          <div className="flex items-center gap-2 mb-6 p-3 bg-muted rounded-xl">
            <Tag className="h-4 w-4 text-primary" />
            <span className="text-sm text-foreground">Stream:</span>
            <span className="text-sm font-medium text-primary">Work</span>
          </div>

          {/* Actions */}
          <div className="flex items-center justify-between pt-4 border-t border-border">
            {isEditing ? (
              <Button
                variant="ghost"
                onClick={handleDelete}
                className="text-destructive hover:text-destructive hover:bg-destructive/10"
              >
                Delete
              </Button>
            ) : (
              <div />
            )}
            <div className="flex gap-3">
              <Button
                variant="outline"
                onClick={() => onOpenChange(false)}
                className="border-input text-foreground hover:bg-muted"
              >
                Cancel
              </Button>
              <Button
                onClick={handleSave}
                disabled={!title.trim()}
                className="bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
              >
                {isEditing ? 'Save Changes' : 'Create Task'}
              </Button>
            </div>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

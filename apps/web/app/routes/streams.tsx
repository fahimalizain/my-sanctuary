import { Plus, MoreHorizontal, Clock, Calendar as CalendarIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { streams, todayBlocks } from '@/app/mock-data'
import type { Task } from '@/app/types'

interface StreamCardProps {
  stream: typeof streams[0]
  tasks: Task[]
}

function StreamCard({ stream, tasks }: StreamCardProps) {
  return (
    <div 
      className="rounded-xl overflow-hidden"
      style={{ backgroundColor: stream.color }}
    >
      <div className="p-4">
        <div className="flex items-center justify-between mb-4">
          <h3 className="font-heading text-lg font-semibold text-white">
            {stream.name}
          </h3>
          <button className="p-1 rounded-md hover:bg-white/10 transition-colors">
            <MoreHorizontal className="h-5 w-5 text-white/70" />
          </button>
        </div>
        
        <div className="text-sm text-white/70 mb-3">
          {tasks.length} tasks
        </div>
      </div>
      
      <div className="bg-black/20 p-3 space-y-2">
        {tasks.slice(0, 4).map((task) => (
          <div key={task.id} className="flex items-center gap-3 text-sm">
            <span className={`
              h-2 w-2 rounded-full
              ${task.priority === 'high' ? 'bg-red-400' : 
                task.priority === 'medium' ? 'bg-amber-400' : 'bg-blue-400'}
            `} />
            <span className="text-white/90 flex-1 truncate">{task.title}</span>
            <span className="text-white/60 text-xs">{task.duration}m</span>
          </div>
        ))}
        {tasks.length > 4 && (
          <p className="text-xs text-white/50 pl-5">
            +{tasks.length - 4} more tasks
          </p>
        )}
      </div>
    </div>
  )
}

export function StreamsComponent() {
  // Distribute tasks among streams for mock data
  const allTasks = todayBlocks.flatMap(b => b.tasks)
  const streamTasks: Record<string, Task[]> = {
    work: allTasks.slice(0, 6),
    gym: allTasks.slice(6, 8),
    family: allTasks.slice(8, 10),
    relax: allTasks.slice(10, 12),
  }

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
            />
          ))}
        </div>

        {/* Quick Stats */}
        <div className="mt-12 grid grid-cols-1 md:grid-cols-3 gap-6">
          <div className="bg-white rounded-xl p-6 shadow-sm">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-sanctuary-green/10 rounded-lg">
                <Clock className="h-5 w-5 text-sanctuary-green" />
              </div>
              <h3 className="font-heading font-semibold">Total Tasks</h3>
            </div>
            <p className="text-3xl font-bold text-foreground">{allTasks.length}</p>
            <p className="text-sm text-muted-foreground mt-1">Across all streams</p>
          </div>
          
          <div className="bg-white rounded-xl p-6 shadow-sm">
            <div className="flex items-center gap-3 mb-4">
              <div className="p-2 bg-work-blue/10 rounded-lg">
                <CalendarIcon className="h-5 w-5 text-work-blue" />
              </div>
              <h3 className="font-heading font-semibold">Scheduled Today</h3>
            </div>
            <p className="text-3xl font-bold text-foreground">{todayBlocks.length}</p>
            <p className="text-sm text-muted-foreground mt-1">Time blocks</p>
          </div>
          
          <div className="bg-white rounded-xl p-6 shadow-sm">
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
    </div>
  )
}

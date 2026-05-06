export interface Task {
  id: string
  title: string
  duration: number
  priority: 'high' | 'medium' | 'low'
}

export interface TimeBlock {
  id: string
  streamId: string
  streamName: string
  streamColor: string
  startTime: string
  endTime: string
  tasks: Task[]
}

export interface Stream {
  id: string
  name: string
  color: string
}

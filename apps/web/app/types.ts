export interface Task {
  id: string;
  title: string;
  duration: number;
  priority: 'high' | 'medium' | 'low';
  completed?: boolean;
}

export interface TimeBlock {
  id: string;
  streamId: string;
  streamName: string;
  streamColor: string;
  startTime: string;
  endTime: string;
  tasks: Task[];
}

export interface Stream {
  id: string;
  name: string;
  color: string;
}

// A task event that appears directly on the timeline (not inside a time block)
export interface TaskEvent extends Task {
  startTime: string;
  endTime: string;
}

// Union type for timeline items - can be a full time block or a task event
export type TimelineItem = TimeBlock | TaskEvent;

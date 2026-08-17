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

// A Google Calendar event synced from the backend
export interface CalendarEvent {
  id: string;
  calendar_id: string;
  google_event_id: string;
  title: string;
  description: string;
  start_time: string; // ISO 8601
  end_time: string; // ISO 8601
  last_synced_at: string;
}

// The envelope returned by GET /api/calendar/events
export interface CalendarEventsResponse {
  events: CalendarEvent[];
  source: 'cache' | string;
}

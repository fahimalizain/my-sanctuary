import { ChevronLeft, ChevronRight, Clock } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { todayItems, streams } from '@/app/mock-data';
import type { TimeBlock } from '@/app/types';
import { cn } from '@/lib/utils';

const WEEK_DAYS = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];

// Filter for TimeBlocks only (standalone tasks don't have stream info for calendar view)
const timeBlocks = todayItems.filter(
  (item): item is TimeBlock => 'tasks' in item,
);

interface CalendarEventProps {
  block: TimeBlock;
  compact?: boolean;
}

function CalendarEvent({ block, compact }: CalendarEventProps) {
  return (
    <div
      className={cn(
        'rounded-lg p-2 text-primary-foreground',
        compact ? 'text-xs' : 'text-sm',
      )}
      style={{ backgroundColor: block.streamColor }}
    >
      <p className={cn('font-medium', compact && 'truncate')}>
        {block.streamName}
      </p>
      {!compact && (
        <p className="text-primary-foreground/70 text-xs mt-1">
          {block.startTime} - {block.endTime}
        </p>
      )}
    </div>
  );
}

export function CalendarPage() {
  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Header */}
        <header className="flex items-center justify-between mb-8">
          <div>
            <h1 className="font-heading text-3xl font-bold text-foreground mb-2">
              Calendar
            </h1>
            <p className="text-muted-foreground">May 2026</p>
          </div>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="icon">
              <ChevronLeft className="h-4 w-4" />
            </Button>
            <Button variant="outline">Today</Button>
            <Button variant="outline" size="icon">
              <ChevronRight className="h-4 w-4" />
            </Button>
          </div>
        </header>

        {/* Calendar Grid */}
        <div className="bg-card rounded-xl shadow-sm border border-border overflow-hidden">
          {/* Week Header */}
          <div className="grid grid-cols-7 border-b border-border">
            {WEEK_DAYS.map((day) => (
              <div key={day} className="p-4 text-center">
                <span className="text-sm font-medium text-muted-foreground">
                  {day}
                </span>
              </div>
            ))}
          </div>

          {/* Days Grid */}
          <div className="grid grid-cols-7 auto-rows-fr">
            {Array.from({ length: 35 }).map((_, index) => {
              const day = index - 4; // Offset for May 2026 starting on Friday
              const isCurrentMonth = day > 0 && day <= 31;
              const isToday = day === 6; // May 6th is today

              return (
                <div
                  key={index}
                  className={cn(
                    'min-h-[120px] p-2 border-b border-r border-border',
                    !isCurrentMonth && 'bg-muted/50',
                    isToday && 'bg-primary/5',
                  )}
                >
                  {isCurrentMonth && (
                    <>
                      <span
                        className={cn(
                          'inline-flex h-7 w-7 items-center justify-center rounded-full text-sm',
                          isToday
                            ? 'bg-primary text-primary-foreground font-semibold'
                            : 'text-foreground',
                        )}
                      >
                        {day}
                      </span>

                      {/* Mock events for some days */}
                      {day === 6 && (
                        <div className="mt-2 space-y-1">
                          {timeBlocks.slice(0, 2).map((block) => (
                            <CalendarEvent
                              key={block.id}
                              block={block}
                              compact
                            />
                          ))}
                        </div>
                      )}

                      {[3, 8, 10, 15, 20, 22, 25].includes(day) && (
                        <div className="mt-2">
                          <div
                            className="h-2 w-full rounded"
                            style={{
                              backgroundColor:
                                streams[
                                  Math.floor(Math.random() * streams.length)
                                ].color,
                            }}
                          />
                        </div>
                      )}
                    </>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* Upcoming Events */}
        <div className="mt-8 grid grid-cols-1 lg:grid-cols-2 gap-6">
          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <h2 className="font-heading text-lg font-semibold mb-4">
              Today&apos;s Schedule
            </h2>
            <div className="space-y-3">
              {timeBlocks.map((block) => (
                <div
                  key={block.id}
                  className="flex items-center gap-4 p-3 rounded-lg bg-muted"
                >
                  <div
                    className="h-10 w-10 rounded-lg flex items-center justify-center"
                    style={{ backgroundColor: block.streamColor }}
                  >
                    <Clock className="h-5 w-5 text-primary-foreground" />
                  </div>
                  <div className="flex-1">
                    <p className="font-medium text-foreground">
                      {block.streamName}
                    </p>
                    <p className="text-sm text-muted-foreground">
                      {block.startTime} - {block.endTime}
                    </p>
                  </div>
                  <span className="text-sm text-muted-foreground">
                    {block.tasks.length} tasks
                  </span>
                </div>
              ))}
            </div>
          </div>

          <div className="bg-card rounded-xl p-6 shadow-sm border border-border">
            <h2 className="font-heading text-lg font-semibold mb-4">
              This Week
            </h2>
            <div className="space-y-4">
              {['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday'].map(
                (day, i) => (
                  <div key={day} className="flex items-center gap-4">
                    <div className="w-16 text-sm font-medium text-muted-foreground">
                      {day.slice(0, 3)}
                    </div>
                    <div className="flex-1 flex gap-2">
                      {i < 3 && (
                        <>
                          <div className="h-8 flex-1 rounded bg-work-blue/20" />
                          <div className="h-8 flex-1 rounded bg-gym-terracotta/20" />
                        </>
                      )}
                      {i === 3 && (
                        <div className="h-8 flex-1 rounded bg-family-plum/20" />
                      )}
                    </div>
                  </div>
                ),
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

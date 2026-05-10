import {
  useRef,
  useLayoutEffect,
  useState,
  useMemo,
  useCallback,
  useEffect,
} from 'react';
import { TimeBlock } from './TimeBlock';
import { TaskRow } from './TaskRow';
import { TaskMiniModal } from './TaskMiniModal';
import type {
  Task,
  TimeBlock as TimeBlockType,
  TaskEvent,
  TimelineItem,
} from '../types';
import { cn } from '@/lib/utils';

interface SkewedTimelineProps {
  items: TimelineItem[];
  className?: string;
  onItemsChange?: (items: TimelineItem[]) => void;
}

const MIN_HOUR_HEIGHT = 40;
const BLOCK_GAP = 20;

function parseHour(time: string): number {
  const [timePart, meridian] = time.split(' ');
  let [h, m] = timePart.split(':').map(Number);
  if (meridian === 'PM' && h !== 12) h += 12;
  if (meridian === 'AM' && h === 12) h = 0;
  return h + m / 60;
}

function formatHour(hour: number): string {
  if (hour === 0) return '12 AM';
  if (hour < 12) return `${hour} AM`;
  if (hour === 12) return '12 PM';
  return `${hour - 12} PM`;
}

function isTimeBlock(item: TimelineItem): item is TimeBlockType {
  return 'tasks' in item && Array.isArray((item as TimeBlockType).tasks);
}

interface HourInfo {
  hour: number;
  top: number;
  isInBlock: boolean;
}

interface ItemPosition {
  item: TimelineItem;
  top: number;
  height: number;
  left: number; // percentage
  width: number; // percentage
  isStandalone: boolean;
}

interface UseSkewedTimelineReturn {
  measured: boolean;
  measureRef: React.RefObject<HTMLDivElement | null>;
  uniqueHours: HourInfo[];
  itemPositions: ItemPosition[];
  totalHeight: number;
}

// Group items by time range for horizontal layout
function groupItemsByTimeRange(items: TimelineItem[]): TimelineItem[][] {
  const groups = new Map<string, TimelineItem[]>();
  const orderKeys: string[] = [];

  for (const item of items) {
    const key = `${item.startTime}-${item.endTime}`;
    if (!groups.has(key)) {
      groups.set(key, []);
      orderKeys.push(key);
    }
    groups.get(key)!.push(item);
  }

  return orderKeys.map((key) => groups.get(key)!);
}

function useSkewedTimeline(items: TimelineItem[]): UseSkewedTimelineReturn {
  const measureRef = useRef<HTMLDivElement>(null);
  const [measured, setMeasured] = useState(false);
  const [naturalHeights, setNaturalHeights] = useState<Map<string, number>>(
    new Map(),
  );
  const heightChecksum = useRef(0);

  const measureHeights = useCallback(() => {
    if (!measureRef.current) return;

    const heights = new Map<string, number>();
    const elements = measureRef.current.querySelectorAll('[data-item-id]');

    let checksum = 0;
    elements.forEach((el) => {
      const id = el.getAttribute('data-item-id');
      if (id) {
        const height = el.getBoundingClientRect().height;
        heights.set(id, height);
        checksum += Math.round(height * 100);
      }
    });

    if (checksum !== heightChecksum.current) {
      heightChecksum.current = checksum;
      setNaturalHeights(new Map(heights));
      setMeasured(true);
    }
  }, [items]);

  useLayoutEffect(() => {
    measureHeights();
  }, [measureHeights]);

  useEffect(() => {
    if (!measureRef.current || typeof ResizeObserver === 'undefined') return;

    const observer = new ResizeObserver(() => measureHeights());
    const elements = measureRef.current.querySelectorAll('[data-item-id]');
    elements.forEach((el) => observer.observe(el));

    return () => observer.disconnect();
  }, [measureHeights]);

  const { uniqueHours, itemPositions, totalHeight } = useMemo(() => {
    const hours: HourInfo[] = [];
    const positions: ItemPosition[] = [];

    if (!measured || naturalHeights.size === 0) {
      return {
        uniqueHours: hours,
        itemPositions: positions,
        totalHeight: 24 * MIN_HOUR_HEIGHT,
      };
    }

    // Group items by time range
    const rows = groupItemsByTimeRange(items);

    let currentY = 16;

    // Before first row
    const firstRow = rows[0];
    if (firstRow && firstRow[0]) {
      const firstStart = parseHour(firstRow[0].startTime);
      if (firstStart > 0) {
        const gapHours = firstStart;
        const minHeight = gapHours * MIN_HOUR_HEIGHT;
        const hourHeight = minHeight / gapHours;

        for (let h = 0; h < gapHours; h++) {
          hours.push({
            hour: h,
            top: currentY + h * hourHeight + hourHeight / 2,
            isInBlock: false,
          });
        }
        currentY += minHeight + BLOCK_GAP;
      }
    }

    // Process each row (items sharing the same time range)
    for (let rowIndex = 0; rowIndex < rows.length; rowIndex++) {
      const row = rows[rowIndex];
      const firstItem = row[0];
      const itemCount = row.length;

      const startHour = Math.floor(parseHour(firstItem.startTime));
      const endHour = Math.ceil(parseHour(firstItem.endTime));
      const duration = endHour - startHour;

      // Calculate row height (max of all items in this row)
      const rowHeight = Math.max(
        ...row.map(
          (item) =>
            naturalHeights.get(item.id) || (isTimeBlock(item) ? 100 : 40),
        ),
      );

      // Hours within this time range
      for (let h = Math.ceil(startHour); h < Math.floor(endHour); h++) {
        const progress = (h - startHour) / duration;
        hours.push({
          hour: h,
          top: currentY + rowHeight * progress,
          isInBlock: true,
        });
      }

      // Position each item in the row (split horizontally)
      const itemWidth = 100 / itemCount;

      row.forEach((item, index) => {
        const naturalHeight =
          naturalHeights.get(item.id) || (isTimeBlock(item) ? 100 : 40);

        positions.push({
          item,
          top: currentY,
          height: naturalHeight, // Each item keeps its natural height within the row
          left: index * itemWidth,
          width: itemWidth,
          isStandalone: !isTimeBlock(item),
        });
      });

      currentY += rowHeight;

      // Gap after this row
      const nextRow = rows[rowIndex + 1];
      if (nextRow && nextRow[0]) {
        const currentEnd = endHour;
        const nextStart = Math.floor(parseHour(nextRow[0].startTime));
        const gapHours = nextStart - currentEnd;

        if (gapHours === 0) {
          hours.push({
            hour: currentEnd,
            top: currentY + BLOCK_GAP / 2,
            isInBlock: false,
          });
          currentY += BLOCK_GAP;
        } else if (gapHours > 0) {
          const minHeight = gapHours * MIN_HOUR_HEIGHT;
          const hourHeight = minHeight / gapHours;

          for (let h = 0; h < gapHours; h++) {
            hours.push({
              hour: currentEnd + h,
              top: currentY + h * hourHeight + hourHeight / 2,
              isInBlock: false,
            });
          }
          currentY += minHeight + BLOCK_GAP;
        }
      }
    }

    // After last row
    if (rows.length > 0) {
      const lastRow = rows[rows.length - 1];
      const lastItem = lastRow[lastRow.length - 1];
      const lastEnd = Math.ceil(parseHour(lastItem.endTime));
      const gapHours = 24 - lastEnd;

      if (gapHours > 0) {
        const minHeight = Math.max(gapHours * MIN_HOUR_HEIGHT, 80);
        const hourHeight = minHeight / gapHours;

        for (let h = 0; h < gapHours; h++) {
          hours.push({
            hour: lastEnd + h,
            top: currentY + h * hourHeight + hourHeight / 2,
            isInBlock: false,
          });
        }
      }
    }

    // Deduplicate hours
    const uniqueHours = hours.filter(
      (h, index, self) => index === self.findIndex((t) => t.hour === h.hour),
    );

    const totalHeight =
      uniqueHours.length > 0
        ? Math.max(...uniqueHours.map((h) => h.top)) + MIN_HOUR_HEIGHT / 2 + 16
        : 24 * MIN_HOUR_HEIGHT;

    return { uniqueHours, itemPositions: positions, totalHeight };
  }, [measured, naturalHeights, items]);

  return {
    measured,
    measureRef,
    uniqueHours,
    itemPositions,
    totalHeight,
  };
}

function itemToTask(item: TimelineItem): Task {
  if (isTimeBlock(item)) {
    return (
      item.tasks[0] || {
        id: item.id,
        title: item.streamName,
        duration: 30,
        priority: 'medium',
      }
    );
  }
  return item as TaskEvent;
}

export function SkewedTimeline({
  items,
  className,
  onItemsChange,
}: SkewedTimelineProps) {
  const { measured, measureRef, uniqueHours, itemPositions, totalHeight } =
    useSkewedTimeline(items);
  const [selectedItem, setSelectedItem] = useState<TimelineItem | null>(null);
  const [anchorRect, setAnchorRect] = useState<DOMRect | null>(null);
  const itemRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const handleItemClick = (item: TimelineItem) => {
    const element = itemRefs.current.get(item.id);
    if (element) {
      setAnchorRect(element.getBoundingClientRect());
      setSelectedItem(item);
    }
  };

  const handleCloseModal = () => {
    setSelectedItem(null);
    setAnchorRect(null);
  };

  const handleSaveItem = (updates: {
    title: string;
    startTime: string;
    endTime: string;
  }) => {
    if (!selectedItem || !onItemsChange) return;

    const updatedItems = items.map((item) => {
      if (item.id !== selectedItem.id) return item;

      if (isTimeBlock(item)) {
        // Update time block
        return {
          ...item,
          streamName: updates.title,
          startTime: updates.startTime,
          endTime: updates.endTime,
        };
      } else {
        // Update task event
        return {
          ...item,
          title: updates.title,
          startTime: updates.startTime,
          endTime: updates.endTime,
        };
      }
    });

    onItemsChange(updatedItems);
    handleCloseModal();
  };

  const setItemRef = (id: string) => (el: HTMLDivElement | null) => {
    if (el) itemRefs.current.set(id, el);
    else itemRefs.current.delete(id);
  };

  return (
    <div className={cn('relative', className)}>
      {/* Hidden measurement container */}
      {!measured && (
        <div
          ref={measureRef}
          className="absolute opacity-0 pointer-events-none left-0 top-0 w-64"
        >
          <div className="space-y-4">
            {items.map((item) => (
              <div key={`measure-${item.id}`} data-item-id={item.id}>
                {isTimeBlock(item) ? (
                  <TimeBlock block={item} />
                ) : (
                  <div className="bg-muted rounded-lg p-3">
                    <TaskRow
                      task={itemToTask(item)}
                      variant="standalone"
                      startTime={item.startTime}
                      endTime={item.endTime}
                    />
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Task Mini Modal */}
      {selectedItem && anchorRect && (
        <TaskMiniModal
          item={selectedItem}
          anchorRect={anchorRect}
          onClose={handleCloseModal}
          onSave={handleSaveItem}
        />
      )}

      {/* Rendered timeline */}
      {measured && (
        <>
          {/* Hour grid lines */}
          <div className="absolute inset-0 pointer-events-none">
            {uniqueHours.map((h) => (
              <div
                key={`grid-${h.hour}`}
                className="absolute left-12 right-0 border-t border-border/20"
                style={{ top: `${h.top}px` }}
              />
            ))}
          </div>

          {/* Hour labels */}
          <div className="absolute left-0 top-0 w-16">
            {uniqueHours.map((h) => (
              <div
                key={`label-${h.hour}`}
                className="absolute right-0 text-sm font-medium -translate-y-1/2 text-muted-foreground/60"
                style={{ top: `${h.top}px` }}
              >
                {formatHour(h.hour)}
              </div>
            ))}
          </div>

          {/* Positioned items - horizontally split when same time range */}
          <div className="absolute left-20 right-0 top-0">
            {itemPositions.map((pos) => (
              <div
                key={pos.item.id}
                ref={setItemRef(pos.item.id)}
                className="absolute px-1 cursor-pointer hover:opacity-90 transition-opacity"
                style={{
                  top: `${pos.top}px`,
                  left: `${pos.left}%`,
                  width: `${pos.width}%`,
                  minHeight: `${pos.height}px`,
                }}
                onClick={() => handleItemClick(pos.item)}
              >
                {pos.isStandalone ? (
                  <div className="bg-muted rounded-lg p-2 h-full">
                    <TaskRow
                      task={itemToTask(pos.item)}
                      variant="standalone"
                      startTime={pos.item.startTime}
                      endTime={pos.item.endTime}
                    />
                  </div>
                ) : (
                  <TimeBlock block={pos.item as TimeBlockType} />
                )}
              </div>
            ))}
          </div>

          {/* Spacer for height */}
          <div style={{ height: `${totalHeight}px` }} />
        </>
      )}

      {/* Fallback while measuring */}
      {!measured && <div style={{ height: `${24 * MIN_HOUR_HEIGHT}px` }} />}
    </div>
  );
}

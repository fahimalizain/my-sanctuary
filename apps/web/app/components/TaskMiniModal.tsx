import { useState, useRef, useEffect } from 'react';
import { X } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { TimelineItem } from '../types';

interface TaskMiniModalProps {
  item: TimelineItem;
  anchorRect: DOMRect;
  onClose: () => void;
  onSave: (updates: {
    title: string;
    startTime: string;
    endTime: string;
  }) => void;
}

// Parse time string to minutes since midnight
function parseTimeToMinutes(time: string): number {
  const [timePart, meridian] = time.split(' ');
  let [h, m] = timePart.split(':').map(Number);
  if (meridian === 'PM' && h !== 12) h += 12;
  if (meridian === 'AM' && h === 12) h = 0;
  return h * 60 + m;
}

// Format minutes to time string
function formatMinutesToTime(minutes: number): string {
  let h = Math.floor(minutes / 60);
  const m = minutes % 60;
  const meridian = h >= 12 ? 'PM' : 'AM';
  if (h === 0) h = 12;
  if (h > 12) h -= 12;
  return `${h}:${m.toString().padStart(2, '0')} ${meridian}`;
}

// Format for display in input (HH:MM)
function formatForInput(minutes: number): string {
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return `${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}`;
}

// Parse input (HH:MM) to minutes
function parseInputToMinutes(input: string): number | null {
  const [h, m] = input.split(':').map(Number);
  if (isNaN(h) || isNaN(m)) return null;
  return h * 60 + m;
}

interface TimeInputProps {
  value: string;
  onChange: (value: string) => void;
  label: string;
}

function TimeInput({ value, onChange, label }: TimeInputProps) {
  const minutes = parseTimeToMinutes(value);
  const [inputValue, setInputValue] = useState(formatForInput(minutes));
  const inputRef = useRef<HTMLInputElement>(null);

  // Sync external value changes
  useEffect(() => {
    setInputValue(formatForInput(parseTimeToMinutes(value)));
  }, [value]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault();
      const currentMinutes = parseInputToMinutes(inputValue);
      if (currentMinutes === null) return;

      const delta = e.key === 'ArrowUp' ? -5 : 5;
      const newMinutes = currentMinutes + delta;

      // Wrap around 24 hours
      const wrappedMinutes = ((newMinutes % 1440) + 1440) % 1440;

      const newInputValue = formatForInput(wrappedMinutes);
      setInputValue(newInputValue);
      onChange(formatMinutesToTime(wrappedMinutes));
    }
  };

  const handleBlur = () => {
    const parsed = parseInputToMinutes(inputValue);
    if (parsed !== null) {
      const wrapped = ((parsed % 1440) + 1440) % 1440;
      const newValue = formatMinutesToTime(wrapped);
      setInputValue(formatForInput(wrapped));
      onChange(newValue);
    } else {
      // Reset to current value if invalid
      setInputValue(formatForInput(parseTimeToMinutes(value)));
    }
  };

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setInputValue(e.target.value);
  };

  return (
    <div className="space-y-1">
      <label className="text-xs font-medium text-muted-foreground">
        {label}
      </label>
      <div className="flex items-center gap-2">
        <input
          ref={inputRef}
          type="text"
          value={inputValue}
          onChange={handleChange}
          onKeyDown={handleKeyDown}
          onBlur={handleBlur}
          placeholder="HH:MM"
          className={cn(
            'w-20 px-2 py-1.5 text-sm rounded-md border bg-background',
            'focus:outline-none focus:ring-2 focus:ring-ring focus:border-ring',
            'text-center font-mono',
          )}
        />
        <span className="text-xs text-muted-foreground">
          {formatMinutesToTime(parseInputToMinutes(inputValue) || 0)}
        </span>
      </div>
      <p className="text-[10px] text-muted-foreground">↑↓ 5min</p>
    </div>
  );
}

export function TaskMiniModal({
  item,
  anchorRect,
  onClose,
  onSave,
}: TaskMiniModalProps) {
  const isBlock = 'tasks' in item;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const [title, setTitle] = useState(
    isBlock ? item.streamName : (item as unknown as { title: string }).title,
  );
  const [startTime, setStartTime] = useState(item.startTime);
  const [endTime, setEndTime] = useState(item.endTime);

  const modalRef = useRef<HTMLDivElement>(null);

  // Close on click outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (modalRef.current && !modalRef.current.contains(e.target as Node)) {
        onClose();
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [onClose]);

  // Close on escape key
  useEffect(() => {
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [onClose]);

  const handleSave = () => {
    onSave({ title, startTime, endTime });
    onClose();
  };

  // Calculate position - anchor to the right of the task
  const modalStyle: React.CSSProperties = {
    position: 'fixed',
    top: Math.max(16, anchorRect.top + anchorRect.height / 2 - 100),
    left: Math.min(window.innerWidth - 320, anchorRect.right + 16),
    width: '280px',
    zIndex: 50,
  };

  return (
    <div
      ref={modalRef}
      className="bg-card rounded-xl shadow-lg border border-border p-4"
      style={modalStyle}
    >
      {/* Header */}
      <div className="flex items-center justify-between mb-4">
        <h3 className="font-heading text-sm font-semibold">
          {isBlock ? 'Edit Time Block' : 'Edit Task'}
        </h3>
        <button
          onClick={onClose}
          className="p-1 rounded-md hover:bg-muted transition-colors"
        >
          <X className="h-4 w-4" />
        </button>
      </div>

      {/* Title */}
      <div className="space-y-1 mb-4">
        <label className="text-xs font-medium text-muted-foreground">
          Title
        </label>
        <input
          type="text"
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          className={cn(
            'w-full px-3 py-2 text-sm rounded-md border bg-background',
            'focus:outline-none focus:ring-2 focus:ring-ring focus:border-ring',
          )}
        />
      </div>

      {/* Time inputs */}
      <div className="grid grid-cols-2 gap-3 mb-4">
        <TimeInput label="Start" value={startTime} onChange={setStartTime} />
        <TimeInput label="End" value={endTime} onChange={setEndTime} />
      </div>

      {/* Actions */}
      <div className="flex justify-end gap-2">
        <button
          onClick={onClose}
          className="px-3 py-1.5 text-sm rounded-md hover:bg-muted transition-colors"
        >
          Cancel
        </button>
        <button
          onClick={handleSave}
          className="px-3 py-1.5 text-sm bg-primary text-primary-foreground rounded-md hover:bg-primary/90 transition-colors"
        >
          Save
        </button>
      </div>
    </div>
  );
}

import { ChevronLeft, ChevronRight, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { ViewSelector } from './ViewSelector';

export interface CalendarHeaderProps {
  rangeTitle: string;
  isRefreshing: boolean;
  periodLength: number;
  onPeriodChange: (n: number) => void;
  onToday: () => void;
  onPrevPeriod: () => void;
  onNextPeriod: () => void;
}

export function CalendarHeader({
  rangeTitle,
  isRefreshing,
  periodLength,
  onPeriodChange,
  onToday,
  onPrevPeriod,
  onNextPeriod,
}: CalendarHeaderProps) {
  return (
    <header className="h-12 shrink-0 flex items-center gap-2 px-3 sm:px-4 border-b border-border/60">
      <h1 className="font-heading text-base sm:text-lg font-semibold text-foreground truncate min-w-0 flex-1">
        {rangeTitle}
      </h1>

      {isRefreshing && (
        <Loader2
          className="h-4 w-4 animate-spin text-muted-foreground shrink-0"
          aria-label="Refreshing events"
        />
      )}

      <ViewSelector periodLength={periodLength} onChange={onPeriodChange} />

      <Button variant="outline" size="sm" onClick={onToday}>
        Today
      </Button>
      <Button
        variant="outline"
        size="icon"
        className="h-8 w-8"
        onClick={onPrevPeriod}
        aria-label="Previous period"
      >
        <ChevronLeft className="h-4 w-4" />
      </Button>
      <Button
        variant="outline"
        size="icon"
        className="h-8 w-8"
        onClick={onNextPeriod}
        aria-label="Next period"
      >
        <ChevronRight className="h-4 w-4" />
      </Button>
    </header>
  );
}

import { useState } from 'react';
import { Check, ChevronDown } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover';
import { cn } from '@/lib/utils';
import { periodLabel } from './week-layout';

const GROUP_1: { n: number; label: string; shortcut: string }[] = [
  { n: 1, label: 'Day', shortcut: '1' },
  { n: 7, label: 'Week', shortcut: 'W' },
];

const GROUP_2: { n: number; label: string; shortcut: string }[] = [2, 3, 4, 5, 6].map(
  (n) => ({
    n,
    label: periodLabel(n),
    shortcut: String(n),
  }),
);

export function ViewSelector({
  periodLength,
  onChange,
}: {
  periodLength: number;
  onChange: (n: number) => void;
}) {
  const [open, setOpen] = useState(false);

  const pick = (n: number) => {
    onChange(n);
    setOpen(false);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          aria-label="Number of displayed days"
          className="gap-1"
        >
          {periodLabel(periodLength)}
          <ChevronDown className="h-3.5 w-3.5 opacity-60" aria-hidden />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-44 p-1" align="start">
        <MenuGroup items={GROUP_1} periodLength={periodLength} onPick={pick} />
        <div className="h-px bg-border my-1" role="separator" />
        <MenuGroup items={GROUP_2} periodLength={periodLength} onPick={pick} />
      </PopoverContent>
    </Popover>
  );
}

function MenuGroup({
  items,
  periodLength,
  onPick,
}: {
  items: { n: number; label: string; shortcut: string }[];
  periodLength: number;
  onPick: (n: number) => void;
}) {
  return (
    <div role="group">
      {items.map((item) => {
        const selected = periodLength === item.n;
        return (
          <button
            key={item.n}
            type="button"
            onClick={() => onPick(item.n)}
            className={cn(
              'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground',
              'hover:bg-muted transition-colors',
              selected && 'font-medium',
            )}
            aria-checked={selected}
            role="menuitemradio"
          >
            <span className="flex h-3.5 w-3.5 shrink-0 items-center justify-center">
              {selected ? (
                <Check className="h-3.5 w-3.5" aria-hidden />
              ) : null}
            </span>
            <span className="flex-1 min-w-0 text-left truncate">{item.label}</span>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
              {item.shortcut}
            </span>
          </button>
        );
      })}
    </div>
  );
}

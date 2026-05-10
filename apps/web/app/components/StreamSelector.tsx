import { ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';

interface StreamSelectorProps {
  className?: string;
}

export function StreamSelector({ className }: StreamSelectorProps) {
  return (
    <button
      className={cn(
        'flex items-center gap-2 px-4 py-2 text-sm font-medium text-foreground/80 hover:text-foreground transition-colors',
        className,
      )}
    >
      <span>My Streams</span>
      <ChevronDown className="h-4 w-4" />
    </button>
  );
}

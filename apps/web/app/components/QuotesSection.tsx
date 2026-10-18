import { useState, useEffect } from 'react';
import { ChevronLeft, ChevronRight, Quote } from 'lucide-react';
import { cn } from '@/lib/utils';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';

/** Auto-advance cadence for the Daily Inspiration strip. */
export const QUOTE_ADVANCE_MS = 60_000;

interface QuotesSectionProps {
  quotes: { text: string; author: string }[];
  className?: string;
}

export function QuotesSection({ quotes, className }: QuotesSectionProps) {
  const [currentIndex, setCurrentIndex] = useState(0);
  const [isReading, setIsReading] = useState(false);

  useEffect(() => {
    // Pause while the full quote is open, then restart from a full interval.
    if (isReading) return;

    const interval = setInterval(() => {
      setCurrentIndex((prev) => (prev + 1) % quotes.length);
    }, QUOTE_ADVANCE_MS);

    return () => clearInterval(interval);
  }, [quotes.length, isReading]);

  const goToPrev = () => {
    setCurrentIndex((prev) => (prev - 1 + quotes.length) % quotes.length);
  };

  const goToNext = () => {
    setCurrentIndex((prev) => (prev + 1) % quotes.length);
  };

  const currentQuote = quotes[currentIndex];

  // Selecting the visible quote means the reader is copying it, not asking for
  // the full text.
  const openFullQuote = () => {
    if (window.getSelection?.()?.toString()) return;
    setIsReading(true);
  };

  return (
    <>
      <div
        className={cn(
          'flex h-14 items-center gap-2 rounded-xl border border-border bg-card px-3 shadow-sm',
          'lg:block lg:h-auto lg:p-6',
          className,
        )}
      >
        <div className="flex shrink-0 items-center gap-2 lg:mb-4">
          <Quote className="h-4 w-4 text-primary lg:h-5 lg:w-5" />
          <h2 className="hidden font-heading text-lg font-semibold text-foreground lg:block">
            Daily Inspiration
          </h2>
        </div>

        <button
          type="button"
          onClick={openFullQuote}
          aria-label="Read full quote"
          aria-haspopup="dialog"
          aria-expanded={isReading}
          className={cn(
            'min-w-0 min-h-0 flex-1 cursor-pointer bg-transparent p-0 text-left font-inherit',
            'lg:flex lg:min-h-[100px] lg:flex-col lg:justify-center',
          )}
        >
          <p className="selectable line-clamp-2 text-xs leading-snug text-foreground/90 lg:mb-3 lg:line-clamp-none lg:text-lg lg:leading-relaxed">
            &ldquo;{currentQuote.text}&rdquo;
          </p>
          <p className="selectable mt-0.5 line-clamp-1 text-[10px] text-muted-foreground lg:mt-0 lg:line-clamp-none lg:text-sm">
            — {currentQuote.author}
          </p>
        </button>

        <button
          type="button"
          onClick={goToNext}
          aria-label="Next quote"
          className="shrink-0 rounded-md p-1 text-muted-foreground transition-colors hover:bg-muted lg:hidden"
        >
          <ChevronRight className="h-4 w-4" />
        </button>

        <div className="mt-4 flex items-center justify-end gap-1 max-lg:hidden">
          <button
            type="button"
            onClick={goToPrev}
            aria-label="Previous quote"
            className="rounded-md p-1.5 transition-colors hover:bg-muted"
          >
            <ChevronLeft className="h-4 w-4 text-muted-foreground" />
          </button>
          <button
            type="button"
            onClick={goToNext}
            aria-label="Next quote"
            className="rounded-md p-1.5 transition-colors hover:bg-muted"
          >
            <ChevronRight className="h-4 w-4 text-muted-foreground" />
          </button>
        </div>
      </div>

      <Dialog open={isReading} onOpenChange={setIsReading}>
        <DialogContent className="max-h-[90dvh] overflow-y-auto">
          <DialogHeader className="text-left">
            <DialogTitle>Daily Inspiration</DialogTitle>
            <p className="selectable text-base leading-relaxed text-foreground break-words">
              &ldquo;{currentQuote.text}&rdquo;
            </p>
            <DialogDescription>— {currentQuote.author}</DialogDescription>
          </DialogHeader>
        </DialogContent>
      </Dialog>
    </>
  );
}

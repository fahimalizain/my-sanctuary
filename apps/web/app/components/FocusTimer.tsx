import { useState, useEffect } from 'react';
import { Play, Pause, RotateCcw } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';

interface FocusTimerProps {
  className?: string;
}

export function FocusTimer({ className }: FocusTimerProps) {
  const [timeLeft, setTimeLeft] = useState(25 * 60);
  const [isRunning, setIsRunning] = useState(false);
  const [sessions, setSessions] = useState(3);

  useEffect(() => {
    if (!isRunning) return;

    const interval = setInterval(() => {
      setTimeLeft((prev) => {
        if (prev <= 0) {
          setIsRunning(false);
          setSessions((s) => s + 1);
          return 25 * 60;
        }
        return prev - 1;
      });
    }, 1000);

    return () => clearInterval(interval);
  }, [isRunning]);

  const minutes = Math.floor(timeLeft / 60);
  const seconds = timeLeft % 60;

  const formatTime = (m: number, s: number) => {
    return `${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}`;
  };

  const handleReset = () => {
    setIsRunning(false);
    setTimeLeft(25 * 60);
  };

  // Below lg this is one slim row (time + icon-only controls); at lg the
  // original card returns.
  return (
    <div
      className={cn(
        'bg-card rounded-xl p-6 shadow-sm border border-border',
        'max-lg:flex max-lg:h-14 max-lg:items-center max-lg:gap-3 max-lg:p-2',
        className,
      )}
    >
      <h2 className="font-heading text-lg font-semibold text-foreground mb-4 max-lg:sr-only">
        Focus Timer
      </h2>

      <div className="text-center mb-6 max-lg:mb-0 max-lg:flex-none max-lg:text-left">
        <div className="font-heading text-5xl font-bold text-primary mb-2 max-lg:mb-0 max-lg:text-2xl max-lg:leading-none">
          {formatTime(minutes, seconds)}
        </div>
        <p className="text-sm text-muted-foreground max-lg:hidden">
          {isRunning ? 'Focus in progress...' : 'Ready to focus?'}
        </p>
      </div>

      <div className="flex items-center justify-center gap-3 mb-6 max-lg:mb-0 max-lg:ml-auto max-lg:gap-2">
        <Button
          variant="outline"
          size="icon"
          onClick={handleReset}
          aria-label="Reset focus timer"
          className="rounded-full max-lg:h-9 max-lg:w-9"
        >
          <RotateCcw className="h-4 w-4" />
        </Button>
        <Button
          onClick={() => setIsRunning(!isRunning)}
          aria-label={isRunning ? 'Pause focus timer' : 'Start focus timer'}
          className="rounded-full px-8 bg-primary hover:bg-primary/90 max-lg:h-9 max-lg:w-9 max-lg:px-0"
        >
          {isRunning ? (
            <>
              <Pause className="h-4 w-4 mr-2 max-lg:mr-0" />
              <span className="max-lg:hidden">Pause</span>
            </>
          ) : (
            <>
              <Play className="h-4 w-4 mr-2 max-lg:mr-0" />
              <span className="max-lg:hidden">Start Focus</span>
            </>
          )}
        </Button>
      </div>

      <div className="text-center max-lg:hidden">
        <p className="text-sm text-muted-foreground">
          Sessions:{' '}
          <span className="font-semibold text-foreground">{sessions}</span>{' '}
          today
        </p>
      </div>
    </div>
  );
}

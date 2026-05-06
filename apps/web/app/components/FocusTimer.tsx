import { useState, useEffect } from 'react'
import { Play, Pause, RotateCcw } from 'lucide-react'
import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'

interface FocusTimerProps {
  className?: string
}

export function FocusTimer({ className }: FocusTimerProps) {
  const [timeLeft, setTimeLeft] = useState(25 * 60)
  const [isRunning, setIsRunning] = useState(false)
  const [sessions, setSessions] = useState(3)

  useEffect(() => {
    if (!isRunning) return

    const interval = setInterval(() => {
      setTimeLeft((prev) => {
        if (prev <= 0) {
          setIsRunning(false)
          setSessions((s) => s + 1)
          return 25 * 60
        }
        return prev - 1
      })
    }, 1000)

    return () => clearInterval(interval)
  }, [isRunning])

  const minutes = Math.floor(timeLeft / 60)
  const seconds = timeLeft % 60

  const formatTime = (m: number, s: number) => {
    return `${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}`
  }

  const handleReset = () => {
    setIsRunning(false)
    setTimeLeft(25 * 60)
  }

  return (
    <div className={cn('bg-card rounded-xl p-6 shadow-sm border border-border', className)}>
      <h2 className="font-heading text-lg font-semibold text-foreground mb-4">
        Focus Timer
      </h2>

      <div className="text-center mb-6">
        <div className="font-heading text-5xl font-bold text-primary mb-2">
          {formatTime(minutes, seconds)}
        </div>
        <p className="text-sm text-muted-foreground">
          {isRunning ? 'Focus in progress...' : 'Ready to focus?'}
        </p>
      </div>

      <div className="flex items-center justify-center gap-3 mb-6">
        <Button
          variant="outline"
          size="icon"
          onClick={handleReset}
          className="rounded-full"
        >
          <RotateCcw className="h-4 w-4" />
        </Button>
        <Button
          onClick={() => setIsRunning(!isRunning)}
          className="rounded-full px-8 bg-primary hover:bg-primary/90"
        >
          {isRunning ? (
            <>
              <Pause className="h-4 w-4 mr-2" />
              Pause
            </>
          ) : (
            <>
              <Play className="h-4 w-4 mr-2" />
              Start Focus
            </>
          )}
        </Button>
      </div>

      <div className="text-center">
        <p className="text-sm text-muted-foreground">
          Sessions: <span className="font-semibold text-foreground">{sessions}</span> today
        </p>
      </div>
    </div>
  )
}

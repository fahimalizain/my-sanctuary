import { useState, useEffect } from 'react'
import { ChevronLeft, ChevronRight, Quote } from 'lucide-react'
import { cn } from '@/lib/utils'

interface QuotesSectionProps {
  quotes: { text: string; author: string }[]
  className?: string
}

export function QuotesSection({ quotes, className }: QuotesSectionProps) {
  const [currentIndex, setCurrentIndex] = useState(0)

  useEffect(() => {
    const interval = setInterval(() => {
      setCurrentIndex((prev) => (prev + 1) % quotes.length)
    }, 10000)

    return () => clearInterval(interval)
  }, [quotes.length])

  const goToPrev = () => {
    setCurrentIndex((prev) => (prev - 1 + quotes.length) % quotes.length)
  }

  const goToNext = () => {
    setCurrentIndex((prev) => (prev + 1) % quotes.length)
  }

  const currentQuote = quotes[currentIndex]

  return (
    <div className={cn('bg-white rounded-xl p-6 shadow-sm', className)}>
      <div className="flex items-center gap-2 mb-4">
        <Quote className="h-5 w-5 text-sanctuary-green" />
        <h2 className="font-heading text-lg font-semibold text-foreground">
          Daily Inspiration
        </h2>
      </div>

      <div className="min-h-[100px] flex flex-col justify-center">
        <p className="text-lg text-foreground/90 leading-relaxed mb-3">
          "{currentQuote.text}"
        </p>
        <p className="text-sm text-muted-foreground">
          — {currentQuote.author}
        </p>
      </div>

      <div className="flex items-center justify-between mt-4">
        <div className="flex gap-1.5">
          {quotes.map((_, index) => (
            <button
              key={index}
              onClick={() => setCurrentIndex(index)}
              className={cn(
                'h-2 w-2 rounded-full transition-colors',
                index === currentIndex ? 'bg-sanctuary-green' : 'bg-gray-200'
              )}
            />
          ))}
        </div>
        <div className="flex gap-1">
          <button
            onClick={goToPrev}
            className="p-1.5 rounded-md hover:bg-gray-100 transition-colors"
          >
            <ChevronLeft className="h-4 w-4 text-gray-600" />
          </button>
          <button
            onClick={goToNext}
            className="p-1.5 rounded-md hover:bg-gray-100 transition-colors"
          >
            <ChevronRight className="h-4 w-4 text-gray-600" />
          </button>
        </div>
      </div>
    </div>
  )
}

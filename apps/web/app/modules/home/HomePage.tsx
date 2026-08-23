import { useState } from 'react';
import { Link } from '@tanstack/react-router';
import { Repeat } from 'lucide-react';
import { SkewedTimeline } from '@/app/components/SkewedTimeline';
import { FocusTimer } from '@/app/components/FocusTimer';
import { QuotesSection } from '@/app/components/QuotesSection';
import { todayItems, quotes } from '@/app/mock-data';
import type { TimelineItem } from '@/app/types';

export function HomePage() {
  const [items, setItems] = useState<TimelineItem[]>(todayItems);

  const handleItemsChange = (newItems: TimelineItem[]) => {
    setItems(newItems);
  };

  return (
    <div className="min-h-screen bg-cream">
      <div className="max-w-7xl mx-auto px-6 py-8">
        {/* Main Content */}
        <div className="grid grid-cols-1 lg:grid-cols-3 gap-8">
          {/* Left Panel - Timeline */}
          <div className="lg:col-span-2">
            <div className="flex items-center justify-between mb-6">
              <h1 className="font-heading text-2xl font-bold text-foreground">
                Today&apos;s Timeline
              </h1>
              <Link
                to="/routines"
                className="inline-flex items-center gap-1.5 text-sm font-medium text-primary hover:underline"
              >
                <Repeat className="h-4 w-4" />
                Routines
              </Link>
            </div>
            <SkewedTimeline items={items} onItemsChange={handleItemsChange} />
          </div>

          {/* Right Panel - Focus & Quotes */}
          <div className="space-y-6">
            <FocusTimer />
            <QuotesSection quotes={quotes} />
          </div>
        </div>
      </div>
    </div>
  );
}

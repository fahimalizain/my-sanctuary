import { Check } from 'lucide-react';
import { EVENT_LABEL_COLORS } from '@/lib/event-label-colors';
import { cn } from '@/lib/utils';

export interface EventLabelColorPickerProps {
  /** The selected palette hex (canonical lowercase `#rrggbb`). */
  value: string;
  onChange: (hex: string) => void;
  'aria-label'?: string;
}

/**
 * Presentational 24-swatch color picker (the Google event-label palette).
 * No freehand input: every value the form can send is one of the swatches,
 * so a stale hex never survives an edit round-trip.
 */
export function EventLabelColorPicker({
  value,
  onChange,
  'aria-label': ariaLabel = 'Color',
}: EventLabelColorPickerProps) {
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className="flex flex-wrap gap-2"
    >
      {EVENT_LABEL_COLORS.map((hex) => {
        const selected = hex === value;
        return (
          <button
            key={hex}
            type="button"
            role="radio"
            aria-checked={selected}
            aria-label={hex}
            title={hex}
            onClick={() => onChange(hex)}
            className={cn(
              'h-8 w-8 rounded-lg border border-input bg-background transition-transform',
              'hover:scale-110 focus:outline-none focus-visible:ring-2 focus-visible:ring-ring',
              selected &&
                'scale-110 ring-2 ring-ring ring-offset-2 ring-offset-background',
            )}
            style={{ backgroundColor: hex }}
          >
            {selected && (
              <Check className="mx-auto h-4 w-4 text-white drop-shadow" />
            )}
          </button>
        );
      })}
    </div>
  );
}

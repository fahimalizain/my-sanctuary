import { Monitor, Moon, Sun } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { ThemePreference } from '@/lib/theme';

interface ThemeOption {
  value: ThemePreference;
  label: string;
  Icon: typeof Sun;
}

const THEME_OPTIONS: ThemeOption[] = [
  { value: 'light', label: 'Light', Icon: Sun },
  { value: 'dark', label: 'Dark', Icon: Moon },
  { value: 'system', label: 'System', Icon: Monitor },
];

const OPTION_CLASS =
  'flex items-center gap-2 px-4 py-2 rounded-lg border-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background';

const SELECTED_CLASS = 'bg-primary/10 text-primary border-primary';
const UNSELECTED_CLASS = 'bg-muted text-muted-foreground border-transparent';

interface ThemePreferenceControlProps {
  /** The stored choice, not the resolved theme. */
  preference: ThemePreference;
  onChange: (preference: ThemePreference) => void;
}

export function ThemePreferenceControl({
  preference,
  onChange,
}: ThemePreferenceControlProps) {
  return (
    <div role="group" aria-label="Theme" className="flex gap-3">
      {THEME_OPTIONS.map(({ value, label, Icon }) => {
        const selected = preference === value;
        return (
          <button
            key={value}
            type="button"
            aria-pressed={selected}
            onClick={() => onChange(value)}
            className={cn(
              OPTION_CLASS,
              selected ? SELECTED_CLASS : UNSELECTED_CLASS,
            )}
          >
            <Icon className="h-4 w-4" />
            <span className="text-sm font-medium">{label}</span>
          </button>
        );
      })}
    </div>
  );
}

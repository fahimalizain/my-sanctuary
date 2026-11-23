import { lazy, Suspense } from 'react';
import type { RichTextEditorProps } from './RichTextEditorImpl';

export type { RichTextFormat } from './RichTextEditorImpl';

/**
 * Lazy boundary around the TipTap editor.
 *
 * TipTap + ProseMirror are a sizeable chunk and only needed once a dialog is
 * opened. The fallback is a working plain textarea, so the field is usable
 * during the few milliseconds the chunk takes to load (and stays usable if the
 * chunk ever fails to load on a flaky connection).
 */
const LazyEditor = lazy(() =>
  import('./RichTextEditorImpl').then((module) => ({
    default: module.RichTextEditorImpl,
  })),
);

export function RichTextEditor(props: RichTextEditorProps) {
  return (
    <Suspense fallback={<EditorFallback {...props} />}>
      <LazyEditor {...props} />
    </Suspense>
  );
}

function EditorFallback({
  value,
  onChange,
  placeholder,
  disabled,
  className,
}: RichTextEditorProps) {
  return (
    <textarea
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      disabled={disabled}
      rows={3}
      aria-label="Description"
      className={[
        'w-full resize-none rounded-xl border border-input bg-background px-4 py-3 text-sm text-foreground placeholder:text-muted-foreground/60',
        'focus:border-primary focus:outline-none focus:ring-2 focus:ring-primary/20',
        disabled ? 'opacity-60' : '',
        className ?? '',
      ].join(' ')}
    />
  );
}

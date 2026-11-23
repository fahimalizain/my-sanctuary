import { useEffect, useRef, type ReactNode } from 'react';
import { EditorContent, useEditor } from '@tiptap/react';
import StarterKit from '@tiptap/starter-kit';
import { Markdown } from '@tiptap/markdown';
import { Placeholder } from '@tiptap/extensions';
import {
  Bold,
  Code,
  Heading1,
  Heading2,
  Heading3,
  Italic,
  Link as LinkIcon,
  List,
  ListOrdered,
  Quote,
  Redo2,
  RemoveFormatting,
  Strikethrough,
  Undo2,
} from 'lucide-react';
import { cn } from '@/lib/utils';

export type RichTextFormat = 'markdown' | 'html';

export interface RichTextEditorProps {
  /** Markdown when `format="markdown"`, HTML when `format="html"`. */
  value: string;
  onChange: (value: string) => void;
  /** `"markdown"` for tasks/routines; `"html"` for Google-owned events. */
  format?: RichTextFormat;
  placeholder?: string;
  /** Disables the toolbar and editing chrome. */
  disabled?: boolean;
  /** Extra classes for the outer border container. */
  className?: string;
}

/**
 * The app's one WYSIWYG description field.
 *
 * Markdown is the storage format for tasks/routines (ADR 0015); the editor is
 * deliberately unaware of that beyond `getMarkdown()`. Event descriptions use
 * the same component with `format="html"` because Google owns their markup.
 *
 * Keeping the TipTap surface here means swapping the editor later is a
 * one-file change: callers only pass a string in and get a string out.
 */
export function RichTextEditorImpl({
  value,
  onChange,
  format = 'markdown',
  placeholder = 'Add more details...',
  disabled = false,
  className,
}: RichTextEditorProps) {
  // Editor callbacks are captured when the editor is created. Refs keep them
  // current without recreating the editor on every parent render.
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const lastValueRef = useRef(value);

  const editor = useEditor(
    {
      extensions: [
        StarterKit.configure({
          heading: { levels: [1, 2, 3] },
          link: {
            openOnClick: false,
            autolink: true,
            linkOnPaste: true,
            HTMLAttributes: {
              rel: 'noopener noreferrer nofollow',
              target: '_blank',
            },
          },
        }),
        ...(format === 'markdown'
          ? [
              Markdown.configure({
                // Single newlines are hard breaks so existing plain-text
                // descriptions keep their shape.
                markedOptions: { gfm: true, breaks: true },
              }),
            ]
          : []),
        Placeholder.configure({ placeholder }),
      ],
      content: value,
      contentType: format,
      editable: !disabled,
      editorProps: {
        attributes: {
          class: 'sanctuary-editor-content',
          'aria-label': 'Description',
        },
      },
      onUpdate: ({ editor }) => {
        // TipTap's Markdown serializer ends with a newline; trim it so a
        // no-op edit does not churn `updated_at` or store blank lines.
        const next = (
          format === 'markdown' ? editor.getMarkdown() : editor.getHTML()
        ).trimEnd();
        if (next === lastValueRef.current) return;
        lastValueRef.current = next;
        onChangeRef.current(next);
      },
    },
    [format, disabled],
  );

  // External resets (opening a different task/routine, clearing the form)
  // must win over the editor's current document. The last-emitted guard
  // prevents echoing the parent's own update back into the editor.
  useEffect(() => {
    if (!editor || value === lastValueRef.current) return;
    lastValueRef.current = value;
    editor.commands.setContent(value, { contentType: format });
  }, [editor, value, format]);

  useEffect(() => {
    if (!editor) return;
    editor.setEditable(!disabled);
  }, [editor, disabled]);

  if (!editor) {
    return (
      <div
        className={cn(
          'rounded-xl border border-input bg-background px-4 py-3 text-sm text-muted-foreground',
          className,
        )}
      >
        Loading editor…
      </div>
    );
  }

  const run = (action: () => void) => action();

  const setLink = () => {
    const previous =
      (editor.getAttributes('link').href as string | undefined) ?? '';
    const raw = window.prompt('Link URL', previous || 'https://');
    if (raw === null) return;
    const trimmed = raw.trim();
    if (!trimmed) {
      editor.chain().focus().extendMarkRange('link').unsetLink().run();
      return;
    }
    const href = normalizeHref(trimmed);
    if (!href) {
      window.alert('Use an http(s) or mailto link.');
      return;
    }
    editor.chain().focus().extendMarkRange('link').setLink({ href }).run();
  };

  return (
    <div
      className={cn(
        'overflow-hidden rounded-xl border border-input bg-background transition-all',
        'focus-within:border-primary focus-within:ring-2 focus-within:ring-primary/20',
        disabled && 'opacity-60',
        className,
      )}
    >
      <div
        className="sanctuary-editor-toolbar flex flex-nowrap items-center gap-0.5 overflow-x-auto border-b border-border/70 bg-muted/30 px-2 py-1.5"
        role="toolbar"
        aria-label="Formatting"
      >
        <ToolbarButton
          label="Bold"
          active={editor.isActive('bold')}
          onClick={() => run(() => editor.chain().focus().toggleBold().run())}
        >
          <Bold className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Italic"
          active={editor.isActive('italic')}
          onClick={() => run(() => editor.chain().focus().toggleItalic().run())}
        >
          <Italic className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Strikethrough"
          active={editor.isActive('strike')}
          onClick={() => run(() => editor.chain().focus().toggleStrike().run())}
        >
          <Strikethrough className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Inline code"
          active={editor.isActive('code')}
          onClick={() => run(() => editor.chain().focus().toggleCode().run())}
        >
          <Code className="h-3.5 w-3.5" />
        </ToolbarButton>

        <Divider />

        <ToolbarButton
          label="Heading 1"
          active={editor.isActive('heading', { level: 1 })}
          onClick={() =>
            run(() => editor.chain().focus().toggleHeading({ level: 1 }).run())
          }
        >
          <Heading1 className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Heading 2"
          active={editor.isActive('heading', { level: 2 })}
          onClick={() =>
            run(() => editor.chain().focus().toggleHeading({ level: 2 }).run())
          }
        >
          <Heading2 className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Heading 3"
          active={editor.isActive('heading', { level: 3 })}
          onClick={() =>
            run(() => editor.chain().focus().toggleHeading({ level: 3 }).run())
          }
        >
          <Heading3 className="h-3.5 w-3.5" />
        </ToolbarButton>

        <Divider />

        <ToolbarButton
          label="Bullet list"
          active={editor.isActive('bulletList')}
          onClick={() =>
            run(() => editor.chain().focus().toggleBulletList().run())
          }
        >
          <List className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Numbered list"
          active={editor.isActive('orderedList')}
          onClick={() =>
            run(() => editor.chain().focus().toggleOrderedList().run())
          }
        >
          <ListOrdered className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Quote"
          active={editor.isActive('blockquote')}
          onClick={() =>
            run(() => editor.chain().focus().toggleBlockquote().run())
          }
        >
          <Quote className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Link"
          active={editor.isActive('link')}
          onClick={setLink}
        >
          <LinkIcon className="h-3.5 w-3.5" />
        </ToolbarButton>

        <Divider />

        <ToolbarButton
          label="Clear formatting"
          onClick={() =>
            run(() => editor.chain().focus().unsetAllMarks().clearNodes().run())
          }
        >
          <RemoveFormatting className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Undo"
          onClick={() => run(() => editor.chain().focus().undo().run())}
        >
          <Undo2 className="h-3.5 w-3.5" />
        </ToolbarButton>
        <ToolbarButton
          label="Redo"
          onClick={() => run(() => editor.chain().focus().redo().run())}
        >
          <Redo2 className="h-3.5 w-3.5" />
        </ToolbarButton>
      </div>
      <EditorContent editor={editor} />
    </div>
  );
}

function ToolbarButton({
  label,
  active = false,
  onClick,
  children,
}: {
  label: string;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={active}
      title={label}
      // Keep the editor selection while pressing a toolbar button.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      className={cn(
        'shrink-0 rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-background hover:text-foreground',
        active && 'bg-background text-primary shadow-sm',
      )}
    >
      {children}
    </button>
  );
}

function Divider() {
  return <span className="mx-1 h-4 w-px shrink-0 bg-border/70" aria-hidden />;
}

/** Only http(s) and mailto links leave the editor. */
function normalizeHref(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (/^(https?:|mailto:)/i.test(trimmed)) return trimmed;
  // A bare domain is friendly enough to upgrade; anything else is refused.
  if (/^[\w.-]+\.[a-z]{2,}(\/.*)?$/i.test(trimmed)) return `https://${trimmed}`;
  return null;
}

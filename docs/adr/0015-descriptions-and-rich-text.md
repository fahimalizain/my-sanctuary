# ADR 0015: Descriptions and rich text

Status: Accepted
Date: 2026-11-21

> Implementation note (2026-11-23): the routine `description` column and the
> shared editor are implemented. Tasks and routines use the Markdown mode;
> calendar event descriptions now use the same editor in HTML mode, with
> sanitized read-only rendering for plain text and Google/Notion HTML.

## Context

Tasks have always carried an optional `description` (notes). Routines — the
standing definitions that seed Home — did not, even though a routine's notes
are useful on the one-shot Google event created when an occurrence starts.

Separately, the UI edits every description in a plain `<textarea>`. We want a
WYSIWYG editor on task, routine, and calendar-event descriptions. The format
choice matters because the same text crosses three boundaries:

- **D1 / the API** — where our own data lives.
- **MCP tools and agents** — which read and write task descriptions.
- **Google Calendar** — whose event `description` is HTML and is also written
  by Notion Calendar, Google Calendar's own formatting bar, and other clients.

Notion Calendar's event descriptions expose headers, links, and lists. Google
Calendar API v3 documents `description` as "Can contain HTML", so HTML is the
interchange format that survives a Notion Calendar → Google sync. Notion's
Markdown-style shortcuts are input ergonomics, not a stored Markdown mode.

HTML is not universally portable, though. Google renders a limited subset;
Apple Calendar and Outlook show raw `<b>`/`<br>` tags. iCalendar RFC 5545
defines `DESCRIPTION` as plain text, and RFC 9073's `STYLED-DESCRIPTION` is
not widely implemented.

## Decision

### Local entities use Markdown

`tasks.description` and `routines.description` are Markdown strings in `TEXT`
columns. The supported subset is CommonMark plus the small GFM pieces a
description needs: paragraphs, H1–H3, bold, italic, strike, inline code,
code blocks, links, bullet/ordered lists, blockquotes, and horizontal rules.

Existing plain-text descriptions are already valid Markdown, so there is no
data migration. Soft line breaks are treated as hard breaks on read and in
the editor, so pre-existing multi-line notes keep their line breaks.

The editor is WYSIWYG. Users see formatted text, not Markdown source, although
Markdown shortcuts (`#`, `**`, `-`, `1.`) work while typing.

### Google-owned events use HTML

A calendar event's description is owned by Google. `calendar_events` rows are
a sync replica, and Google may return HTML produced by another client. We do
not convert inbound event HTML into Markdown; that round-trip is lossy and
would rewrite formatting created elsewhere. Event descriptions are rendered
through a strict sanitizer and, when editable, edited as HTML.

### Outbound task/routine notes are plain text

When a task or routine starts and creates a Google event, its Markdown is
rendered to clean plain text: emphasis markers removed, list markers kept,
links written as `label (url)`, headings separated by blank lines. Blank notes
omit the `description` key entirely.

Sending HTML from local entities is deliberately deferred. It renders in
Google and Notion Calendar but shows raw tags in Apple Calendar and Outlook.
The plain-text mapping keeps every client readable.

### Editor and sanitization

- TipTap 3 (`@tiptap/react`, `@tiptap/starter-kit`, `@tiptap/markdown`) is the
  shared editor. One component exposes `format="markdown"` for tasks/routines
  and `format="html"` for events. It is lazy-loaded behind a plain-textarea
  fallback, so the editor chunk (~153 KB gzip) is only fetched when a dialog
  opens and the initial app bundle is unchanged.
- When a read-only Markdown rendering is added (Phase 3), it uses
  `react-markdown` + `remark-gfm` + `remark-breaks` and never `rehype-raw`;
  raw HTML is not executed.
- Inbound event HTML will be sanitized with DOMPurify before rendering
  (Phase 3).
- Markdown-to-calendar-text lives in `api-core` (`pulldown-cmark`, no default
  features) so task starts, occurrence starts, and future MCP paths share one
  mapping.

## Consequences

- Routine descriptions are optional and nullable in D1; `NULL` reads as `""`
  exactly like task notes.
- The routine description is copied to the one-shot event when an occurrence
  starts. It is a create-time copy: later edits to the event stay in Google,
  and later edits to the routine affect future starts only.
- Descriptions are capped at 16 KB (`400 description too long`) to bound D1
  rows and editor payloads.
- `@tiptap/markdown` is still marked beta upstream. Keep the version pinned,
  keep Markdown as the source of truth, and rely on the round-trip tests; the
  editor is isolated behind `RichTextEditor`, so it can be swapped without
  touching the API or stored data.
- Adding images, tables, colors, mentions, or collaborative editing requires
  revisiting this ADR. ProseMirror/Lexical JSON is not a storage format here.
- Event HTML editing can normalize Google's markup. A plain-text fallback
  remains available for events that must stay readable in Apple Calendar and
  Outlook.

import DOMPurify from 'dompurify';
import { normalizeEditorHtml } from '@/lib/rich-text-html';

/**
 * Google-owned event descriptions are HTML. Notion Calendar and Google
 * Calendar's own formatting bar write that HTML; older/plain notes are still
 * common. These helpers keep the boundary honest:
 *
 * - plain text is escaped and its newlines become `<br>` before editing
 * - inbound HTML is sanitized before rendering or loading into the editor
 * - the editor writes HTML back, because Google is the owner
 */

const HTML_TAG = /<\/?[a-z][^>]*>/i;

const ALLOWED_TAGS = [
  'p',
  'div',
  'span',
  'br',
  'b',
  'strong',
  'i',
  'em',
  'u',
  's',
  'strike',
  'del',
  'ul',
  'ol',
  'li',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'blockquote',
  'pre',
  'code',
  'a',
  'hr',
  'table',
  'thead',
  'tbody',
  'tr',
  'th',
  'td',
];

const ALLOWED_ATTR = ['href', 'title'];

// Lazily created so importing this module never touches `window` (for
// example, a pure calendar-clipboard test that only pulls in EventInspector).
let purifier: ReturnType<typeof DOMPurify> | null = null;

function getPurifier(): ReturnType<typeof DOMPurify> {
  if (purifier) return purifier;
  purifier = DOMPurify(window);
  // Links from Google/Notion open in a new tab without handing the opener to
  // the target.
  purifier.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName === 'A') {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer nofollow');
    }
  });
  return purifier;
}

/** Whether a stored event description looks like HTML rather than plain text. */
export function isHtmlDescription(value: string): boolean {
  return HTML_TAG.test(value);
}

/** Escapes text for safe inclusion in the editor's HTML. */
export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

/** Converts a plain-text event note to the HTML the editor expects. */
export function plainTextToEditorHtml(value: string): string {
  if (!value) return '';
  const escaped = escapeHtml(value);
  return `<p>${escaped.replace(/\r\n?/g, '\n').replace(/\n/g, '<br>')}</p>`;
}

/** Strips everything outside the event-description allowlist. */
export function sanitizeEventDescriptionHtml(html: string): string {
  return getPurifier().sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOW_DATA_ATTR: false,
  });
}

/**
 * The value handed to the HTML editor for a stored event description.
 * Plain text is preserved line-for-line; HTML is sanitized and normalized.
 */
export function eventDescriptionToEditorHtml(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return '';
  if (!isHtmlDescription(trimmed)) return plainTextToEditorHtml(value);
  return normalizeEditorHtml(sanitizeEventDescriptionHtml(trimmed));
}

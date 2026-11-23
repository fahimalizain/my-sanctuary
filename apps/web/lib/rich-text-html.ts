/**
 * Small helpers for the editor's HTML mode.
 *
 * These answer "did the user actually write anything?" without parsing into
 * the DOM: TipTap leaves `<p></p>` / `<p><br></p>` behind when the field is
 * emptied, and an all-whitespace description must clear the stored value.
 */
export function isEmptyEditorHtml(html: string): boolean {
  const trimmed = html.trim();
  if (!trimmed) return true;
  // A horizontal rule or image is visible content even without text.
  if (/<(img|hr)\b/i.test(trimmed)) return false;
  const text = trimmed
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<[^>]*>/g, '')
    .replace(/&nbsp;/gi, ' ')
    .trim();
  return text.length === 0;
}

/** Trims trailing whitespace and maps an empty document to `''`. */
export function normalizeEditorHtml(html: string): string {
  const trimmed = html.trimEnd();
  return isEmptyEditorHtml(trimmed) ? '' : trimmed;
}

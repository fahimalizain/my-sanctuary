/**
 * The 24 default Google event-label hexes (lowercase `#rrggbb`), mirrored
 * from the API palette.
 *
 * KEEP IN SYNC with `GOOGLE_EVENT_LABEL_COLORS` and
 * `DEFAULT_EVENT_LABEL_COLOR` in `packages/api-core/src/google_color.rs`.
 */
export const EVENT_LABEL_COLORS = [
  '#009688', // eucalyptus
  '#039be5', // peacock
  '#0b8043', // basil
  '#33b679', // sage
  '#3f51b5', // blueberry
  '#4285f4', // cobalt
  '#616161', // graphite
  '#795548', // cocoa
  '#7986cb', // lavender
  '#7cb342', // pistachio
  '#8e24aa', // grape
  '#9e69af', // amethyst
  '#a79b8e', // birch
  '#ad1457', // radicchio
  '#b39ddb', // wisteria
  '#c0ca33', // avocado
  '#d50000', // tomato
  '#d81b60', // cherry blossom
  '#e4c441', // citron
  '#e67c73', // flamingo
  '#ef6c00', // pumpkin
  '#f09300', // mango
  '#f4511e', // tangerine
  '#f6bf26', // banana
] as const;

/** Category/list create-dialog default (peacock). */
export const DEFAULT_EVENT_LABEL_COLOR = '#039be5';

/**
 * True iff `hex` is exactly one of the 24 event-label hexes.
 *
 * Membership is exact after trimming and case-folding (`#039BE5` counts,
 * `#abc` does not — shorthand is not expanded in the UI). The API stores
 * canonical lowercase, so callers may lowercase the result before submit.
 */
export function isEventLabelHex(hex: string): boolean {
  const normalized = hex.trim().toLowerCase();
  return EVENT_LABEL_COLORS.some((color) => color === normalized);
}

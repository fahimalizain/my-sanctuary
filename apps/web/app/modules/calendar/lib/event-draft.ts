/** Title is persist-worthy only when the user typed something. */
export function isPersistableDraftTitle(title: string): boolean {
  return title.trim().length > 0;
}

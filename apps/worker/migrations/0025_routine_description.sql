-- Routines gain the same optional notes field tasks carry. Nullable so the
-- migration is a metadata-only ALTER for existing rows; the Rust projection
-- maps NULL to "" via `de_empty_string` (same convention as `tasks.description`).
ALTER TABLE routines ADD COLUMN description TEXT;

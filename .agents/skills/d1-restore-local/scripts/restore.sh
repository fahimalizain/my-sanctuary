#!/usr/bin/env bash
#
# restore.sh — copy remote sanctuary-db onto the local wrangler D1 database.
# Safe by construction: every `d1 execute` below is hardcoded to --local, so
# this script can never write to the remote (production) D1 database.
set -euo pipefail

DB_NAME="sanctuary-db"
ASSUME_YES=0
KEEP_DUMP=0
DUMP=""
DUMP_GIVEN=0

# Default to the sanctuary Cloudflare account. Wrangler `d1 export --remote`
# fails in non-interactive mode when the token can see multiple accounts
# (`unable to select one in non-interactive mode`) even though `whoami`
# lists only this one. Honour an explicit CLOUDFLARE_ACCOUNT_ID if set.
: "${CLOUDFLARE_ACCOUNT_ID:=95ec2591c70d5cf2f2e07bb70e252be6}"
export CLOUDFLARE_ACCOUNT_ID

usage() {
  cat <<'EOF'
restore.sh - restore production D1 (sanctuary-db) onto local wrangler D1.

Usage:
  bash .agents/skills/d1-restore-local/scripts/restore.sh [options]

Options:
  -y, --yes        skip the wipe confirmation (non-interactive use)
      --keep-dump  keep the SQL dump in tmp/d1/ after a successful restore
      --dump PATH  import an existing dump instead of exporting from remote
  -h, --help       show this help and exit

Writes ONLY to local D1 (apps/worker/.wrangler/state/v3/d1).
Never writes to production.
EOF
}

# --- locate the repo root (works from any cwd) ------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT=""
dir="$SCRIPT_DIR"
while [ "$dir" != "/" ]; do
  if [ -f "$dir/apps/worker/wrangler.toml" ]; then
    REPO_ROOT="$dir"
    break
  fi
  dir="$(dirname "$dir")"
done
if [ -z "$REPO_ROOT" ]; then
  REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [ -z "$REPO_ROOT" ] || [ ! -f "$REPO_ROOT/apps/worker/wrangler.toml" ]; then
  printf 'error: could not locate repo root (apps/worker/wrangler.toml not found)\n' >&2
  exit 1
fi

WRANGLER_DIR="$REPO_ROOT/apps/worker"
D1_STATE_DIR="$WRANGLER_DIR/.wrangler/state/v3/d1"
DUMP_DIR="$REPO_ROOT/tmp/d1"

# Run wrangler from the repo root so `npx` always resolves the workspace copy,
# no matter where this script was invoked from.
run_wrangler() {
  (cd "$REPO_ROOT" && npx wrangler "$@")
}

# --- flags ------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    -y|--yes) ASSUME_YES=1; shift ;;
    --keep-dump) KEEP_DUMP=1; shift ;;
    --dump)
      if [ $# -lt 2 ]; then
        printf 'error: --dump requires a PATH argument\n' >&2
        usage >&2
        exit 2
      fi
      DUMP="$2"
      DUMP_GIVEN=1
      shift 2
      ;;
    -h|--help) usage; exit 0 ;;
    *)
      printf 'error: unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

# --- safety: auth -----------------------------------------------------------
printf '==> checking Cloudflare auth\n'
if ! run_wrangler whoami --cwd "$WRANGLER_DIR" >/dev/null 2>&1; then
  printf 'error: not authenticated with Cloudflare.\n' >&2
  printf '  Run `npx wrangler login` (from the repo root), then re-run this script.\n' >&2
  exit 1
fi
printf '    ok\n'

# --- safety: wipe confirmation ----------------------------------------------
confirm_wipe() {
  if [ "$ASSUME_YES" -eq 1 ]; then
    return 0
  fi
  if [ -t 0 ]; then
    printf 'This WIPES local sanctuary-db. Continue? [y/N] '
    read -r answer || true
    case "$answer" in
      [yY]|[yY][eE][sS]) return 0 ;;
      *)
        printf 'aborting (local D1 untouched).\n' >&2
        exit 1
        ;;
    esac
  fi
  printf 'error: refusing to wipe local D1 without confirmation.\n' >&2
  printf '  Wiping local D1 requires --yes; re-run once the user confirms\n' >&2
  printf '  they accept losing the local D1 data.\n' >&2
  exit 2
}

# --- safety: local D1 must not be in use (SQLite lock) -----------------------
check_local_d1_free() {
  [ -d "$D1_STATE_DIR" ] || return 0
  local holder=""
  if command -v lsof >/dev/null 2>&1; then
    holder="$(lsof +D "$D1_STATE_DIR" 2>/dev/null | grep -E '\.sqlite' || true)"
  fi
  if [ -z "$holder" ] && command -v pgrep >/dev/null 2>&1; then
    if pgrep -f 'wrangler dev|workerd' >/dev/null 2>&1; then
      holder="a wrangler/workerd process is running"
    fi
  fi
  if [ -n "$holder" ]; then
    printf 'error: local D1 state at %s is in use (SQLite lock).\n' "$D1_STATE_DIR" >&2
    printf '  Stop `npx nx serve worker` (wrangler dev) first, then re-run this script.\n' >&2
    exit 1
  fi
}

# --- local wipe --------------------------------------------------------------
wipe_local_d1() {
  printf '==> wiping local D1 state (%s)\n' "$D1_STATE_DIR"
  rm -rf "$D1_STATE_DIR"
}

# --- find the local sqlite file (fallback import target) ---------------------
find_local_sqlite() {
  local files=() miniflare_files=() f chosen="" size="" best=-1
  while IFS= read -r f; do
    files+=("$f")
  done < <(find "$D1_STATE_DIR" -type f \
    -name '*.sqlite' ! -name 'metadata.sqlite' ! -name '*-shm' ! -name '*-wal' \
    2>/dev/null)
  for f in "${files[@]}"; do
    case "$f" in
      */miniflare-D1DatabaseObject/*) miniflare_files+=("$f") ;;
    esac
  done
  if [ "${#miniflare_files[@]}" -gt 0 ]; then
    files=("${miniflare_files[@]}")
  fi
  for f in "${files[@]}"; do
    size="$(wc -c < "$f" | tr -d ' ')"
    if [ "$size" -gt "$best" ]; then
      chosen="$f"
      best="$size"
    fi
  done
  printf '%s' "$chosen"
}

# --- verification -------------------------------------------------------------
verify_local_d1() {
  printf '==> verifying local D1\n'
  local out="" tables="" t n=""
  out="$(run_wrangler d1 execute "$DB_NAME" --local --json --command \
    "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name;" \
    --yes --cwd "$WRANGLER_DIR" 2>/dev/null || true)"
  tables="$(printf '%s' "$out" | grep -oE '"name":[[:space:]]*"[^"]*"' \
    | sed -E 's/"name":[[:space:]]*"//; s/"$//' | paste -sd ', ' - || true)"
  if [ -n "$tables" ]; then
    printf '    tables: %s\n' "$tables"
  else
    printf '    (no user tables found)\n'
  fi
  for t in users tasks calendar_events routines agenda_items task_lists; do
    out="$(run_wrangler d1 execute "$DB_NAME" --local --json --command \
      "SELECT COUNT(*) AS count FROM \"$t\";" \
      --yes --cwd "$WRANGLER_DIR" 2>/dev/null || true)"
    n="$(printf '%s' "$out" | sed -nE 's/.*"count":[[:space:]]*([0-9]+).*/\1/p' | head -n1)"
    if [ -n "$n" ]; then
      printf '    %-16s %s rows\n' "$t" "$n"
    else
      printf '    %-16s (missing)\n' "$t"
    fi
  done
}

# --- main --------------------------------------------------------------------
confirm_wipe
check_local_d1_free

if [ "$DUMP_GIVEN" -eq 1 ]; then
  case "$DUMP" in
    /*) ;;
    *) DUMP="$PWD/$DUMP" ;;
  esac
  if [ ! -f "$DUMP" ]; then
    printf 'error: dump file not found: %s\n' "$DUMP" >&2
    exit 1
  fi
  printf '==> using existing dump %s\n' "$DUMP"
else
  printf '==> exporting remote sanctuary-db (read-only)\n'
  mkdir -p "$DUMP_DIR"
  DUMP="$DUMP_DIR/prod-$(date +%Y%m%d-%H%M%S).sql"
  run_wrangler d1 export "$DB_NAME" --remote --output="$DUMP" --skip-confirmation --cwd "$WRANGLER_DIR"
  printf '    exported to %s\n' "$DUMP"
fi

wipe_local_d1

printf '==> importing dump into local D1\n'
if run_wrangler d1 execute "$DB_NAME" --local --file="$DUMP" --yes --cwd "$WRANGLER_DIR"; then
  :
else
  printf '    wrangler import failed; falling back to sqlite3 with deferred FKs\n' >&2
  wipe_local_d1
  printf '==> initializing empty local D1\n'
  if ! run_wrangler d1 execute "$DB_NAME" --local --command 'SELECT 1;' --yes --cwd "$WRANGLER_DIR"; then
    printf 'error: could not initialize local D1.\n' >&2
    printf '  Local D1 was wiped; re-run the script.\n' >&2
    exit 1
  fi
  DB_FILE="$(find_local_sqlite)"
  if [ -z "$DB_FILE" ]; then
    printf 'error: no local sqlite file found under %s\n' "$D1_STATE_DIR" >&2
    printf '  Local D1 was wiped; re-run the script.\n' >&2
    exit 1
  fi
  if ! command -v sqlite3 >/dev/null 2>&1; then
    printf 'error: sqlite3 not found on PATH.\n' >&2
    printf '  Install it (e.g. `brew install sqlite`) and re-run.\n' >&2
    printf '  Local D1 was wiped; re-run the script after installing sqlite3.\n' >&2
    exit 1
  fi
  printf '==> applying dump via sqlite3 (%s)\n' "$DB_FILE"
  if ! {
    printf '%s\n' 'PRAGMA defer_foreign_keys = ON;'
    cat "$DUMP"
  } | sqlite3 "$DB_FILE"; then
    printf 'error: sqlite3 import failed.\n' >&2
    printf '  Local D1 was wiped; re-run the script.\n' >&2
    exit 1
  fi
fi

verify_local_d1

if [ "$KEEP_DUMP" -eq 1 ]; then
  printf '==> keeping dump at %s\n' "$DUMP"
  printf '    NOTE: the dump contains google_oauth_tokens; do not cat or commit it.\n'
elif [ "$DUMP_GIVEN" -eq 1 ]; then
  printf '==> keeping user-supplied dump (not deleted)\n'
else
  printf '==> removing dump %s\n' "$DUMP"
  rm -f "$DUMP"
fi

printf '==> done: local sanctuary-db restored\n'

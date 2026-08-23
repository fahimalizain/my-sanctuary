---
name: d1-restore-local
description: Restore the production Cloudflare D1 database (sanctuary-db) onto the local wrangler D1 used by wrangler dev. Use when the user wants to pull, copy, sync, dump, or restore production D1 locally, refresh local data from prod, or mentions d1 export, local D1, or sanctuary-db restore.
metadata:
  author: my-sanctuary
  version: '1.0.0'
---

# Restore Production D1 Locally

## What it does

Copies the remote `sanctuary-db` onto the local wrangler D1 database (the one `wrangler dev` reads). It wipes the local database first so the restore is a true copy, never a merge — and it can never write to production.

## Quick start

1. Confirm the user accepts losing the local D1 data.
2. Stop `npx nx serve worker` / `wrangler dev` if it is running (SQLite lock).
3. From the repo root, after the user confirms the wipe, run non-interactively:

   ```
   bash .agents/skills/d1-restore-local/scripts/restore.sh --yes
   ```

   Interactive (TTY) use: omit `--yes` to get a wipe prompt.
4. Optional: `--keep-dump` keeps the SQL dump in `tmp/d1/` for later re-imports.
5. Report the verification counts from the script output. Never print dump contents.

## Hard rules

- Never `wrangler d1 execute --remote` with a dump file.
- Never `wrangler d1 time-travel restore` (mutates production).
- Always `--local` on execute/import.
- The dump lives in `tmp/d1/`; delete it after restore unless `--keep-dump`.
- Do not `cat` or commit the dump — it contains `google_oauth_tokens`.

## Facts

| Item | Value |
|------|-------|
| Wrangler config | `apps/worker/wrangler.toml` |
| Database name | `sanctuary-db` (not the binding name) |
| Binding | `DB` |
| database_id | `7fcdb90e-d286-412d-9161-a63e62ebaf44` |
| account_id | `95ec2591c70d5cf2f2e07bb70e252be6` (Fahimalizain@gmail.com) |
| Wrangler cwd | `apps/worker` (`--cwd apps/worker` from repo root) |
| Local persist | `apps/worker/.wrangler/state/v3/d1/` |
| Dump location | `tmp/d1/` (gitignored) |
| Package manager | npm (`npx wrangler ...`) |
| wrangler | `^4.0.0` |

## Troubleshooting

| Problem | Fix |
|---------|-----|
| Auth / error 10000 | `npx wrangler login`, then re-run the script |
| `More than one account available` (unable to select in non-interactive mode) | Script defaults `CLOUDFLARE_ACCOUNT_ID` to Fahimalizain's account; override the env var only if targeting a different account. Prefer re-running the script. |
| Database is locked | Stop `npx nx serve worker`, then re-run |
| Table already exists | `rm -rf apps/worker/.wrangler/state/v3/d1`, then re-run |
| FOREIGN KEY constraint | Script falls back to sqlite3; install it (`brew install sqlite`) if missing |
| UNIQUE d1_migrations | Wipe local state and re-run (do not apply migrations on top of a full dump) |

Prefer re-running the script over hand-rolling wrangler commands.

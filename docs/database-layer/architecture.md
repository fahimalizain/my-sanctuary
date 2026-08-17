# Architecture

The database layer lives in `@packages/api-core`, the shared Go library consumed
by both `@apps/api` (native local dev) and `@apps/cloudflare-deploy` (Cloudflare
Workers WASM + D1).

## Two targets, one interface

The architecture uses **repository interfaces** defined in the shared package,
with two implementations selected at link time via Go build tags:

| Target                          | GOOS/GOARCH      | Impl file      | Driver                         |
| ------------------------------- | ---------------- | -------------- | ------------------------------ |
| `apps/api` (local)              | `darwin`/`linux` | `gorm_repo.go` | `glebarez/sqlite` (pure Go)    |
| `apps/cloudflare-deploy` (WASM) | `js`/`wasm`      | `d1_repo.go`   | Cloudflare D1 via `syscall/js` |

Because the two targets have different `GOOS`, **build tags are mandatory** and
the GORM/D1 files are mutually exclusive at compile time. This keeps the WASM
binary free of GORM and the pure-Go SQLite driver, which cannot run under
`GOOS=js` anyway.

## File layout

```
packages/api-core/
├── models/
│   └── models.go              # User, GoogleOAuthToken, GoogleCalendar, CalendarEvent
├── repository/
│   ├── repository.go          # Interfaces, common types (no build tag)
│   ├── errors.go              # ErrNotFound, ErrConflict
│   ├── gorm_repo.go           # //go:build !js — GORM + SQLite impl
│   ├── gorm_repo_test.go      # //go:build !js — in-memory tests
│   └── d1_repo.go             # //go:build js — D1 via syscall/js
├── auth/
│   └── token_refresher.go     # OAuth2 token refresh service (uses TokenRepo)
├── handlers/
│   ├── handlers.go            # Dependencies struct holds *Repo fields
│   ├── auth.go                # Uses UserRepo + TokenRepo; cookie session
│   └── calendar.go            # Uses CalendarRepo + CalendarEventRepo as cache
└── config/config.go           # DatabaseDSN (local) + D1 binding name

apps/cloudflare-deploy/
├── main.go                    # Wires D1 repos; reads env.DB via shim
├── d1_shim.js                 # JS glue: stashes env.DB on globalThis.__D1__
├── build.sh                   # GOOS=js GOARCH=wasm go build
└── wrangler.toml              # [[d1_databases]] binding = "DB"

apps/api/
└── main.go                    # Wires GORM repos; AutoMigrate on startup
```

## Design decisions

### 1. Token placement — separate `GoogleOAuthToken` table

Google OAuth tokens (`access_token`, `refresh_token`, `expiry`) live in a
dedicated `google_oauth_tokens` table, 1:1 with `users`. This keeps `User` a
pure identity record — `SELECT * FROM users` never drags secrets through
memory, and multi-provider support is a new table, not a schema change to
`User`.

### 2. Sessions — stateless cookie, no DB table

Sessions use `gorilla/sessions` encrypted cookie store. The cookie carries
**only** user identity (`user_id`, `email`, `name`, `picture`) — never Google
tokens. No `sessions` table is needed. Revocation is handled by rotating
`SESSION_SECRET` or by adding an optional `revoked_tokens` set later if
per-session revocation becomes a requirement.

### 3. Build tags — mandatory

The WASM build uses `GOOS=js GOARCH=wasm`. Under `GOOS=js`:

- `syscall/js` is available.
- `database/sql`'s native driver registration does **not** work — there is no
  POSIX syscall layer.
- `glebarez/sqlite` (built on `modernc.org/sqlite`) **does not compile** for
  `GOOS=js` (it uses `os`/`syscall` heavily).

Therefore:

- `gorm_repo.go` and its test get `//go:build !js`.
- `d1_repo.go` gets `//go:build js`.
- `repository.go` interfaces file has **no build tag** (compiles in both).
- `apps/api/main.go` calls `repository.NewGORMUserRepo(...)` — compiles because
  `GOOS != js`.
- `apps/cloudflare-deploy/main.go` calls `repository.NewD1UserRepo(...)` —
  compiles because `GOOS == js` and the GORM file is excluded.

### 4. Calendar events — local cache with per-calendar sync

`CalendarEvent` rows are a **cache/sync** of the user's Google Calendar. Reads
hit our DB; writes go to Google first, then upsert locally. Each row carries
Google sync metadata (`google_etag`, `google_updated_at`, `last_synced_at`).

A per-calendar `sync_token` (Google's `nextSyncToken`) is stored on the
`google_calendars` table so incremental syncs are efficient and one user can
sync multiple calendars independently. See [D1 Binding](d1-binding.md) for the
shim that bridges `env.DB` into Go.

### 5. IDs, audit columns, soft-delete

- **Primary keys:** UUIDv4 strings (`TEXT` in SQLite/D1). Avoids leaking row
  counts and is safe across distributed/Worker environments.
- **Audit columns:** `created_at` and `updated_at` on every table, stored as
  ISO 8601 `TEXT` and mapped to `time.Time` in Go.
- **Soft delete:** `deleted_at TEXT NULL` on all tables. Queries filter
  `deleted_at IS NULL`.
- **Timestamps:** Always ISO 8601 `TEXT`. Never `INTEGER` epoch — D1's TEXT
  affinity is the common denominator and GORM's SQLite driver serializes
  `time.Time` to ISO 8601 by default.

> **Known tension:** GORM soft-deletes via `deleted_at` while D1 uses hard
> `DELETE`. Both backends filter `deleted_at IS NULL` on reads, but D1's
> `Delete` methods do hard deletes while GORM's do soft deletes. This is a
> known design divergence to be aligned in future work.

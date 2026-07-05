# Database Layer

Documentation for the database layer of `@packages/api-core`, the shared Go
library consumed by both `@apps/api` (native local dev) and
`@apps/cloudflare-deploy` (Cloudflare Workers WASM + D1).

## Pages

| Page                            | Description                                                                       |
| ------------------------------- | --------------------------------------------------------------------------------- |
| [Architecture](architecture.md) | Two-target design, build tags, file layout, design decisions                      |
| [Models](models.md)             | The four entities: `User`, `GoogleOAuthToken`, `GoogleCalendar`, `CalendarEvent`  |
| [Repository](repository.md)     | Interfaces, sentinel errors, GORM & D1 implementations, testing                   |
| [Migrations](migrations.md)     | AutoMigrate (local) vs `wrangler d1 migrations` (D1), schema evolution discipline |
| [D1 Binding](d1-binding.md)     | JS shim that bridges `env.DB` into Go via `globalThis.__D1__`                     |

## Quick reference

- **Local dev:** GORM + `glebarez/sqlite` (pure Go). AutoMigrate on startup.
- **Workers:** D1 via `syscall/js`. SQL migrations applied with Wrangler.
- **Build tags:** `//go:build !js` (GORM) vs `//go:build js` (D1). Mandatory.
- **IDs:** UUIDv4 `TEXT` everywhere.
- **Timestamps:** ISO 8601 `TEXT`. Never `INTEGER` epoch.
- **Booleans:** `INTEGER` 0/1 in D1 SQL; Go `bool` in models (driver maps).
- **Sentinel errors:** `repository.ErrNotFound`, `repository.ErrConflict`.

# Repository Layer

Source: [`packages/api-core/repository/`](../../packages/api-core/repository/)

## Interfaces

Source: [`repository.go`](../../packages/api-core/repository/repository.go) (no build tag — compiles in both native and JS targets)

```go
type PaginationParams struct {
    Offset int
    Limit  int
}

type TimeRange struct {
    Start time.Time
    End   time.Time
}

type UserRepo interface {
    GetByID(ctx context.Context, id string) (*models.User, error)
    GetByGoogleID(ctx context.Context, googleID string) (*models.User, error)
    GetByEmail(ctx context.Context, email string) (*models.User, error)
    UpsertByGoogleID(ctx context.Context, user *models.User) (*models.User, error)
}

type TokenRepo interface {
    GetByUserID(ctx context.Context, userID string) (*models.GoogleOAuthToken, error)
    Upsert(ctx context.Context, token *models.GoogleOAuthToken) error
    Delete(ctx context.Context, userID string) error
}

type CalendarRepo interface {
    ListByUserID(ctx context.Context, userID string) ([]models.GoogleCalendar, error)
    GetByID(ctx context.Context, id string) (*models.GoogleCalendar, error)
    GetByGoogleCalID(ctx context.Context, userID, googleCalID string) (*models.GoogleCalendar, error)
    Upsert(ctx context.Context, cal *models.GoogleCalendar) error
    UpsertBatch(ctx context.Context, cals []models.GoogleCalendar) error
    UpdateSyncState(ctx context.Context, calendarID, syncToken string, lastSyncedAt time.Time) error
    SetSyncEnabled(ctx context.Context, calendarID string, enabled bool) error
    Delete(ctx context.Context, id string) error
}

type CalendarEventRepo interface {
    Upsert(ctx context.Context, event *models.CalendarEvent) error
    UpsertBatch(ctx context.Context, events []models.CalendarEvent) error
    GetByID(ctx context.Context, id string) (*models.CalendarEvent, error)
    ListByUserID(ctx context.Context, userID string, params PaginationParams) ([]models.CalendarEvent, error)
    ListByCalendarID(ctx context.Context, calendarID string, params PaginationParams) ([]models.CalendarEvent, error)
    ListByUserIDAndTimeRange(ctx context.Context, userID string, tr TimeRange) ([]models.CalendarEvent, error)
    Delete(ctx context.Context, id string) error
    DeleteByGoogleEventID(ctx context.Context, calendarID, googleEventID string) error
    DeleteStale(ctx context.Context, calendarID string, olderThan time.Time) error
}
```

## Sentinel errors

Source: [`errors.go`](../../packages/api-core/repository/errors.go)

```go
var ErrNotFound = errors.New("repository: record not found")
var ErrConflict = errors.New("repository: record conflict")
```

Both implementations map their backend-specific "not found" onto `ErrNotFound`
so handlers can `errors.Is(err, repository.ErrNotFound)` uniformly.

## Why `TokenRepo` is split from `UserRepo`

- **Least privilege:** handlers that only need identity (`/auth/me`) call
  `UserRepo`; only the token-refresh service and calendar sync touch
  `TokenRepo`.
- **Testability:** faking `TokenRepo` is trivial and doesn't require generating
  fake identity rows.
- **Future multi-provider:** adding `AppleOAuthToken`, `GitHubOAuthToken` etc.
  is a new table + repo, not a schema change to `User`.

## GORM implementation

Source: [`gorm_repo.go`](../../packages/api-core/repository/gorm_repo.go) (`//go:build !js`)

Uses `glebarez/sqlite` (pure-Go SQLite driver) so the native build has no CGO
dependency.

### Connection & migration

```go
func NewGORMDB(dsn string) (*gorm.DB, error) {
    return gorm.Open(sqlite.Open(dsn), &gorm.Config{
        Logger: logger.Default.LogMode(logger.Warn),
    })
}

func AutoMigrate(db *gorm.DB) error {
    return db.AutoMigrate(
        &models.User{},
        &models.GoogleOAuthToken{},
        &models.GoogleCalendar{},
        &models.CalendarEvent{},
    )
}
```

### Key behaviours

- **User-scoped event queries** JOIN through `google_calendars`:
  ```go
  Joins("JOIN google_calendars ON google_calendars.id = calendar_events.calendar_id").
  Where("google_calendars.user_id = ? AND calendar_events.deleted_at IS NULL", userID)
  ```
- **Token upsert** preserves `refresh_token` when the new value is empty
  (Google only sends it on first consent).
- **Calendar upsert** preserves `sync_token` and `last_synced_at` when the
  caller doesn't set them.
- `UpsertBatch` wraps each row in a transaction.

## D1 implementation

Source: [`d1_repo.go`](../../packages/api-core/repository/d1_repo.go) (`//go:build js`)

Accesses D1 via `syscall/js`. The D1 binding is injected as a `D1BindingFunc`
(set up by the [D1 shim](d1-binding.md)).

### Bridge helpers

```go
type D1BindingFunc func() js.Value

func d1Query(getD1 D1BindingFunc, sql string, args ...interface{}) (js.Value, error)
func d1Exec(getD1 D1BindingFunc, sql string, args ...interface{}) error
```

### Key behaviours

- Uses `ON CONFLICT … DO UPDATE` (no `RETURNING *` — unsupported on some D1
  versions; upserts fall back to a follow-up `SELECT` if needed).
- **Token upsert** preserves `refresh_token`:
  ```sql
  refresh_token = COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token)
  ```
- **Calendar upsert** preserves `sync_token`/`last_synced_at`:
  ```sql
  sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token),
  last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at)
  ```
- User-scoped event queries JOIN through `google_calendars`:
  ```sql
  SELECT e.* FROM calendar_events e
  JOIN google_calendars c ON c.id = e.calendar_id
  WHERE c.user_id = ? AND e.deleted_at IS NULL
  ```

## Testing

### Repository unit tests (GORM, in-memory SQLite)

Source: [`gorm_repo_test.go`](../../packages/api-core/repository/gorm_repo_test.go) (`//go:build !js`)

Tests cover:

- `TestUserRepo_UpsertByGoogleID` — insert then update by `google_id`
- `TestTokenRepo_SeparateFromUser` — token table is separate, not auto-loaded
- `TestTokenRepo_UpsertPreservesRefreshToken` — empty `refresh_token` preserves
  existing value
- `TestCalendarRepo_UpsertAndSync` — calendar upsert, sync state update,
  event batch upsert, `ListByUserID` (JOIN), `DeleteByGoogleEventID`
- `TestCalendarRepo_UpsertPreservesSyncToken` — empty `sync_token` preserves
  existing value

Run with:

```bash
cd packages/api-core && go test ./repository/ -v
```

### D1 tests

D1 cannot run in a Go unit test (no JS runtime). Test via:

```bash
npx wrangler dev --experimental-local   # uses local D1 preview
curl http://localhost:8787/api/calendar/events
```

### Integration tests

Use `httptest` + in-memory SQLite repos, mirroring the existing `main_test.go`
pattern but wiring the repos.

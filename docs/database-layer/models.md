# Models

Source: [`packages/api-core/models/models.go`](../../packages/api-core/models/models.go)

Four entities with UUID primary keys, audit columns, and soft-delete:

## User

The identity record. Never holds OAuth credentials.

```go
type User struct {
    ID        string     `gorm:"primaryKey;type:text" json:"id"`
    GoogleID  string     `gorm:"uniqueIndex;not null" json:"google_id"`
    Email     string     `gorm:"uniqueIndex;not null" json:"email"`
    Name      string     `json:"name"`
    Picture   string     `json:"picture"`
    CreatedAt time.Time  `json:"created_at"`
    UpdatedAt time.Time  `json:"updated_at"`
    DeletedAt *time.Time `gorm:"index" json:"-"`

    GoogleToken *GoogleOAuthToken `gorm:"foreignKey:UserID" json:"-"`
}
```

- `GoogleToken` is a pointer — callers opt in to loading it via
  `TokenRepo.GetByUserID`. Not auto-preloaded.
- `json:"-"` on `GoogleToken` is a backstop preventing accidental leakage into
  JSON responses or logs.

## GoogleOAuthToken

Google OAuth2 credentials. 1:1 with `User`, kept in a separate table so `User`
never carries secrets.

```go
type GoogleOAuthToken struct {
    ID           string     `gorm:"primaryKey;type:text" json:"id"`
    UserID       string     `gorm:"uniqueIndex;not null" json:"user_id"`
    AccessToken  string     `gorm:"type:text;not null" json:"-"`
    RefreshToken string     `gorm:"type:text" json:"-"`
    Expiry       time.Time  `gorm:"not null" json:"expiry"`
    TokenType    string     `gorm:"default:Bearer" json:"token_type"`
    Scope        string     `gorm:"type:text" json:"scope"`
    CreatedAt    time.Time  `json:"created_at"`
    UpdatedAt    time.Time  `json:"updated_at"`
    DeletedAt    *time.Time `gorm:"index" json:"-"`
}
```

- `AccessToken`, `RefreshToken` are `json:"-"` — never serialized.
- **Refresh-token preservation:** Google only returns `refresh_token` on first
  consent. `TokenRepo.Upsert` preserves the existing `refresh_token` when the
  new value is empty. Both GORM and D1 implementations enforce this.

## GoogleCalendar

One of the user's calendars from `/users/me/calendarList`. Each row owns its
own incremental sync cursor and a `sync_enabled` flag so users can opt out of
syncing specific calendars.

```go
type GoogleCalendar struct {
    ID           string     `gorm:"primaryKey;type:text" json:"id"`
    UserID       string     `gorm:"index;not null" json:"user_id"`
    GoogleCalID  string     `gorm:"column:google_calendar_id;uniqueIndex:idx_user_calid,priority:2;not null" json:"google_calendar_id"`
    Summary      string     `json:"summary"`
    TimeZone     string     `json:"time_zone"`
    Primary      bool       `gorm:"default:false" json:"primary"`
    AccessRole   string     `json:"access_role"`    // owner | reader | writer | freeBusyReader
    SyncEnabled  bool       `gorm:"default:true" json:"sync_enabled"`
    SyncToken    string     `gorm:"type:text" json:"sync_token"`  // Google nextSyncToken, per calendar
    LastSyncedAt *time.Time `json:"last_synced_at"`
    CreatedAt    time.Time  `json:"created_at"`
    UpdatedAt    time.Time  `json:"updated_at"`
    DeletedAt    *time.Time `gorm:"index" json:"-"`
}
```

- `GoogleCalID` has an explicit `column:google_calendar_id` GORM tag to match
  the D1 migration SQL column name (GORM would otherwise snake-case it to
  `google_cal_id`).
- **Unique keys:** `GoogleCalID` is unique **per user** — composite unique index
  `idx_user_calid` on `(UserID, GoogleCalID)`.
- `SyncToken` holds Google's `nextSyncToken` for incremental syncs — one token
  per calendar, not per user.
- Both GORM and D1 `Upsert` preserve existing `sync_token`/`last_synced_at`
  when the caller doesn't set them.

## CalendarEvent

A cached row from the user's Google Calendar.

```go
type CalendarEvent struct {
    ID              string     `gorm:"primaryKey;type:text" json:"id"`
    CalendarID      string     `gorm:"index;not null" json:"calendar_id"` // FK → google_calendars.id
    GoogleEventID   string     `gorm:"uniqueIndex:idx_cal_google,priority:2;not null" json:"google_event_id"`
    GoogleETag      string     `json:"google_etag"`
    GoogleUpdatedAt time.Time  `json:"google_updated_at"`
    LastSyncedAt    time.Time  `json:"last_synced_at"`
    Title           string     `gorm:"not null" json:"title"`
    Description     string     `gorm:"type:text" json:"description"`
    StartTime       time.Time  `gorm:"not null" json:"start_time"`
    EndTime         time.Time  `gorm:"not null" json:"end_time"`
    CreatedAt       time.Time  `json:"created_at"`
    UpdatedAt       time.Time  `json:"updated_at"`
    DeletedAt       *time.Time `gorm:"index" json:"-"`
}
```

- `GoogleEventID` is unique **per calendar** — composite unique index
  `idx_cal_google` on `(CalendarID, GoogleEventID)`.
- Events reference `google_calendars.id`, not `users.id` directly. User-scoped
  queries JOIN through `google_calendars`.
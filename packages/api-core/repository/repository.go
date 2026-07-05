package repository

import (
	"context"
	"time"

	"my-sanctuary/packages/api-core/models"
)

// PaginationParams controls list endpoint result windows.
type PaginationParams struct {
	Offset int
	Limit  int
}

// TimeRange bounds a calendar query to a [Start, End] interval.
type TimeRange struct {
	Start time.Time
	End   time.Time
}

// UserRepo — identity only. No token methods here.
type UserRepo interface {
	GetByID(ctx context.Context, id string) (*models.User, error)
	GetByGoogleID(ctx context.Context, googleID string) (*models.User, error)
	GetByEmail(ctx context.Context, email string) (*models.User, error)
	UpsertByGoogleID(ctx context.Context, user *models.User) (*models.User, error)
}

// TokenRepo — Google OAuth credentials. Separate from UserRepo so handlers
// that only need identity don't touch secrets.
type TokenRepo interface {
	GetByUserID(ctx context.Context, userID string) (*models.GoogleOAuthToken, error)
	Upsert(ctx context.Context, token *models.GoogleOAuthToken) error
	Delete(ctx context.Context, userID string) error
}

// CalendarRepo — the user's Google Calendars (from /users/me/calendarList).
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

// CalendarEventRepo — local cache of Google Calendar events.
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
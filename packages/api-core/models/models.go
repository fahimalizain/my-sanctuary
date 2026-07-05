package models

import "time"

// User is the identity record. It never holds OAuth credentials.
type User struct {
	ID        string     `gorm:"primaryKey;type:text" json:"id"`
	GoogleID  string     `gorm:"uniqueIndex;not null" json:"google_id"`
	Email     string     `gorm:"uniqueIndex;not null" json:"email"`
	Name      string     `json:"name"`
	Picture   string     `json:"picture"`
	CreatedAt time.Time  `json:"created_at"`
	UpdatedAt time.Time  `json:"updated_at"`
	DeletedAt *time.Time `gorm:"index" json:"-"`

	// HasOne — loaded explicitly via TokenRepo, not auto-preloaded.
	GoogleToken *GoogleOAuthToken `gorm:"foreignKey:UserID" json:"-"`
}

// GoogleOAuthToken stores Google OAuth2 credentials for a user.
// 1:1 with User. Kept separate so User never carries secrets.
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

// TableName overrides GORM's snake-casing which would produce
// "google_o_auth_tokens"; the D1 migration and queries use
// "google_oauth_tokens".
func (GoogleOAuthToken) TableName() string {
	return "google_oauth_tokens"
}

// GoogleCalendar is one of the user's calendars from /users/me/calendarList.
// Each row owns its own incremental sync cursor (nextSyncToken) and a
// sync_enabled flag so users can opt out of syncing specific calendars.
type GoogleCalendar struct {
	ID           string     `gorm:"primaryKey;type:text" json:"id"`
	UserID       string     `gorm:"index;not null" json:"user_id"`
	GoogleCalID  string     `gorm:"column:google_calendar_id;uniqueIndex:idx_user_calid,priority:2;not null" json:"google_calendar_id"`
	Summary      string     `json:"summary"`
	TimeZone     string     `json:"time_zone"`
	Primary      bool       `gorm:"column:is_primary;default:false" json:"primary"`
	AccessRole   string     `json:"access_role"`
	SyncEnabled  bool       `gorm:"default:true" json:"sync_enabled"`
	SyncToken    string     `gorm:"type:text" json:"sync_token"`
	LastSyncedAt *time.Time `json:"last_synced_at"`
	CreatedAt    time.Time  `json:"created_at"`
	UpdatedAt    time.Time  `json:"updated_at"`
	DeletedAt    *time.Time `gorm:"index" json:"-"`
}

// CalendarEvent is a cached row from the user's Google Calendar.
type CalendarEvent struct {
	ID              string     `gorm:"primaryKey;type:text" json:"id"`
	CalendarID      string     `gorm:"index;not null" json:"calendar_id"`
	GoogleEventID   string     `gorm:"uniqueIndex:idx_cal_google,priority:2;not null" json:"google_event_id"`
	GoogleETag      string     `json:"google_etag"`
	GoogleUpdatedAt time.Time  `json:"google_updated_at"`
	LastSyncedAt    time.Time  `json:"last_synced_at"`
	Title           string     `gorm:"not null" json:"title"`
	Description     string     `gorm:"type:text" json:"description"`
	StartTime       time.Time  `gorm:"not null" json:"start_time"`
	EndTime         time.Time  `gorm:"not null" json:"end_time"`
	Recurrence      string     `gorm:"type:text" json:"recurrence"` // JSON array of RRULE strings, empty if non-recurring
	CreatedAt       time.Time  `json:"created_at"`
	UpdatedAt       time.Time  `json:"updated_at"`
	DeletedAt       *time.Time `gorm:"index" json:"-"`
}
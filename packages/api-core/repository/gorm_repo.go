//go:build !js

package repository

import (
	"context"
	"errors"
	"time"

	"github.com/glebarez/sqlite"
	"github.com/google/uuid"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"

	"my-sanctuary/packages/api-core/models"
)

// ──────────────────────────────────────────
// Connection & Migration
// ──────────────────────────────────────────

// NewGORMDB opens a GORM connection backed by the pure-Go SQLite driver
// (glebarez/sqlite). The DSN is a file path or ":memory:".
func NewGORMDB(dsn string) (*gorm.DB, error) {
	return gorm.Open(sqlite.Open(dsn), &gorm.Config{
		Logger: logger.Default.LogMode(logger.Error),
	})
}

// AutoMigrate creates the schema for all model tables. It is additive-only:
// it creates missing tables, columns, and indexes but never drops or renames.
func AutoMigrate(db *gorm.DB) error {
	return db.AutoMigrate(
		&models.User{},
		&models.GoogleOAuthToken{},
		&models.GoogleCalendar{},
		&models.CalendarEvent{},
	)
}

// ──────────────────────────────────────────
// UserRepo (GORM)
// ──────────────────────────────────────────

type gormUserRepo struct{ db *gorm.DB }

func NewGORMUserRepo(db *gorm.DB) UserRepo { return &gormUserRepo{db} }

func (r *gormUserRepo) GetByID(ctx context.Context, id string) (*models.User, error) {
	var u models.User
	err := r.db.WithContext(ctx).Where("deleted_at IS NULL").First(&u, "id = ?", id).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &u, err
}

func (r *gormUserRepo) GetByGoogleID(ctx context.Context, googleID string) (*models.User, error) {
	var u models.User
	err := r.db.WithContext(ctx).Where("google_id = ? AND deleted_at IS NULL", googleID).First(&u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &u, err
}

func (r *gormUserRepo) GetByEmail(ctx context.Context, email string) (*models.User, error) {
	var u models.User
	err := r.db.WithContext(ctx).Where("email = ? AND deleted_at IS NULL", email).First(&u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &u, err
}

func (r *gormUserRepo) UpsertByGoogleID(ctx context.Context, user *models.User) (*models.User, error) {
	existing, err := r.GetByGoogleID(ctx, user.GoogleID)
	if err != nil && !errors.Is(err, ErrNotFound) {
		return nil, err
	}
	if existing != nil {
		user.ID = existing.ID
		user.CreatedAt = existing.CreatedAt
		user.UpdatedAt = time.Now()
		err = r.db.WithContext(ctx).Model(&models.User{}).Where("id = ?", existing.ID).Updates(map[string]interface{}{
			"email":      user.Email,
			"name":       user.Name,
			"picture":    user.Picture,
			"updated_at": user.UpdatedAt,
		}).Error
		return user, err
	}
	user.ID = uuid.NewString()
	user.CreatedAt = time.Now()
	user.UpdatedAt = time.Now()
	if err := r.db.WithContext(ctx).Create(user).Error; err != nil {
		return nil, err
	}
	return user, nil
}

// ──────────────────────────────────────────
// TokenRepo (GORM)
// ──────────────────────────────────────────

type gormTokenRepo struct{ db *gorm.DB }

func NewGORMTokenRepo(db *gorm.DB) TokenRepo { return &gormTokenRepo{db} }

func (r *gormTokenRepo) GetByUserID(ctx context.Context, userID string) (*models.GoogleOAuthToken, error) {
	var t models.GoogleOAuthToken
	err := r.db.WithContext(ctx).Where("user_id = ? AND deleted_at IS NULL", userID).First(&t).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &t, err
}

func (r *gormTokenRepo) Upsert(ctx context.Context, token *models.GoogleOAuthToken) error {
	var existing models.GoogleOAuthToken
	err := r.db.WithContext(ctx).Where("user_id = ?", token.UserID).First(&existing).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		token.ID = uuid.NewString()
		token.CreatedAt = time.Now()
		token.UpdatedAt = time.Now()
		return r.db.WithContext(ctx).Create(token).Error
	}
	if err != nil {
		return err
	}
	token.ID = existing.ID
	token.CreatedAt = existing.CreatedAt
	token.UpdatedAt = time.Now()
	// Google only returns refresh_token on first consent; preserve the
	// existing value when the caller doesn't supply one.
	if token.RefreshToken == "" {
		token.RefreshToken = existing.RefreshToken
	}
	return r.db.WithContext(ctx).Model(&models.GoogleOAuthToken{}).Where("id = ?", existing.ID).Updates(map[string]interface{}{
		"access_token":  token.AccessToken,
		"refresh_token": token.RefreshToken,
		"expiry":        token.Expiry,
		"scope":         token.Scope,
		"token_type":    token.TokenType,
		"updated_at":    token.UpdatedAt,
	}).Error
}

func (r *gormTokenRepo) Delete(ctx context.Context, userID string) error {
	return r.db.WithContext(ctx).Where("user_id = ?", userID).Delete(&models.GoogleOAuthToken{}).Error
}

// ──────────────────────────────────────────
// CalendarRepo (GORM)
// ──────────────────────────────────────────

type gormCalendarRepo struct{ db *gorm.DB }

func NewGORMCalendarRepo(db *gorm.DB) CalendarRepo { return &gormCalendarRepo{db} }

func (r *gormCalendarRepo) ListByUserID(ctx context.Context, userID string) ([]models.GoogleCalendar, error) {
	var cals []models.GoogleCalendar
	err := r.db.WithContext(ctx).
		Where("user_id = ? AND deleted_at IS NULL", userID).
		Order("\"primary\" DESC, summary ASC").
		Find(&cals).Error
	return cals, err
}

func (r *gormCalendarRepo) GetByID(ctx context.Context, id string) (*models.GoogleCalendar, error) {
	var c models.GoogleCalendar
	err := r.db.WithContext(ctx).Where("id = ? AND deleted_at IS NULL", id).First(&c).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &c, err
}

func (r *gormCalendarRepo) GetByGoogleCalID(ctx context.Context, userID, googleCalID string) (*models.GoogleCalendar, error) {
	var c models.GoogleCalendar
	err := r.db.WithContext(ctx).
		Where("user_id = ? AND google_calendar_id = ? AND deleted_at IS NULL", userID, googleCalID).
		First(&c).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &c, err
}

func (r *gormCalendarRepo) Upsert(ctx context.Context, cal *models.GoogleCalendar) error {
	existing, err := r.GetByGoogleCalID(ctx, cal.UserID, cal.GoogleCalID)
	if err != nil && !errors.Is(err, ErrNotFound) {
		return err
	}
	now := time.Now()
	if existing == nil {
		cal.ID = uuid.NewString()
		cal.CreatedAt = now
		cal.UpdatedAt = now
		return r.db.WithContext(ctx).Create(cal).Error
	}
	cal.ID = existing.ID
	cal.CreatedAt = existing.CreatedAt
	cal.UpdatedAt = now
	if cal.SyncToken == "" {
		cal.SyncToken = existing.SyncToken
	}
	if cal.LastSyncedAt == nil {
		cal.LastSyncedAt = existing.LastSyncedAt
	}
	return r.db.WithContext(ctx).Model(&models.GoogleCalendar{}).Where("id = ?", existing.ID).Updates(map[string]interface{}{
		"summary":        cal.Summary,
		"time_zone":       cal.TimeZone,
		"primary":         cal.Primary,
		"access_role":     cal.AccessRole,
		"sync_enabled":    cal.SyncEnabled,
		"sync_token":      cal.SyncToken,
		"last_synced_at": cal.LastSyncedAt,
		"updated_at":     cal.UpdatedAt,
	}).Error
}

func (r *gormCalendarRepo) UpsertBatch(ctx context.Context, cals []models.GoogleCalendar) error {
	return r.db.WithContext(ctx).Transaction(func(tx *gorm.DB) error {
		for i := range cals {
			if err := (&gormCalendarRepo{tx}).Upsert(ctx, &cals[i]); err != nil {
				return err
			}
		}
		return nil
	})
}

func (r *gormCalendarRepo) UpdateSyncState(ctx context.Context, calendarID, syncToken string, lastSyncedAt time.Time) error {
	return r.db.WithContext(ctx).Model(&models.GoogleCalendar{}).Where("id = ?", calendarID).
		Updates(map[string]interface{}{
			"sync_token":     syncToken,
			"last_synced_at": lastSyncedAt,
			"updated_at":     time.Now(),
		}).Error
}

func (r *gormCalendarRepo) SetSyncEnabled(ctx context.Context, calendarID string, enabled bool) error {
	return r.db.WithContext(ctx).Model(&models.GoogleCalendar{}).Where("id = ?", calendarID).
		Updates(map[string]interface{}{
			"sync_enabled": enabled,
			"updated_at":   time.Now(),
		}).Error
}

func (r *gormCalendarRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Where("id = ?", id).Delete(&models.GoogleCalendar{}).Error
}

// ──────────────────────────────────────────
// CalendarEventRepo (GORM)
// ──────────────────────────────────────────

type gormCalendarEventRepo struct{ db *gorm.DB }

func NewGORMCalendarEventRepo(db *gorm.DB) CalendarEventRepo {
	return &gormCalendarEventRepo{db}
}

func (r *gormCalendarEventRepo) Upsert(ctx context.Context, event *models.CalendarEvent) error {
	var existing models.CalendarEvent
	err := r.db.WithContext(ctx).
		Where("calendar_id = ? AND google_event_id = ?", event.CalendarID, event.GoogleEventID).
		First(&existing).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		event.ID = uuid.NewString()
		event.CreatedAt = time.Now()
		event.UpdatedAt = time.Now()
		return r.db.WithContext(ctx).Create(event).Error
	}
	if err != nil {
		return err
	}
	event.ID = existing.ID
	event.CreatedAt = existing.CreatedAt
	event.UpdatedAt = time.Now()
	return r.db.WithContext(ctx).Model(&models.CalendarEvent{}).Where("id = ?", existing.ID).Updates(map[string]interface{}{
		"google_etag":       event.GoogleETag,
		"google_updated_at": event.GoogleUpdatedAt,
		"last_synced_at":    event.LastSyncedAt,
		"title":             event.Title,
		"description":       event.Description,
		"start_time":        event.StartTime,
		"end_time":          event.EndTime,
		"updated_at":        event.UpdatedAt,
	}).Error
}

func (r *gormCalendarEventRepo) UpsertBatch(ctx context.Context, events []models.CalendarEvent) error {
	return r.db.WithContext(ctx).Transaction(func(tx *gorm.DB) error {
		for i := range events {
			if err := (&gormCalendarEventRepo{tx}).Upsert(ctx, &events[i]); err != nil {
				return err
			}
		}
		return nil
	})
}

func (r *gormCalendarEventRepo) GetByID(ctx context.Context, id string) (*models.CalendarEvent, error) {
	var e models.CalendarEvent
	err := r.db.WithContext(ctx).Where("id = ? AND deleted_at IS NULL", id).First(&e).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, ErrNotFound
	}
	return &e, err
}

func (r *gormCalendarEventRepo) ListByUserID(ctx context.Context, userID string, p PaginationParams) ([]models.CalendarEvent, error) {
	var events []models.CalendarEvent
	err := r.db.WithContext(ctx).
		Joins("JOIN google_calendars ON google_calendars.id = calendar_events.calendar_id").
		Where("google_calendars.user_id = ? AND calendar_events.deleted_at IS NULL", userID).
		Order("calendar_events.start_time ASC").
		Offset(p.Offset).Limit(p.Limit).
		Find(&events).Error
	return events, err
}

func (r *gormCalendarEventRepo) ListByCalendarID(ctx context.Context, calendarID string, p PaginationParams) ([]models.CalendarEvent, error) {
	var events []models.CalendarEvent
	err := r.db.WithContext(ctx).
		Where("calendar_id = ? AND deleted_at IS NULL", calendarID).
		Order("start_time ASC").
		Offset(p.Offset).Limit(p.Limit).
		Find(&events).Error
	return events, err
}

func (r *gormCalendarEventRepo) ListByUserIDAndTimeRange(ctx context.Context, userID string, tr TimeRange) ([]models.CalendarEvent, error) {
	var events []models.CalendarEvent
	err := r.db.WithContext(ctx).
		Joins("JOIN google_calendars ON google_calendars.id = calendar_events.calendar_id").
		Where("google_calendars.user_id = ? AND calendar_events.deleted_at IS NULL AND start_time >= ? AND end_time <= ?", userID, tr.Start, tr.End).
		Order("calendar_events.start_time ASC").
		Find(&events).Error
	return events, err
}

func (r *gormCalendarEventRepo) Delete(ctx context.Context, id string) error {
	return r.db.WithContext(ctx).Where("id = ?", id).Delete(&models.CalendarEvent{}).Error
}

func (r *gormCalendarEventRepo) DeleteByGoogleEventID(ctx context.Context, calendarID, googleEventID string) error {
	return r.db.WithContext(ctx).
		Where("calendar_id = ? AND google_event_id = ?", calendarID, googleEventID).
		Delete(&models.CalendarEvent{}).Error
}

func (r *gormCalendarEventRepo) DeleteStale(ctx context.Context, calendarID string, olderThan time.Time) error {
	return r.db.WithContext(ctx).
		Where("calendar_id = ? AND last_synced_at < ?", calendarID, olderThan).
		Delete(&models.CalendarEvent{}).Error
}
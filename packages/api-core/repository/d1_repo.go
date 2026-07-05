//go:build js

package repository

import (
	"context"
	"fmt"
	"syscall/js"
	"time"

	"github.com/google/uuid"

	"my-sanctuary/packages/api-core/models"
)

// D1BindingFunc returns the D1 js.Value. Injected at startup by the
// cloudflare-deploy app (see d1_shim.js).
type D1BindingFunc func() js.Value

// ──────────────────────────────────────────
// JS bridge helpers
// ──────────────────────────────────────────

// awaitPromise resolves a JS Promise synchronously via then/catch channels.
// D1's prepare().bind().all() and prepare().bind().run() return Promises.
func awaitPromise(promiseVal js.Value) (js.Value, error) {
	resultCh := make(chan js.Value)
	errCh := make(chan error)
	var then, catch js.Func
	then = js.FuncOf(func(_ js.Value, args []js.Value) any {
		defer then.Release()
		resultCh <- args[0]
		return js.Undefined()
	})
	catch = js.FuncOf(func(_ js.Value, args []js.Value) any {
		defer catch.Release()
		errCh <- fmt.Errorf("d1: promise rejected: %s", args[0].Call("toString").String())
		return js.Undefined()
	})
	promiseVal.Call("then", then).Call("catch", catch)
	select {
	case result := <-resultCh:
		return result, nil
	case err := <-errCh:
		return js.Value{}, err
	}
}

func d1Query(getD1 D1BindingFunc, sql string, args ...interface{}) (js.Value, error) {
	d1 := getD1()
	if d1.IsUndefined() || d1.IsNull() {
		return js.Value{}, fmt.Errorf("d1: binding not initialized")
	}
	promise := d1.Call("prepare", sql).Call("bind", args...).Call("all")
	result, err := awaitPromise(promise)
	if err != nil {
		return js.Value{}, err
	}
	if !result.Get("success").Bool() {
		msg := "d1: query failed"
		if e := result.Get("error"); !e.IsUndefined() && !e.IsNull() {
			msg = e.String()
		}
		return js.Value{}, fmt.Errorf(msg)
	}
	return result, nil
}

func d1Exec(getD1 D1BindingFunc, sql string, args ...interface{}) error {
	d1 := getD1()
	if d1.IsUndefined() || d1.IsNull() {
		return fmt.Errorf("d1: binding not initialized")
	}
	promise := d1.Call("prepare", sql).Call("bind", args...).Call("run")
	result, err := awaitPromise(promise)
	if err != nil {
		return err
	}
	if !result.Get("success").Bool() {
		msg := "d1: exec failed"
		if e := result.Get("error"); !e.IsUndefined() && !e.IsNull() {
			msg = e.String()
		}
		return fmt.Errorf(msg)
	}
	return nil
}

func firstRow(result js.Value) (js.Value, bool) {
	results := result.Get("results")
	if results.IsUndefined() || results.IsNull() || results.Get("length").Int() == 0 {
		return js.Value{}, false
	}
	return results.Index(0), true
}

// ──────────────────────────────────────────
// Type conversion helpers
// ──────────────────────────────────────────

func jsString(v js.Value) string {
	if v.IsUndefined() || v.IsNull() {
		return ""
	}
	return v.String()
}

func jsTimeOrZero(v js.Value) time.Time {
	t := jsTimePtr(v)
	if t == nil {
		return time.Time{}
	}
	return *t
}

func jsTimePtr(v js.Value) *time.Time {
	if v.IsUndefined() || v.IsNull() {
		return nil
	}
	switch v.Type() {
	case js.TypeString:
		t, err := time.Parse(time.RFC3339, v.String())
		if err != nil {
			return nil
		}
		return &t
	case js.TypeNumber:
		t := time.Unix(int64(v.Int()), 0)
		return &t
	}
	return nil
}

// ──────────────────────────────────────────
// Row scanners
// ──────────────────────────────────────────

func scanUser(row js.Value) models.User {
	return models.User{
		ID:        jsString(row.Get("id")),
		GoogleID:  jsString(row.Get("google_id")),
		Email:     jsString(row.Get("email")),
		Name:      jsString(row.Get("name")),
		Picture:   jsString(row.Get("picture")),
		CreatedAt: jsTimeOrZero(row.Get("created_at")),
		UpdatedAt: jsTimeOrZero(row.Get("updated_at")),
		DeletedAt: jsTimePtr(row.Get("deleted_at")),
	}
}

func scanToken(row js.Value) models.GoogleOAuthToken {
	return models.GoogleOAuthToken{
		ID:           jsString(row.Get("id")),
		UserID:       jsString(row.Get("user_id")),
		AccessToken:  jsString(row.Get("access_token")),
		RefreshToken: jsString(row.Get("refresh_token")),
		Expiry:       jsTimeOrZero(row.Get("expiry")),
		TokenType:    jsString(row.Get("token_type")),
		Scope:        jsString(row.Get("scope")),
		CreatedAt:    jsTimeOrZero(row.Get("created_at")),
		UpdatedAt:    jsTimeOrZero(row.Get("updated_at")),
		DeletedAt:    jsTimePtr(row.Get("deleted_at")),
	}
}

func scanCalendar(row js.Value) models.GoogleCalendar {
	// D1 returns booleans as INTEGER 0/1, not JS boolean.
	primary := false
	if p := row.Get("is_primary"); !p.IsUndefined() && !p.IsNull() {
		primary = p.Int() != 0
	}
	syncEnabled := true
	if s := row.Get("sync_enabled"); !s.IsUndefined() && !s.IsNull() {
		syncEnabled = s.Int() != 0
	}
	return models.GoogleCalendar{
		ID:           jsString(row.Get("id")),
		UserID:       jsString(row.Get("user_id")),
		GoogleCalID:  jsString(row.Get("google_calendar_id")),
		Summary:      jsString(row.Get("summary")),
		TimeZone:     jsString(row.Get("time_zone")),
		Primary:      primary,
		AccessRole:   jsString(row.Get("access_role")),
		SyncEnabled:  syncEnabled,
		SyncToken:    jsString(row.Get("sync_token")),
		LastSyncedAt: jsTimePtr(row.Get("last_synced_at")),
		CreatedAt:    jsTimeOrZero(row.Get("created_at")),
		UpdatedAt:    jsTimeOrZero(row.Get("updated_at")),
		DeletedAt:    jsTimePtr(row.Get("deleted_at")),
	}
}

func scanCalendarEvent(row js.Value) models.CalendarEvent {
	return models.CalendarEvent{
		ID:              jsString(row.Get("id")),
		CalendarID:      jsString(row.Get("calendar_id")),
		GoogleEventID:   jsString(row.Get("google_event_id")),
		GoogleETag:      jsString(row.Get("google_etag")),
		GoogleUpdatedAt: jsTimeOrZero(row.Get("google_updated_at")),
		LastSyncedAt:    jsTimeOrZero(row.Get("last_synced_at")),
		Title:           jsString(row.Get("title")),
		Description:     jsString(row.Get("description")),
		StartTime:       jsTimeOrZero(row.Get("start_time")),
		EndTime:         jsTimeOrZero(row.Get("end_time")),
		CreatedAt:       jsTimeOrZero(row.Get("created_at")),
		UpdatedAt:       jsTimeOrZero(row.Get("updated_at")),
		DeletedAt:       jsTimePtr(row.Get("deleted_at")),
	}
}

func scanCalendarList(result js.Value) []models.GoogleCalendar {
	results := result.Get("results")
	if results.IsUndefined() || results.IsNull() {
		return nil
	}
	n := results.Get("length").Int()
	out := make([]models.GoogleCalendar, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, scanCalendar(results.Index(i)))
	}
	return out
}

func scanEventList(result js.Value) []models.CalendarEvent {
	results := result.Get("results")
	if results.IsUndefined() || results.IsNull() {
		return nil
	}
	n := results.Get("length").Int()
	out := make([]models.CalendarEvent, 0, n)
	for i := 0; i < n; i++ {
		out = append(out, scanCalendarEvent(results.Index(i)))
	}
	return out
}

// ──────────────────────────────────────────
// UserRepo (D1)
// ──────────────────────────────────────────

type d1UserRepo struct{ getD1 D1BindingFunc }

func NewD1UserRepo(getD1 D1BindingFunc) UserRepo { return &d1UserRepo{getD1} }

func (r *d1UserRepo) GetByID(ctx context.Context, id string) (*models.User, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM users WHERE id = ? AND deleted_at IS NULL", id)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	u := scanUser(row)
	return &u, nil
}

func (r *d1UserRepo) GetByGoogleID(ctx context.Context, googleID string) (*models.User, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM users WHERE google_id = ? AND deleted_at IS NULL", googleID)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	u := scanUser(row)
	return &u, nil
}

func (r *d1UserRepo) GetByEmail(ctx context.Context, email string) (*models.User, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM users WHERE email = ? AND deleted_at IS NULL", email)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	u := scanUser(row)
	return &u, nil
}

func (r *d1UserRepo) UpsertByGoogleID(ctx context.Context, user *models.User) (*models.User, error) {
	now := time.Now().UTC().Format(time.RFC3339)
	sql := `INSERT INTO users (id, google_id, email, name, picture, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(google_id) DO UPDATE SET
			email = excluded.email,
			name = excluded.name,
			picture = excluded.picture,
			updated_at = excluded.updated_at
		RETURNING *`
	newID := uuid.NewString()
	res, err := d1Query(r.getD1, sql, newID, user.GoogleID, user.Email, user.Name, user.Picture, now, now)
	if err != nil {
		existing, gerr := r.GetByGoogleID(ctx, user.GoogleID)
		if gerr != nil {
			return nil, err
		}
		user.ID = existing.ID
		uerr := d1Exec(r.getD1,
			"UPDATE users SET email = ?, name = ?, picture = ?, updated_at = ? WHERE id = ?",
			user.Email, user.Name, user.Picture, now, existing.ID)
		if uerr != nil {
			return nil, uerr
		}
		return user, nil
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, fmt.Errorf("d1: upsert returned no rows")
	}
	u := scanUser(row)
	return &u, nil
}

// ──────────────────────────────────────────
// TokenRepo (D1)
// ──────────────────────────────────────────

type d1TokenRepo struct{ getD1 D1BindingFunc }

func NewD1TokenRepo(getD1 D1BindingFunc) TokenRepo { return &d1TokenRepo{getD1} }

func (r *d1TokenRepo) GetByUserID(ctx context.Context, userID string) (*models.GoogleOAuthToken, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM google_oauth_tokens WHERE user_id = ? AND deleted_at IS NULL", userID)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	t := scanToken(row)
	return &t, nil
}

func (r *d1TokenRepo) Upsert(ctx context.Context, token *models.GoogleOAuthToken) error {
	now := time.Now().UTC().Format(time.RFC3339)
	expiry := token.Expiry.UTC().Format(time.RFC3339)
	sql := `INSERT INTO google_oauth_tokens (id, user_id, access_token, refresh_token, expiry, token_type, scope, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(user_id) DO UPDATE SET
			access_token = excluded.access_token,
			refresh_token = COALESCE(NULLIF(excluded.refresh_token, ''), google_oauth_tokens.refresh_token),
			expiry = excluded.expiry,
			scope = excluded.scope,
			token_type = excluded.token_type,
			updated_at = excluded.updated_at`
	return d1Exec(r.getD1, sql,
		uuid.NewString(), token.UserID, token.AccessToken, token.RefreshToken,
		expiry, token.TokenType, token.Scope, now, now,
	)
}

func (r *d1TokenRepo) Delete(ctx context.Context, userID string) error {
	return d1Exec(r.getD1, "DELETE FROM google_oauth_tokens WHERE user_id = ?", userID)
}

// ──────────────────────────────────────────
// CalendarRepo (D1)
// ──────────────────────────────────────────

type d1CalendarRepo struct{ getD1 D1BindingFunc }

func NewD1CalendarRepo(getD1 D1BindingFunc) CalendarRepo { return &d1CalendarRepo{getD1} }

func (r *d1CalendarRepo) ListByUserID(ctx context.Context, userID string) ([]models.GoogleCalendar, error) {
	res, err := d1Query(r.getD1,
		"SELECT * FROM google_calendars WHERE user_id = ? AND deleted_at IS NULL ORDER BY is_primary DESC, summary ASC",
		userID)
	if err != nil {
		return nil, err
	}
	return scanCalendarList(res), nil
}

func (r *d1CalendarRepo) GetByID(ctx context.Context, id string) (*models.GoogleCalendar, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM google_calendars WHERE id = ? AND deleted_at IS NULL", id)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	c := scanCalendar(row)
	return &c, nil
}

func (r *d1CalendarRepo) GetByGoogleCalID(ctx context.Context, userID, googleCalID string) (*models.GoogleCalendar, error) {
	res, err := d1Query(r.getD1,
		"SELECT * FROM google_calendars WHERE user_id = ? AND google_calendar_id = ? AND deleted_at IS NULL",
		userID, googleCalID)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	c := scanCalendar(row)
	return &c, nil
}

func (r *d1CalendarRepo) Upsert(ctx context.Context, cal *models.GoogleCalendar) error {
	now := time.Now().UTC().Format(time.RFC3339)
	var lastSynced string
	if cal.LastSyncedAt != nil {
		lastSynced = cal.LastSyncedAt.UTC().Format(time.RFC3339)
	}
	sql := `INSERT INTO google_calendars
		(id, user_id, google_calendar_id, summary, time_zone, is_primary, access_role, sync_enabled, sync_token, last_synced_at, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(user_id, google_calendar_id) DO UPDATE SET
			summary = excluded.summary,
			time_zone = excluded.time_zone,
			is_primary = excluded.is_primary,
			access_role = excluded.access_role,
			sync_enabled = excluded.sync_enabled,
			sync_token = COALESCE(NULLIF(excluded.sync_token, ''), google_calendars.sync_token),
			last_synced_at = COALESCE(NULLIF(excluded.last_synced_at, ''), google_calendars.last_synced_at),
			updated_at = excluded.updated_at`
	return d1Exec(r.getD1, sql,
		uuid.NewString(), cal.UserID, cal.GoogleCalID, cal.Summary, cal.TimeZone,
		cal.Primary, cal.AccessRole, cal.SyncEnabled, cal.SyncToken, lastSynced, now, now,
	)
}

func (r *d1CalendarRepo) UpsertBatch(ctx context.Context, cals []models.GoogleCalendar) error {
	for i := range cals {
		if err := r.Upsert(ctx, &cals[i]); err != nil {
			return err
		}
	}
	return nil
}

func (r *d1CalendarRepo) UpdateSyncState(ctx context.Context, calendarID, syncToken string, lastSyncedAt time.Time) error {
	now := time.Now().UTC().Format(time.RFC3339)
	return d1Exec(r.getD1,
		"UPDATE google_calendars SET sync_token = ?, last_synced_at = ?, updated_at = ? WHERE id = ?",
		syncToken, lastSyncedAt.UTC().Format(time.RFC3339), now, calendarID)
}

func (r *d1CalendarRepo) SetSyncEnabled(ctx context.Context, calendarID string, enabled bool) error {
	now := time.Now().UTC().Format(time.RFC3339)
	return d1Exec(r.getD1,
		"UPDATE google_calendars SET sync_enabled = ?, updated_at = ? WHERE id = ?",
		enabled, now, calendarID)
}

func (r *d1CalendarRepo) Delete(ctx context.Context, id string) error {
	return d1Exec(r.getD1, "DELETE FROM google_calendars WHERE id = ?", id)
}

// ──────────────────────────────────────────
// CalendarEventRepo (D1)
// ──────────────────────────────────────────

type d1CalendarEventRepo struct{ getD1 D1BindingFunc }

func NewD1CalendarEventRepo(getD1 D1BindingFunc) CalendarEventRepo {
	return &d1CalendarEventRepo{getD1}
}

func (r *d1CalendarEventRepo) Upsert(ctx context.Context, event *models.CalendarEvent) error {
	now := time.Now().UTC().Format(time.RFC3339)
	sql := `INSERT INTO calendar_events
		(id, calendar_id, google_event_id, google_etag, google_updated_at, last_synced_at, title, description, start_time, end_time, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(calendar_id, google_event_id) DO UPDATE SET
			google_etag = excluded.google_etag,
			google_updated_at = excluded.google_updated_at,
			last_synced_at = excluded.last_synced_at,
			title = excluded.title,
			description = excluded.description,
			start_time = excluded.start_time,
			end_time = excluded.end_time,
			updated_at = excluded.updated_at`
	return d1Exec(r.getD1, sql,
		uuid.NewString(), event.CalendarID, event.GoogleEventID, event.GoogleETag,
		event.GoogleUpdatedAt.UTC().Format(time.RFC3339),
		event.LastSyncedAt.UTC().Format(time.RFC3339),
		event.Title, event.Description,
		event.StartTime.UTC().Format(time.RFC3339),
		event.EndTime.UTC().Format(time.RFC3339),
		now, now,
	)
}

func (r *d1CalendarEventRepo) UpsertBatch(ctx context.Context, events []models.CalendarEvent) error {
	for i := range events {
		if err := r.Upsert(ctx, &events[i]); err != nil {
			return err
		}
	}
	return nil
}

func (r *d1CalendarEventRepo) GetByID(ctx context.Context, id string) (*models.CalendarEvent, error) {
	res, err := d1Query(r.getD1, "SELECT * FROM calendar_events WHERE id = ? AND deleted_at IS NULL", id)
	if err != nil {
		return nil, err
	}
	row, ok := firstRow(res)
	if !ok {
		return nil, ErrNotFound
	}
	e := scanCalendarEvent(row)
	return &e, nil
}

func (r *d1CalendarEventRepo) ListByUserID(ctx context.Context, userID string, p PaginationParams) ([]models.CalendarEvent, error) {
	res, err := d1Query(r.getD1,
		`SELECT e.* FROM calendar_events e
		 JOIN google_calendars c ON c.id = e.calendar_id
		 WHERE c.user_id = ? AND e.deleted_at IS NULL
		 ORDER BY e.start_time ASC LIMIT ? OFFSET ?`,
		userID, p.Limit, p.Offset)
	if err != nil {
		return nil, err
	}
	return scanEventList(res), nil
}

func (r *d1CalendarEventRepo) ListByCalendarID(ctx context.Context, calendarID string, p PaginationParams) ([]models.CalendarEvent, error) {
	res, err := d1Query(r.getD1,
		"SELECT * FROM calendar_events WHERE calendar_id = ? AND deleted_at IS NULL ORDER BY start_time ASC LIMIT ? OFFSET ?",
		calendarID, p.Limit, p.Offset)
	if err != nil {
		return nil, err
	}
	return scanEventList(res), nil
}

func (r *d1CalendarEventRepo) ListByUserIDAndTimeRange(ctx context.Context, userID string, tr TimeRange) ([]models.CalendarEvent, error) {
	res, err := d1Query(r.getD1,
		`SELECT e.* FROM calendar_events e
		 JOIN google_calendars c ON c.id = e.calendar_id
		 WHERE c.user_id = ? AND e.deleted_at IS NULL AND e.start_time >= ? AND e.end_time <= ?
		 ORDER BY e.start_time ASC`,
		userID, tr.Start.UTC().Format(time.RFC3339), tr.End.UTC().Format(time.RFC3339))
	if err != nil {
		return nil, err
	}
	return scanEventList(res), nil
}

func (r *d1CalendarEventRepo) Delete(ctx context.Context, id string) error {
	return d1Exec(r.getD1, "DELETE FROM calendar_events WHERE id = ?", id)
}

func (r *d1CalendarEventRepo) DeleteByGoogleEventID(ctx context.Context, calendarID, googleEventID string) error {
	return d1Exec(r.getD1,
		"DELETE FROM calendar_events WHERE calendar_id = ? AND google_event_id = ?",
		calendarID, googleEventID)
}

func (r *d1CalendarEventRepo) DeleteStale(ctx context.Context, calendarID string, olderThan time.Time) error {
	return d1Exec(r.getD1,
		"DELETE FROM calendar_events WHERE calendar_id = ? AND last_synced_at < ?",
		calendarID, olderThan.UTC().Format(time.RFC3339))
}
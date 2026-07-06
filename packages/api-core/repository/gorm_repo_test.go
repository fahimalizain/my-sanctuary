//go:build !js

package repository

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"gorm.io/gorm"

	"my-sanctuary/packages/api-core/models"
)

func newTestDB(t *testing.T) *gorm.DB {
	t.Helper()
	db, err := NewGORMDB(":memory:")
	if err != nil {
		t.Fatalf("failed to open db: %v", err)
	}
	if err := AutoMigrate(db); err != nil {
		t.Fatalf("failed to migrate: %v", err)
	}
	return db
}

func TestUserRepo_UpsertByGoogleID(t *testing.T) {
	db := newTestDB(t)
	repo := NewGORMUserRepo(db)
	ctx := context.Background()

	user := &models.User{GoogleID: "g-1", Email: "a@b.com", Name: "First"}
	created, err := repo.UpsertByGoogleID(ctx, user)
	if err != nil {
		t.Fatal(err)
	}
	if created.ID == "" {
		t.Fatal("expected UUID")
	}

	user.Name = "Second"
	updated, err := repo.UpsertByGoogleID(ctx, user)
	if err != nil {
		t.Fatal(err)
	}
	if updated.ID != created.ID {
		t.Fatalf("expected same ID, got %s vs %s", created.ID, updated.ID)
	}
	if updated.Name != "Second" {
		t.Fatalf("expected name update, got %s", updated.Name)
	}
}

func TestTokenRepo_SeparateFromUser(t *testing.T) {
	db := newTestDB(t)
	users := NewGORMUserRepo(db)
	tokens := NewGORMTokenRepo(db)
	ctx := context.Background()

	user, _ := users.UpsertByGoogleID(ctx, &models.User{GoogleID: "g-1", Email: "a@b.com", Name: "N"})

	err := tokens.Upsert(ctx, &models.GoogleOAuthToken{
		UserID: user.ID, AccessToken: "at", RefreshToken: "rt",
		Expiry: time.Now().Add(time.Hour), TokenType: "Bearer",
	})
	if err != nil {
		t.Fatal(err)
	}

	fetched, err := users.GetByID(ctx, user.ID)
	if err != nil {
		t.Fatal(err)
	}
	if fetched.GoogleToken != nil {
		t.Fatal("User should not auto-load GoogleToken")
	}

	tok, err := tokens.GetByUserID(ctx, user.ID)
	if err != nil {
		t.Fatal(err)
	}
	if tok.AccessToken != "at" {
		t.Fatalf("expected at, got %s", tok.AccessToken)
	}
}

func TestTokenRepo_UpsertPreservesRefreshToken(t *testing.T) {
	db := newTestDB(t)
	users := NewGORMUserRepo(db)
	tokens := NewGORMTokenRepo(db)
	ctx := context.Background()

	user, _ := users.UpsertByGoogleID(ctx, &models.User{GoogleID: "g-1", Email: "a@b.com", Name: "N"})

	if err := tokens.Upsert(ctx, &models.GoogleOAuthToken{
		UserID: user.ID, AccessToken: "at1", RefreshToken: "rt1",
		Expiry: time.Now().Add(time.Hour), TokenType: "Bearer",
	}); err != nil {
		t.Fatal(err)
	}

	if err := tokens.Upsert(ctx, &models.GoogleOAuthToken{
		UserID: user.ID, AccessToken: "at2", RefreshToken: "",
		Expiry: time.Now().Add(2 * time.Hour), TokenType: "Bearer",
	}); err != nil {
		t.Fatal(err)
	}

	tok, _ := tokens.GetByUserID(ctx, user.ID)
	if tok.AccessToken != "at2" {
		t.Fatalf("expected at2, got %s", tok.AccessToken)
	}
	if tok.RefreshToken != "rt1" {
		t.Fatalf("expected rt1 preserved, got %s", tok.RefreshToken)
	}
}

func TestCalendarRepo_UpsertAndSync(t *testing.T) {
	db := newTestDB(t)
	cals := NewGORMCalendarRepo(db)
	events := NewGORMCalendarEventRepo(db)
	ctx := context.Background()
	userID := uuid.NewString()

	cal := &models.GoogleCalendar{
		UserID: userID, GoogleCalID: "work@x.com", Summary: "Work",
		Primary: false, AccessRole: "writer", SyncEnabled: true,
	}
	if err := cals.Upsert(ctx, cal); err != nil {
		t.Fatal(err)
	}

	if err := cals.UpdateSyncState(ctx, cal.ID, "token-1", time.Now()); err != nil {
		t.Fatal(err)
	}
	got, _ := cals.GetByID(ctx, cal.ID)
	if got.SyncToken != "token-1" {
		t.Fatalf("expected token-1, got %s", got.SyncToken)
	}

	evs := []models.CalendarEvent{
		{CalendarID: cal.ID, GoogleEventID: "e1", Title: "One", StartTime: time.Now(), EndTime: time.Now().Add(time.Hour), LastSyncedAt: time.Now()},
		{CalendarID: cal.ID, GoogleEventID: "e2", Title: "Two", StartTime: time.Now(), EndTime: time.Now().Add(time.Hour), LastSyncedAt: time.Now()},
	}
	if err := events.UpsertBatch(ctx, evs); err != nil {
		t.Fatal(err)
	}

	gotEvs, err := events.ListByUserID(ctx, userID, PaginationParams{Limit: 10})
	if err != nil {
		t.Fatal(err)
	}
	if len(gotEvs) != 2 {
		t.Fatalf("expected 2, got %d", len(gotEvs))
	}

	// Re-upsert with updated titles — multi-row ON CONFLICT must update, not duplicate.
	evs[0].Title = "One Updated"
	evs[1].Title = "Two Updated"
	if err := events.UpsertBatch(ctx, evs); err != nil {
		t.Fatal(err)
	}
	gotEvs, err = events.ListByUserID(ctx, userID, PaginationParams{Limit: 10})
	if err != nil {
		t.Fatal(err)
	}
	if len(gotEvs) != 2 {
		t.Fatalf("expected 2 after re-upsert, got %d", len(gotEvs))
	}
	titles := map[string]string{}
	for _, e := range gotEvs {
		titles[e.GoogleEventID] = e.Title
	}
	if titles["e1"] != "One Updated" || titles["e2"] != "Two Updated" {
		t.Fatalf("expected updated titles, got %+v", titles)
	}

	if err := events.DeleteByGoogleEventID(ctx, cal.ID, "e1"); err != nil {
		t.Fatal(err)
	}
	gotEvs, _ = events.ListByUserID(ctx, userID, PaginationParams{Limit: 10})
	if len(gotEvs) != 1 || gotEvs[0].GoogleEventID != "e2" {
		t.Fatalf("expected single e2, got %+v", gotEvs)
	}
}

func TestCalendarEventRepo_ListByUserIDAndTimeRange_Overlap(t *testing.T) {
	db := newTestDB(t)
	cals := NewGORMCalendarRepo(db)
	events := NewGORMCalendarEventRepo(db)
	ctx := context.Background()
	userID := uuid.NewString()

	cal := &models.GoogleCalendar{
		UserID: userID, GoogleCalID: "work@x.com", Summary: "Work",
		Primary: true, AccessRole: "owner", SyncEnabled: true,
	}
	if err := cals.Upsert(ctx, cal); err != nil {
		t.Fatal(err)
	}

	// Fixed window: [day 2 00:00, day 4 00:00) in UTC.
	day1 := time.Date(2026, 7, 1, 12, 0, 0, 0, time.UTC)
	day2 := time.Date(2026, 7, 2, 12, 0, 0, 0, time.UTC)
	day3 := time.Date(2026, 7, 3, 12, 0, 0, 0, time.UTC)
	day5 := time.Date(2026, 7, 5, 12, 0, 0, 0, time.UTC)
	windowStart := time.Date(2026, 7, 2, 0, 0, 0, 0, time.UTC)
	windowEnd := time.Date(2026, 7, 4, 0, 0, 0, 0, time.UTC)

	evs := []models.CalendarEvent{
		// Fully before window — excluded
		{CalendarID: cal.ID, GoogleEventID: "before", Title: "Before", StartTime: day1, EndTime: day1.Add(time.Hour), LastSyncedAt: day1},
		// Fully inside — included
		{CalendarID: cal.ID, GoogleEventID: "inside", Title: "Inside", StartTime: day2, EndTime: day2.Add(time.Hour), LastSyncedAt: day2},
		// Spans into window from before — included (overlap)
		{CalendarID: cal.ID, GoogleEventID: "span-in", Title: "SpanIn", StartTime: day1, EndTime: day3, LastSyncedAt: day1},
		// Spans out of window — included (overlap)
		{CalendarID: cal.ID, GoogleEventID: "span-out", Title: "SpanOut", StartTime: day3, EndTime: day5, LastSyncedAt: day3},
		// Fully after window — excluded
		{CalendarID: cal.ID, GoogleEventID: "after", Title: "After", StartTime: day5, EndTime: day5.Add(time.Hour), LastSyncedAt: day5},
	}
	if err := events.UpsertBatch(ctx, evs); err != nil {
		t.Fatal(err)
	}

	got, err := events.ListByUserIDAndTimeRange(ctx, userID, TimeRange{Start: windowStart, End: windowEnd})
	if err != nil {
		t.Fatal(err)
	}
	ids := map[string]bool{}
	for _, e := range got {
		ids[e.GoogleEventID] = true
	}
	for _, want := range []string{"inside", "span-in", "span-out"} {
		if !ids[want] {
			t.Errorf("expected %q in results, got %+v", want, ids)
		}
	}
	for _, ban := range []string{"before", "after"} {
		if ids[ban] {
			t.Errorf("did not expect %q in results, got %+v", ban, ids)
		}
	}
}

func TestCalendarRepo_UpsertPreservesSyncToken(t *testing.T) {
	db := newTestDB(t)
	cals := NewGORMCalendarRepo(db)
	ctx := context.Background()
	userID := uuid.NewString()

	cal := &models.GoogleCalendar{
		UserID: userID, GoogleCalID: "c@x.com", Summary: "C",
		SyncEnabled: true, SyncToken: "first-token",
	}
	if err := cals.Upsert(ctx, cal); err != nil {
		t.Fatal(err)
	}

	updated := &models.GoogleCalendar{
		UserID: userID, GoogleCalID: "c@x.com", Summary: "C2",
		SyncEnabled: false, SyncToken: "",
	}
	if err := cals.Upsert(ctx, updated); err != nil {
		t.Fatal(err)
	}

	got, _ := cals.GetByGoogleCalID(ctx, userID, "c@x.com")
	if got.SyncToken != "first-token" {
		t.Fatalf("expected first-token preserved, got %q", got.SyncToken)
	}
	if got.SyncEnabled != false {
		t.Fatal("expected sync_enabled to be updated to false")
	}
}
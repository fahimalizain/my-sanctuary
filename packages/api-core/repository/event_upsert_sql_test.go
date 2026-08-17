//go:build !js

package repository

import (
	"strings"
	"testing"
	"time"

	"my-sanctuary/packages/api-core/models"
)

func TestBuildEventUpsertSQL_SingleRow(t *testing.T) {
	now := time.Now()
	sql, args := buildEventUpsertSQL([]models.CalendarEvent{{
		CalendarID:    "cal-1",
		GoogleEventID: "g-1",
		GoogleETag:    "etag",
		Title:         "Meeting",
		StartTime:     now,
		EndTime:       now.Add(time.Hour),
		LastSyncedAt:  now,
	}})

	if !strings.Contains(sql, "INSERT INTO calendar_events") {
		t.Fatalf("expected INSERT, got: %s", sql)
	}
	if !strings.Contains(sql, "ON CONFLICT(calendar_id, google_event_id)") {
		t.Fatalf("expected ON CONFLICT, got: %s", sql)
	}
	if strings.Count(sql, "(?,?,?,?,?,?,?,?,?,?,?,?,?)") != 1 {
		t.Fatalf("expected one VALUES tuple, got sql: %s", sql)
	}
	if len(args) != eventUpsertColCount {
		t.Fatalf("expected %d args, got %d", eventUpsertColCount, len(args))
	}
	if args[1] != "cal-1" || args[2] != "g-1" || args[6] != "Meeting" {
		t.Fatalf("unexpected bound values: %#v", args)
	}
}

func TestBuildEventUpsertSQL_MultiRow(t *testing.T) {
	now := time.Now()
	events := make([]models.CalendarEvent, eventUpsertChunkSize)
	for i := range events {
		events[i] = models.CalendarEvent{
			CalendarID:    "cal-1",
			GoogleEventID: "g-" + string(rune('a'+i)),
			Title:         "E",
			StartTime:     now,
			EndTime:       now.Add(time.Hour),
			LastSyncedAt:  now,
		}
	}

	sql, args := buildEventUpsertSQL(events)
	if strings.Count(sql, "(?,?,?,?,?,?,?,?,?,?,?,?,?)") != eventUpsertChunkSize {
		t.Fatalf("expected %d VALUES tuples, got sql: %s", eventUpsertChunkSize, sql)
	}
	wantArgs := eventUpsertChunkSize * eventUpsertColCount
	if len(args) != wantArgs {
		t.Fatalf("expected %d args (under D1 limit of 100), got %d", wantArgs, len(args))
	}
	if wantArgs > 100 {
		t.Fatalf("chunk exceeds D1 bind limit: %d", wantArgs)
	}
}

func TestEventUpsertChunkSize_RespectsD1BindLimit(t *testing.T) {
	if eventUpsertChunkSize*eventUpsertColCount > 100 {
		t.Fatalf("chunk size %d * %d cols = %d > 100",
			eventUpsertChunkSize, eventUpsertColCount, eventUpsertChunkSize*eventUpsertColCount)
	}
	if eventUpsertChunkSize != 7 {
		t.Fatalf("expected chunk size 7, got %d", eventUpsertChunkSize)
	}
}

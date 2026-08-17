package repository

import (
	"strings"
	"time"

	"github.com/google/uuid"

	"my-sanctuary/packages/api-core/models"
)

// D1 allows at most 100 bound parameters per SQL statement.
// calendar_events upsert binds 13 columns per row → max 7 rows per statement.
const (
	eventUpsertColCount  = 13
	eventUpsertChunkSize = 100 / eventUpsertColCount // 7
)

const eventUpsertOnConflict = `ON CONFLICT(calendar_id, google_event_id) DO UPDATE SET
			google_etag = excluded.google_etag,
			google_updated_at = excluded.google_updated_at,
			last_synced_at = excluded.last_synced_at,
			title = excluded.title,
			description = excluded.description,
			start_time = excluded.start_time,
			end_time = excluded.end_time,
			recurrence = excluded.recurrence,
			updated_at = excluded.updated_at`

// buildEventUpsertSQL builds a multi-row INSERT … ON CONFLICT statement for a
// chunk of events (must be non-empty and ≤ eventUpsertChunkSize).
// Shared by the D1 implementation; kept free of build tags for unit tests.
func buildEventUpsertSQL(events []models.CalendarEvent) (sql string, args []interface{}) {
	now := time.Now().UTC().Format(time.RFC3339)

	var b strings.Builder
	b.WriteString(`INSERT INTO calendar_events
		(id, calendar_id, google_event_id, google_etag, google_updated_at, last_synced_at, title, description, start_time, end_time, recurrence, created_at, updated_at)
		VALUES `)

	args = make([]interface{}, 0, len(events)*eventUpsertColCount)
	for i := range events {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString("(?,?,?,?,?,?,?,?,?,?,?,?,?)")
		ev := &events[i]
		args = append(args,
			uuid.NewString(), ev.CalendarID, ev.GoogleEventID, ev.GoogleETag,
			ev.GoogleUpdatedAt.UTC().Format(time.RFC3339),
			ev.LastSyncedAt.UTC().Format(time.RFC3339),
			ev.Title, ev.Description,
			ev.StartTime.UTC().Format(time.RFC3339),
			ev.EndTime.UTC().Format(time.RFC3339),
			ev.Recurrence,
			now, now,
		)
	}
	b.WriteByte(' ')
	b.WriteString(eventUpsertOnConflict)
	return b.String(), args
}

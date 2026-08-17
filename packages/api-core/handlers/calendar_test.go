package handlers

import (
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestParseEventTimeRange_Defaults(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/api/calendar/events", nil)
	tr, err := parseEventTimeRange(r)
	if err != nil {
		t.Fatal(err)
	}
	if !tr.End.After(tr.Start) {
		t.Fatalf("expected end after start, got %v .. %v", tr.Start, tr.End)
	}
}

func TestParseEventTimeRange_FromQuery(t *testing.T) {
	min := "2026-06-28T18:00:00.000Z"
	max := "2026-08-09T18:00:00.000Z"
	r := httptest.NewRequest(http.MethodGet, "/api/calendar/events?time_min="+min+"&time_max="+max, nil)
	tr, err := parseEventTimeRange(r)
	if err != nil {
		t.Fatal(err)
	}
	wantStart, _ := time.Parse(time.RFC3339Nano, min)
	wantEnd, _ := time.Parse(time.RFC3339Nano, max)
	if !tr.Start.Equal(wantStart) || !tr.End.Equal(wantEnd) {
		t.Fatalf("got %v .. %v, want %v .. %v", tr.Start, tr.End, wantStart, wantEnd)
	}
}

func TestParseEventTimeRange_Invalid(t *testing.T) {
	r := httptest.NewRequest(http.MethodGet, "/api/calendar/events?time_min=not-a-date", nil)
	if _, err := parseEventTimeRange(r); err == nil {
		t.Fatal("expected error for invalid time_min")
	}

	r = httptest.NewRequest(http.MethodGet, "/api/calendar/events?time_min=2026-07-01T00:00:00Z&time_max=2026-06-01T00:00:00Z", nil)
	if _, err := parseEventTimeRange(r); err == nil {
		t.Fatal("expected error when time_max is before time_min")
	}
}

package handlers

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"golang.org/x/oauth2"
	"my-sanctuary/packages/api-core/auth"
	"my-sanctuary/packages/api-core/config"
	"my-sanctuary/packages/api-core/models"
	"my-sanctuary/packages/api-core/repository"
)

const syncStaleThreshold = 5 * time.Minute

// CalendarHandler provides endpoints that exercise Google Calendar read/write scope.
type CalendarHandler struct {
	oauthConfig    *oauth2.Config
	httpClient     *http.Client
	auth           *AuthHandler
	calendarRepo   repository.CalendarRepo
	eventRepo      repository.CalendarEventRepo
	tokenRefresher *auth.TokenRefresher
}

// NewCalendarHandler creates a calendar handler backed by the auth handler and repos.
func NewCalendarHandler(cfg *config.Config, httpClient *http.Client, authHandler *AuthHandler, calRepo repository.CalendarRepo, eventRepo repository.CalendarEventRepo, tokenRepo repository.TokenRepo) *CalendarHandler {
	oauthCfg := &oauth2.Config{
		ClientID:     cfg.OAuth.ClientID,
		ClientSecret: cfg.OAuth.ClientSecret,
		Scopes:       []string{"https://www.googleapis.com/auth/calendar"},
		Endpoint: oauth2.Endpoint{
			AuthURL:  "https://accounts.google.com/o/oauth2/auth",
			TokenURL: "https://oauth2.googleapis.com/token",
		},
	}
	return &CalendarHandler{
		oauthConfig:    oauthCfg,
		httpClient:     httpClient,
		auth:           authHandler,
		calendarRepo:   calRepo,
		eventRepo:      eventRepo,
		tokenRefresher: auth.NewTokenRefresher(oauthCfg, tokenRepo, httpClient),
	}
}

// googleCalendarListEntry models a single entry from /users/me/calendarList.
type googleCalendarListEntry struct {
	ID         string `json:"id"`
	Summary    string `json:"summary"`
	TimeZone   string `json:"timeZone"`
	Primary    bool   `json:"primary"`
	AccessRole string `json:"accessRole"`
}

// googleCalendarListResponse models the response from /users/me/calendarList.
type googleCalendarListResponse struct {
	Items []googleCalendarListEntry `json:"items"`
}

// googleEvent models the subset of a Google Calendar event we persist.
type googleEvent struct {
	ID          string   `json:"id"`
	Etag        string   `json:"etag"`
	Updated     string   `json:"updated"`
	Summary     string   `json:"summary"`
	Description string   `json:"description"`
	Recurrence  []string `json:"recurrence"`
	Start       struct {
		DateTime string `json:"dateTime"`
	} `json:"start"`
	End struct {
		DateTime string `json:"dateTime"`
	} `json:"end"`
}

// googleEventsResponse models the response from /calendars/{id}/events.
type googleEventsResponse struct {
	Items          []googleEvent `json:"items"`
	NextSyncToken  string        `json:"nextSyncToken"`
	NextPageToken  string        `json:"nextPageToken"`
}

// googleCancelledEntry models a cancelled event in incremental sync responses.
type googleCancelledEntry struct {
	ID    string `json:"id"`
	Status string `json:"status"`
}

// ListEvents serves cached events for all of the user's sync-enabled
// calendars, syncing each calendar from Google if its cache is stale.
func (h *CalendarHandler) ListEvents(w http.ResponseWriter, r *http.Request) {
	userID, ok := h.auth.userIDFromSession(r)
	if !ok {
		writeError(w, http.StatusUnauthorized, "unauthorized")
		return
	}

	tok, err := h.tokenRefresher.RefreshIfNeeded(r.Context(), userID)
	if err != nil {
		writeError(w, http.StatusUnauthorized, "unauthorized")
		return
	}

	cals, err := h.calendarRepo.ListByUserID(r.Context(), userID)
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load calendars")
		return
	}
	if len(cals) == 0 {
		cals, err = h.refreshCalendarList(r.Context(), userID, tok)
		if err != nil {
			fmt.Printf("refreshCalendarList failed: %v\n", err)
			writeError(w, http.StatusInternalServerError, "failed to fetch calendar list")
			return
		}
	}

	for i := range cals {
		if !cals[i].SyncEnabled {
			continue
		}
		if cals[i].LastSyncedAt != nil && time.Since(*cals[i].LastSyncedAt) < syncStaleThreshold {
			continue
		}
		fmt.Printf("syncing calendar %s (%s)...\n", cals[i].ID, cals[i].GoogleCalID)
		err := h.syncCalendar(r.Context(), &cals[i], tok)
		if err != nil {
			if isGoogle404(err) {
				// Some calendars in /users/me/calendarList (e.g. "Contacts' birthdays and events",
				// holidays) don't support events.list and return 404. Auto-disable sync
				// for these so they don't error on every request. The user can re-enable
				// them later if Google adds support.
				fmt.Printf("calendar %s (%s) returned 404 — disabling sync\n", cals[i].ID, cals[i].GoogleCalID)
				_ = h.calendarRepo.SetSyncEnabled(r.Context(), cals[i].ID, false)
			} else {
				fmt.Printf("sync failed for calendar %s: %v\n", cals[i].ID, err)
			}
		} else {
			fmt.Printf("synced calendar %s (%s) successfully\n", cals[i].ID, cals[i].GoogleCalID)
		}
	}

	events, err := h.eventRepo.ListByUserID(r.Context(), userID, repository.PaginationParams{Limit: 100})
	if err != nil {
		writeError(w, http.StatusInternalServerError, "failed to load events")
		return
	}
	writeJSON(w, map[string]interface{}{"events": events, "source": "cache"})
}

// oauth2Context injects the Workers-compatible HTTP client into the context
// so oauth2's transport uses fetch with correct `this` binding. Without this,
// Google API calls panic with "Illegal invocation" on Cloudflare Workers.
// See docs/CF_WORKERS_OAUTH2_ILLEGAL_INVOCATION.md for details.
func (h *CalendarHandler) oauth2Context(ctx context.Context) context.Context {
	if h.httpClient != nil {
		return context.WithValue(ctx, oauth2.HTTPClient, h.httpClient)
	}
	return ctx
}

// refreshCalendarList fetches /users/me/calendarList and upserts each row.
func (h *CalendarHandler) refreshCalendarList(ctx context.Context, userID string, tok *oauth2.Token) ([]models.GoogleCalendar, error) {
	client := h.oauthConfig.Client(h.oauth2Context(ctx), tok)
	resp, err := client.Get("https://www.googleapis.com/calendar/v3/users/me/calendarList")
	if err != nil {
		return nil, fmt.Errorf("calendarList fetch: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("calendarList fetch: google returned %d: %s", resp.StatusCode, string(body))
	}

	var list googleCalendarListResponse
	if err := json.NewDecoder(resp.Body).Decode(&list); err != nil {
		return nil, fmt.Errorf("calendarList decode: %w", err)
	}

	rows := make([]models.GoogleCalendar, 0, len(list.Items))
	for _, gc := range list.Items {
		rows = append(rows, models.GoogleCalendar{
			UserID:      userID,
			GoogleCalID: gc.ID,
			Summary:     gc.Summary,
			TimeZone:    gc.TimeZone,
			Primary:     gc.Primary,
			AccessRole:  gc.AccessRole,
			SyncEnabled: true,
		})
	}
	if err := h.calendarRepo.UpsertBatch(ctx, rows); err != nil {
		return nil, err
	}
	return h.calendarRepo.ListByUserID(ctx, userID)
}

// syncCalendar performs a full or incremental sync of one calendar.
func (h *CalendarHandler) syncCalendar(ctx context.Context, cal *models.GoogleCalendar, tok *oauth2.Token) error {
	now := time.Now()

	resp, err := h.fetchGoogleEvents(ctx, tok, cal.GoogleCalID, cal.SyncToken)
	if err != nil {
		return err
	}

	events := make([]models.CalendarEvent, 0, len(resp.Items))
	for i := range resp.Items {
		events = append(events, mapGoogleEventToModel(resp.Items[i], cal.ID))
	}
	if err := h.eventRepo.UpsertBatch(ctx, events); err != nil {
		return err
	}

	nextToken := resp.NextSyncToken
	if nextToken == "" {
		nextToken = cal.SyncToken
	}
	return h.calendarRepo.UpdateSyncState(ctx, cal.ID, nextToken, now)
}

// fetchGoogleEvents calls events.list, optionally with a syncToken for
// incremental sync. It follows nextPageToken for full syncs.
func (h *CalendarHandler) fetchGoogleEvents(ctx context.Context, tok *oauth2.Token, googleCalID, syncToken string) (*googleEventsResponse, error) {
	client := h.oauthConfig.Client(h.oauth2Context(ctx), tok)

	var allItems []googleEvent
	var nextSyncToken string
	pageToken := ""

	for {
		url := fmt.Sprintf("https://www.googleapis.com/calendar/v3/calendars/%s/events?singleEvents=false&maxResults=250", googleCalID)
		if syncToken != "" {
			url += "&syncToken=" + syncToken
		}
		if pageToken != "" {
			url += "&pageToken=" + pageToken
		}

		resp, err := client.Get(url)
		if err != nil {
			return nil, err
		}
		if resp.StatusCode == 410 {
			resp.Body.Close()
			return h.fetchGoogleEvents(ctx, tok, googleCalID, "")
		}
		if resp.StatusCode != http.StatusOK {
			resp.Body.Close()
			return nil, fmt.Errorf("google events.list returned %d", resp.StatusCode)
		}

		var page googleEventsResponse
		if err := json.NewDecoder(resp.Body).Decode(&page); err != nil {
			resp.Body.Close()
			return nil, err
		}
		resp.Body.Close()

		allItems = append(allItems, page.Items...)
		nextSyncToken = page.NextSyncToken
		pageToken = page.NextPageToken
		if pageToken == "" {
			break
		}
	}

	return &googleEventsResponse{Items: allItems, NextSyncToken: nextSyncToken}, nil
}

// CreateEvent inserts an event into Google on a specific calendar, then
// upserts the returned row locally so the cache stays consistent.
func (h *CalendarHandler) CreateEvent(w http.ResponseWriter, r *http.Request) {
	userID, ok := h.auth.userIDFromSession(r)
	if !ok {
		writeError(w, http.StatusUnauthorized, "unauthorized")
		return
	}

	tok, err := h.tokenRefresher.RefreshIfNeeded(r.Context(), userID)
	if err != nil {
		writeError(w, http.StatusUnauthorized, "unauthorized")
		return
	}

	var input struct {
		CalendarID  string `json:"calendar_id"`
		Summary     string `json:"summary"`
		Description string `json:"description,omitempty"`
		Start       string `json:"start"`
		End         string `json:"end"`
	}
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		writeError(w, http.StatusBadRequest, "invalid body")
		return
	}

	cal, err := h.calendarRepo.GetByID(r.Context(), input.CalendarID)
	if err != nil {
		writeError(w, http.StatusNotFound, "calendar not found")
		return
	}

	created, err := h.postGoogleEvent(r.Context(), tok, cal.GoogleCalID, input.Summary, input.Description, input.Start, input.End)
	if err != nil {
		writeError(w, http.StatusBadGateway, err.Error())
		return
	}

	ev := mapGoogleEventToModel(*created, cal.ID)
	if err := h.eventRepo.Upsert(r.Context(), &ev); err != nil {
		fmt.Printf("cache upsert failed for created event: %v\n", err)
	}
	writeJSON(w, map[string]interface{}{"event": ev, "source": "google"})
}

// postGoogleEvent inserts an event into a Google Calendar via the API.
func (h *CalendarHandler) postGoogleEvent(ctx context.Context, tok *oauth2.Token, googleCalID, summary, description, start, end string) (*googleEvent, error) {
	event := map[string]interface{}{
		"summary":     summary,
		"description": description,
		"start":       map[string]string{"dateTime": start},
		"end":         map[string]string{"dateTime": end},
	}
	body, _ := json.Marshal(event)

	url := fmt.Sprintf("https://www.googleapis.com/calendar/v3/calendars/%s/events", googleCalID)
	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")

	client := h.oauthConfig.Client(h.oauth2Context(ctx), tok)
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("google events.insert returned %d", resp.StatusCode)
	}

	var created googleEvent
	if err := json.NewDecoder(resp.Body).Decode(&created); err != nil {
		return nil, err
	}
	return &created, nil
}

// mapGoogleEventToModel converts a Google Calendar event API response into
// our local CalendarEvent model, parsing the RFC 3339 timestamps.
func mapGoogleEventToModel(g googleEvent, calendarID string) models.CalendarEvent {
	now := time.Now()
	updated, _ := time.Parse(time.RFC3339, g.Updated)
	start, _ := time.Parse(time.RFC3339, g.Start.DateTime)
	end, _ := time.Parse(time.RFC3339, g.End.DateTime)
	return models.CalendarEvent{
		CalendarID:      calendarID,
		GoogleEventID:   g.ID,
		GoogleETag:      g.Etag,
		GoogleUpdatedAt: updated,
		LastSyncedAt:    now,
		Title:           g.Summary,
		Description:     g.Description,
		StartTime:       start,
		EndTime:         end,
		Recurrence:      serializeRecurrence(g.Recurrence),
	}
}

func serializeRecurrence(rules []string) string {
	if len(rules) == 0 {
		return ""
	}
	b, _ := json.Marshal(rules)
	return string(b)
}

func writeJSON(w http.ResponseWriter, v interface{}) {
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(map[string]string{"error": msg})
}

func isGoogle404(err error) bool {
	return err != nil && strings.Contains(err.Error(), "404")
}
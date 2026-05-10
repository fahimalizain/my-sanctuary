package handlers

import (
	"bytes"
	"encoding/json"
	"net/http"
)

// CalendarHandler provides endpoints that exercise Google Calendar read/write scope.
type CalendarHandler struct {
	auth *AuthHandler
}

// NewCalendarHandler creates a calendar handler backed by the auth handler.
func NewCalendarHandler(auth *AuthHandler) *CalendarHandler {
	return &CalendarHandler{auth: auth}
}

// ListEvents returns the next 10 events from the user's primary calendar.
func (h *CalendarHandler) ListEvents(w http.ResponseWriter, r *http.Request) {
	token, ok := h.auth.tokenFromSession(r)
	if !ok {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnauthorized)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "unauthorized"})
		return
	}

	client := h.auth.oauthConfig.Client(h.auth.oauth2Context(r.Context()), token)
	resp, err := client.Get("https://www.googleapis.com/calendar/v3/calendars/primary/events?maxResults=10&orderBy=startTime&singleEvents=true")
	if err != nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
		return
	}
	defer resp.Body.Close()

	var result struct {
		Items []map[string]interface{} `json:"items"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]interface{}{"events": result.Items})
}

// CreateEvent inserts a new event into the user's primary calendar.
func (h *CalendarHandler) CreateEvent(w http.ResponseWriter, r *http.Request) {
	token, ok := h.auth.tokenFromSession(r)
	if !ok {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusUnauthorized)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "unauthorized"})
		return
	}

	var input struct {
		Summary     string `json:"summary"`
		Description string `json:"description,omitempty"`
		Start       string `json:"start"`
		End         string `json:"end"`
	}
	if err := json.NewDecoder(r.Body).Decode(&input); err != nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadRequest)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": "invalid body"})
		return
	}

	event := map[string]interface{}{
		"summary":     input.Summary,
		"description": input.Description,
		"start":       map[string]string{"dateTime": input.Start},
		"end":         map[string]string{"dateTime": input.End},
	}

	body, _ := json.Marshal(event)
	req, err := http.NewRequestWithContext(r.Context(), "POST", "https://www.googleapis.com/calendar/v3/calendars/primary/events", bytes.NewReader(body))
	if err != nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
		return
	}
	req.Header.Set("Content-Type", "application/json")

	client := h.auth.oauthConfig.Client(h.auth.oauth2Context(r.Context()), token)
	resp, err := client.Do(req)
	if err != nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
		return
	}
	defer resp.Body.Close()

	var created map[string]interface{}
	_ = json.NewDecoder(resp.Body).Decode(&created)

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(resp.StatusCode)
	_ = json.NewEncoder(w).Encode(created)
}

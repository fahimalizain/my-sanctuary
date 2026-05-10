package handlers

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"my-sanctuary/apps/api/config"
)

func TestAuthHandler_Initiate_Redirects(t *testing.T) {
	h := NewAuthHandler(&config.Config{
		OAuth: &config.OAuthConfig{
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			RedirectURL:  "http://localhost:8080/auth/google/callback",
		},
		SessionSecret: "super-secret-key-for-tests!!",
		FrontendURL:   "http://localhost:5173",
	}, nil)

	req := httptest.NewRequest(http.MethodGet, "/auth/google", nil)
	rr := httptest.NewRecorder()

	h.Initiate(rr, req)

	if rr.Code != http.StatusTemporaryRedirect {
		t.Fatalf("expected status %d, got %d", http.StatusTemporaryRedirect, rr.Code)
	}
	loc := rr.Header().Get("Location")
	if !strings.Contains(loc, "accounts.google.com") {
		t.Fatalf("expected redirect to google, got %s", loc)
	}
}

func TestAuthHandler_Callback_InvalidState(t *testing.T) {
	h := NewAuthHandler(&config.Config{
		OAuth: &config.OAuthConfig{
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			RedirectURL:  "http://localhost:8080/auth/google/callback",
		},
		SessionSecret: "super-secret-key-for-tests!!",
		FrontendURL:   "http://localhost:5173",
	}, nil)

	req := httptest.NewRequest(http.MethodGet, "/auth/google/callback?state=bad", nil)
	rr := httptest.NewRecorder()

	h.Callback(rr, req)

	if rr.Code != http.StatusBadRequest {
		t.Fatalf("expected status %d, got %d", http.StatusBadRequest, rr.Code)
	}
}

func TestAuthHandler_Me_Unauthorized(t *testing.T) {
	h := NewAuthHandler(&config.Config{
		OAuth: &config.OAuthConfig{
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			RedirectURL:  "http://localhost:8080/auth/google/callback",
		},
		SessionSecret: "super-secret-key-for-tests!!",
		FrontendURL:   "http://localhost:5173",
	}, nil)

	req := httptest.NewRequest(http.MethodGet, "/auth/me", nil)
	rr := httptest.NewRecorder()

	h.Me(rr, req)

	if rr.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d", http.StatusOK, rr.Code)
	}
	if !strings.Contains(rr.Body.String(), `"user":null`) && !strings.Contains(rr.Body.String(), `"user": null`) {
		t.Fatalf("expected null user, got %s", rr.Body.String())
	}
}

func TestAuthHandler_Logout_ClearsSession(t *testing.T) {
	h := NewAuthHandler(&config.Config{
		OAuth: &config.OAuthConfig{
			ClientID:     "test-client",
			ClientSecret: "test-secret",
			RedirectURL:  "http://localhost:8080/auth/google/callback",
		},
		SessionSecret: "super-secret-key-for-tests!!",
		FrontendURL:   "http://localhost:5173",
	}, nil)

	req := httptest.NewRequest(http.MethodPost, "/auth/logout", nil)
	rr := httptest.NewRecorder()

	h.Logout(rr, req)

	if rr.Code != http.StatusOK {
		t.Fatalf("expected status %d, got %d", http.StatusOK, rr.Code)
	}
}

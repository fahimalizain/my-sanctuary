package handlers

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"

	"github.com/gorilla/sessions"
	"golang.org/x/oauth2"
	"my-sanctuary/packages/api-core/config"
	"my-sanctuary/packages/api-core/models"
	"my-sanctuary/packages/api-core/repository"
)

const (
	sessionName     = "sanctuary-session"
	stateCookieName = "oauth_state"
)

// AuthHandler manages Google OAuth and session state.
type AuthHandler struct {
	oauthConfig *oauth2.Config
	store       *sessions.CookieStore
	frontendURL string
	httpClient  *http.Client
	userRepo    repository.UserRepo
	tokenRepo   repository.TokenRepo
}

// GoogleUser represents the public profile returned by Google.
type GoogleUser struct {
	ID            string `json:"id"`
	Email         string `json:"email"`
	VerifiedEmail bool   `json:"verified_email"`
	Name          string `json:"name"`
	Picture       string `json:"picture"`
}

// NewAuthHandler creates an AuthHandler from application config.
// userRepo and tokenRepo may be nil for routes that don't need persistence
// (e.g. the existing test handlers), but Callback will fail if they are nil.
func NewAuthHandler(cfg *config.Config, httpClient *http.Client, userRepo repository.UserRepo, tokenRepo repository.TokenRepo) *AuthHandler {
	store := sessions.NewCookieStore([]byte(cfg.SessionSecret))
	store.Options = &sessions.Options{
		Path:     "/",
		MaxAge:   86400 * 7,
		HttpOnly: true,
		Secure:   cfg.SecureCookie,
		SameSite: http.SameSiteLaxMode,
	}

	return &AuthHandler{
		oauthConfig: &oauth2.Config{
			ClientID:     cfg.OAuth.ClientID,
			ClientSecret: cfg.OAuth.ClientSecret,
			RedirectURL:  cfg.OAuth.RedirectURL,
			Scopes: []string{
				"openid",
				"email",
				"profile",
				"https://www.googleapis.com/auth/calendar",
			},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://accounts.google.com/o/oauth2/auth",
				TokenURL: "https://oauth2.googleapis.com/token",
			},
		},
		store:       store,
		frontendURL: cfg.FrontendURL,
		httpClient:  httpClient,
		userRepo:    userRepo,
		tokenRepo:   tokenRepo,
	}
}

// oauth2Context returns a context that carries the custom HTTP client for oauth2
// operations. This is required in environments like Cloudflare Workers where the
// default Go HTTP client can trigger "Illegal invocation" errors due to loose
// fetch bindings in the JS/WASM runtime.
func (h *AuthHandler) oauth2Context(ctx context.Context) context.Context {
	if h.httpClient != nil {
		return context.WithValue(ctx, oauth2.HTTPClient, h.httpClient)
	}
	return ctx
}

func generateState() string {
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	return base64.URLEncoding.EncodeToString(b)
}

// Initiate starts the Google OAuth flow by redirecting to Google's consent screen.
func (h *AuthHandler) Initiate(w http.ResponseWriter, r *http.Request) {
	state := generateState()
	http.SetCookie(w, &http.Cookie{
		Name:     stateCookieName,
		Value:    state,
		Path:     "/",
		MaxAge:   600,
		HttpOnly: true,
		Secure:   h.store.Options.Secure,
		SameSite: http.SameSiteLaxMode,
	})

	url := h.oauthConfig.AuthCodeURL(state, oauth2.AccessTypeOffline, oauth2.ApprovalForce)
	http.Redirect(w, r, url, http.StatusTemporaryRedirect)
}

// Callback handles the redirect from Google after user consent.
func (h *AuthHandler) Callback(w http.ResponseWriter, r *http.Request) {
	state := r.URL.Query().Get("state")
	cookie, err := r.Cookie(stateCookieName)
	if err != nil || cookie.Value != state {
		http.Error(w, "invalid state", http.StatusBadRequest)
		return
	}
	// clear state cookie
	http.SetCookie(w, &http.Cookie{
		Name:     stateCookieName,
		Value:    "",
		Path:     "/",
		MaxAge:   -1,
		HttpOnly: true,
		Secure:   h.store.Options.Secure,
		SameSite: http.SameSiteLaxMode,
	})

	code := r.URL.Query().Get("code")
	if code == "" {
		http.Error(w, "code not found", http.StatusBadRequest)
		return
	}

	token, err := h.oauthConfig.Exchange(h.oauth2Context(r.Context()), code)
	if err != nil {
		http.Error(w, fmt.Sprintf("failed to exchange token: %v", err), http.StatusInternalServerError)
		return
	}

	client := h.oauthConfig.Client(h.oauth2Context(r.Context()), token)
	resp, err := client.Get("https://www.googleapis.com/oauth2/v2/userinfo")
	if err != nil {
		http.Error(w, fmt.Sprintf("failed to get user info: %v", err), http.StatusInternalServerError)
		return
	}
	defer resp.Body.Close()

	var user GoogleUser
	if err := json.NewDecoder(resp.Body).Decode(&user); err != nil {
		http.Error(w, fmt.Sprintf("failed to decode user info: %v", err), http.StatusInternalServerError)
		return
	}

	// Persist user identity to the DB (upsert by google_id).
	savedUser, err := h.userRepo.UpsertByGoogleID(r.Context(), &models.User{
		GoogleID: user.ID,
		Email:    user.Email,
		Name:     user.Name,
		Picture:  user.Picture,
	})
	if err != nil {
		http.Error(w, fmt.Sprintf("failed to save user: %v", err), http.StatusInternalServerError)
		return
	}

	// Persist Google tokens to the dedicated table.
	if err := h.tokenRepo.Upsert(r.Context(), &models.GoogleOAuthToken{
		UserID:       savedUser.ID,
		AccessToken:  token.AccessToken,
		RefreshToken: token.RefreshToken,
		Expiry:       token.Expiry,
		TokenType:    token.TokenType,
	}); err != nil {
		http.Error(w, fmt.Sprintf("failed to save token: %v", err), http.StatusInternalServerError)
		return
	}

	// Cookie carries only identity. Google tokens live in the DB now.
	session, _ := h.store.Get(r, sessionName)
	session.Values["user_id"] = savedUser.ID
	session.Values["email"] = savedUser.Email
	session.Values["name"] = savedUser.Name
	session.Values["picture"] = savedUser.Picture

	if err := session.Save(r, w); err != nil {
		http.Error(w, fmt.Sprintf("failed to save session: %v", err), http.StatusInternalServerError)
		return
	}

	http.Redirect(w, r, h.frontendURL, http.StatusTemporaryRedirect)
}

// Me returns the currently authenticated user, or null if not authenticated.
func (h *AuthHandler) Me(w http.ResponseWriter, r *http.Request) {
	session, err := h.store.Get(r, sessionName)
	if err != nil || session.Values["user_id"] == nil {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_ = json.NewEncoder(w).Encode(map[string]interface{}{"user": nil})
		return
	}

	user := &GoogleUser{
		ID:      getString(session.Values, "user_id"),
		Email:   getString(session.Values, "email"),
		Name:    getString(session.Values, "name"),
		Picture: getString(session.Values, "picture"),
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]interface{}{"user": user})
}

// Logout clears the session cookie.
func (h *AuthHandler) Logout(w http.ResponseWriter, r *http.Request) {
	session, _ := h.store.Get(r, sessionName)
	session.Values = map[interface{}]interface{}{}
	session.Options.MaxAge = -1
	_ = session.Save(r, w)
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]interface{}{"success": true})
}

// userIDFromSession returns the authenticated user's DB id from the session.
// The session no longer carries Google tokens; those live in the DB and are
// retrieved via TokenRefresher when needed.
func (h *AuthHandler) userIDFromSession(r *http.Request) (string, bool) {
	session, err := h.store.Get(r, sessionName)
	if err != nil || session.Values["user_id"] == nil {
		return "", false
	}
	id := getString(session.Values, "user_id")
	if id == "" {
		return "", false
	}
	return id, true
}

func getString(m map[interface{}]interface{}, key string) string {
	v, ok := m[key]
	if !ok {
		return ""
	}
	s, _ := v.(string)
	return s
}

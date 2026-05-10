package config

import (
	"encoding/json"
	"fmt"
	"net/url"
	"os"
)

// Config holds application configuration loaded from environment variables.
type Config struct {
	OAuth         *OAuthConfig
	SessionSecret string
	FrontendURL   string
	SecureCookie  bool
}

// OAuthConfig holds the parsed Google OAuth client credentials.
type OAuthConfig struct {
	ClientID     string
	ClientSecret string
	RedirectURL  string
}

type googleCredentialsFile struct {
	Web *struct {
		ClientID     string   `json:"client_id"`
		ClientSecret string   `json:"client_secret"`
		RedirectURIs []string `json:"redirect_uris"`
	} `json:"web"`
	ClientID     string   `json:"client_id"`
	ClientSecret string   `json:"client_secret"`
	RedirectURIs []string `json:"redirect_uris"`
}

// Load reads environment variables and returns a validated Config.
func Load() (*Config, error) {
	return LoadWithEnv(os.Getenv)
}

// LoadWithEnv reads configuration using the provided environment variable getter.
// This allows custom env sources (e.g. Cloudflare Workers runtime) to be used.
func LoadWithEnv(getenv func(string) string) (*Config, error) {
	cfg := &Config{
		FrontendURL: getenv("FRONTEND_URL"),
	}
	if cfg.FrontendURL == "" {
		cfg.FrontendURL = "http://localhost:5173"
	}

	cfg.SessionSecret = getenv("SESSION_SECRET")
	if cfg.SessionSecret == "" {
		return nil, fmt.Errorf("SESSION_SECRET environment variable is required")
	}

	cfg.SecureCookie = getenv("SECURE_COOKIE") == "true"

	googleJSON := getenv("GOOGLE_CREDENTIALS_JSON")
	if googleJSON == "" {
		return nil, fmt.Errorf("GOOGLE_CREDENTIALS_JSON environment variable is required")
	}

	var creds googleCredentialsFile
	if err := json.Unmarshal([]byte(googleJSON), &creds); err != nil {
		return nil, fmt.Errorf("invalid GOOGLE_CREDENTIALS_JSON: %w", err)
	}

	oauth := &OAuthConfig{}
	var redirectURIs []string
	if creds.Web != nil {
		oauth.ClientID = creds.Web.ClientID
		oauth.ClientSecret = creds.Web.ClientSecret
		redirectURIs = creds.Web.RedirectURIs
	} else {
		oauth.ClientID = creds.ClientID
		oauth.ClientSecret = creds.ClientSecret
		redirectURIs = creds.RedirectURIs
	}

	if oauth.ClientID == "" || oauth.ClientSecret == "" {
		return nil, fmt.Errorf("GOOGLE_CREDENTIALS_JSON missing client_id or client_secret")
	}

	frontendHost := hostnameFromURL(cfg.FrontendURL)
	for _, uri := range redirectURIs {
		if hostnameFromURL(uri) == frontendHost {
			oauth.RedirectURL = uri
			break
		}
	}
	if oauth.RedirectURL == "" {
		if len(redirectURIs) > 0 {
			oauth.RedirectURL = redirectURIs[0]
		} else {
			oauth.RedirectURL = cfg.FrontendURL + "/auth/google/callback"
		}
	}

	cfg.OAuth = oauth
	return cfg, nil
}

func hostnameFromURL(raw string) string {
	u, err := url.Parse(raw)
	if err != nil {
		return ""
	}
	return u.Hostname()
}

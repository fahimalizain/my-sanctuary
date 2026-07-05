package auth

import (
	"context"
	"errors"
	"net/http"
	"time"

	"golang.org/x/oauth2"

	"my-sanctuary/packages/api-core/models"
	"my-sanctuary/packages/api-core/repository"
)

// TokenRefresher refreshes Google OAuth access tokens before they expire,
// persisting the refreshed token via TokenRepo.
type TokenRefresher struct {
	oauthCfg   *oauth2.Config
	tokenRepo  repository.TokenRepo
	httpClient *http.Client
}

// NewTokenRefresher creates a TokenRefresher. The httpClient is injected so
// Workers (GOOS=js) can use its fetch-based transport instead of the default
// Go HTTP client.
func NewTokenRefresher(cfg *oauth2.Config, tr repository.TokenRepo, httpClient *http.Client) *TokenRefresher {
	return &TokenRefresher{oauthCfg: cfg, tokenRepo: tr, httpClient: httpClient}
}

// RefreshIfNeeded returns a valid token, refreshing first if it is within
// 5 minutes of expiry. The refreshed token is persisted via TokenRepo.
// Google only returns refresh_token on first consent, so TokenRepo.Upsert
// preserves the existing refresh_token when the new one is empty.
func (r *TokenRefresher) RefreshIfNeeded(ctx context.Context, userID string) (*oauth2.Token, error) {
	stored, err := r.tokenRepo.GetByUserID(ctx, userID)
	if err != nil {
		if errors.Is(err, repository.ErrNotFound) {
			return nil, errors.New("no token for user")
		}
		return nil, err
	}

	current := &oauth2.Token{
		AccessToken:  stored.AccessToken,
		RefreshToken: stored.RefreshToken,
		Expiry:       stored.Expiry,
	}

	if current.Expiry.IsZero() || current.Expiry.After(time.Now().Add(5*time.Minute)) {
		return current, nil
	}

	ctx = context.WithValue(ctx, oauth2.HTTPClient, r.httpClient)
	src := r.oauthCfg.TokenSource(ctx, current)
	refreshed, err := src.Token()
	if err != nil {
		return nil, err
	}

	if err := r.tokenRepo.Upsert(ctx, &models.GoogleOAuthToken{
		UserID:       userID,
		AccessToken:  refreshed.AccessToken,
		RefreshToken: refreshed.RefreshToken,
		Expiry:       refreshed.Expiry,
		TokenType:    refreshed.TokenType,
	}); err != nil {
		return nil, err
	}
	return refreshed, nil
}
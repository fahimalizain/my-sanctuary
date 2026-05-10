package handlers

import (
	"context"
	"fmt"
	"net/http"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humachi"
	"github.com/go-chi/chi/v5"
	"github.com/go-chi/cors"
	"my-sanctuary/apps/api/config"
)

// Dependencies holds injected configuration for route registration.
type Dependencies struct {
	Config *config.Config
}

// GreetingInput represents the input for the greeting endpoint.
type GreetingInput struct {
	Name string `path:"name" doc:"Name to greet"`
}

// GreetingOutput represents the output for the greeting endpoint.
type GreetingOutput struct {
	Body struct {
		Message string `json:"message" doc:"Greeting message"`
	}
}

// RegisterRoutes registers all API routes on the provided chi router.
func RegisterRoutes(router chi.Router, deps *Dependencies) {
	authHandler := NewAuthHandler(deps.Config)

	corsMiddleware := cors.New(cors.Options{
		AllowedOrigins:   []string{deps.Config.FrontendURL},
		AllowedMethods:   []string{"GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"},
		AllowedHeaders:   []string{"Accept", "Authorization", "Content-Type", "X-CSRF-Token"},
		ExposedHeaders:   []string{"Link"},
		AllowCredentials: true,
		MaxAge:           300,
	})
	router.Use(corsMiddleware.Handler)

	api := humachi.New(router, huma.DefaultConfig("Sanctuary API", "1.0.0"))

	// Register greeting endpoint
	huma.Register(api, huma.Operation{
		OperationID: "greet",
		Method:      http.MethodGet,
		Path:        "/greeting/{name}",
	}, func(ctx context.Context, input *GreetingInput) (*GreetingOutput, error) {
		resp := &GreetingOutput{}
		resp.Body.Message = fmt.Sprintf("Hello, %s!", input.Name)
		return resp, nil
	})

	// Register health endpoint
	huma.Register(api, huma.Operation{
		OperationID: "health",
		Method:      http.MethodGet,
		Path:        "/health",
	}, func(ctx context.Context, input *struct{}) (*struct {
		Body struct {
			Status string `json:"status"`
		}
	}, error) {
		return &struct {
			Body struct {
				Status string `json:"status"`
			}
		}{
			Body: struct {
				Status string `json:"status"`
			}{Status: "ok"},
		}, nil
	})

	// Auth routes
	router.Get("/auth/google", authHandler.Initiate)
	router.Get("/auth/google/callback", authHandler.Callback)
	router.Get("/auth/me", authHandler.Me)
	router.Post("/auth/logout", authHandler.Logout)

	// Calendar routes
	calHandler := NewCalendarHandler(authHandler)
	router.Get("/api/calendar/events", calHandler.ListEvents)
	router.Post("/api/calendar/events", calHandler.CreateEvent)
}

package main

import (
	"fmt"
	"net/http"

	"github.com/danielgtaylor/huma/v2/humacli"
	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"my-sanctuary/apps/api/config"
	"my-sanctuary/apps/api/handlers"
)

var version = "dev"

func main() {
	cli := humacli.New(func(hooks humacli.Hooks, options *struct{}) {
		cfg, err := config.Load()
		if err != nil {
			fmt.Printf("failed to load config: %v\n", err)
			return
		}

		router := chi.NewMux()
		router.Use(middleware.Recoverer)
		router.Use(middleware.Logger)
		handlers.RegisterRoutes(router, &handlers.Dependencies{Config: cfg, HTTPClient: http.DefaultClient, Version: version})

		server := &http.Server{
			Addr:    ":8080",
			Handler: router,
		}

		hooks.OnStart(func() {
			fmt.Printf("Sanctuary API v%s running on http://localhost:8080\n", version)
			if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				fmt.Printf("Server error: %v\n", err)
			}
		})
	})

	cli.Run()
}

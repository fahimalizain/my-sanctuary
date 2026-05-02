package main

import (
	"fmt"
	"net/http"

	"github.com/danielgtaylor/huma/v2/humacli"
	"github.com/go-chi/chi/v5"
	"my-sanctuary/apps/api/handlers"
)

func main() {
	cli := humacli.New(func(hooks humacli.Hooks, options *struct{}) {
		router := chi.NewMux()
		handlers.RegisterRoutes(router)

		server := &http.Server{
			Addr:    ":8080",
			Handler: router,
		}

		hooks.OnStart(func() {
			fmt.Println("Server running on http://localhost:8080")
			if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
				fmt.Printf("Server error: %v\n", err)
			}
		})
	})

	cli.Run()
}

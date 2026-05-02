package main

import (
	"context"
	"fmt"
	"net/http"

	"github.com/danielgtaylor/huma/v2"
	"github.com/danielgtaylor/huma/v2/adapters/humachi"
	"github.com/danielgtaylor/huma/v2/humacli"
	"github.com/go-chi/chi/v5"
)

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

func main() {
	cli := humacli.New(func(hooks humacli.Hooks, options *struct{}) {
		router := chi.NewMux()
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

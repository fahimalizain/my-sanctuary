package main

import (
	"embed"
	"fmt"
	"io/fs"
	"net/http"
	"strings"
	"syscall/js"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"github.com/syumai/workers"
	"github.com/syumai/workers/cloudflare"
	"github.com/syumai/workers/cloudflare/fetch"
	"my-sanctuary/packages/api-core/config"
	"my-sanctuary/packages/api-core/handlers"
	"my-sanctuary/packages/api-core/repository"
)

//go:embed dist/*
var staticFS embed.FS

var version = "dev"

// getD1Binding returns the D1 binding set by d1_shim.js on globalThis.__D1__.
// The shim wraps the generated fetch handler to stash env.DB before invoking Go.
func getD1Binding() js.Value {
	return js.Global().Get("__D1__")
}

func main() {
	cfg, err := config.LoadWithEnv(cloudflare.Getenv)
	if err != nil {
		fmt.Printf("failed to load config: %v\n", err)
		return
	}

	router := chi.NewMux()
	router.Use(middleware.Recoverer)
	router.Use(middleware.Logger)

	// Register API routes from the shared handlers package.
	// D1 repos read from the __D1__ global set by d1_shim.js.
	handlers.RegisterRoutes(router, &handlers.Dependencies{
		Config:            cfg,
		HTTPClient:        fetch.NewClient().HTTPClient(fetch.RedirectModeFollow),
		Version:           version,
		UserRepo:          repository.NewD1UserRepo(getD1Binding),
		TokenRepo:         repository.NewD1TokenRepo(getD1Binding),
		CalendarRepo:      repository.NewD1CalendarRepo(getD1Binding),
		CalendarEventRepo: repository.NewD1CalendarEventRepo(getD1Binding),
	})

	// Static files + SPA fallback
	subFS, err := fs.Sub(staticFS, "dist")
	if err != nil {
		panic(err)
	}
	fileServer := http.FileServer(http.FS(subFS))

	router.Get("/*", func(w http.ResponseWriter, r *http.Request) {
		path := strings.TrimPrefix(r.URL.Path, "/")
		if path != "" {
			if f, err := subFS.Open(path); err == nil {
				f.Close()
				fileServer.ServeHTTP(w, r)
				return
			}
		}
		// SPA fallback: serve index.html for client-side routes
		r.URL.Path = "/"
		fileServer.ServeHTTP(w, r)
	})

	workers.Serve(router)
}

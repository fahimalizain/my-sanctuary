package main

import (
	"embed"
	"io/fs"
	"net/http"
	"strings"

	"github.com/go-chi/chi/v5"
	"github.com/syumai/workers"
	"my-sanctuary/apps/api/handlers"
)

//go:embed dist/*
var staticFS embed.FS

func main() {
	router := chi.NewMux()

	// Register API routes from the shared handlers package
	handlers.RegisterRoutes(router)

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

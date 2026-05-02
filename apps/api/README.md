# API

The API is built with [Huma](https://huma.rocks/) on top of [Chi](https://github.com/go-chi/chi).

- **Chi** (`go-chi/chi/v5`) is the HTTP router. It handles path matching, URL parameters, and middleware chains.
- **Huma** (`danielgtaylor/huma/v2`) is the OpenAPI framework. It wraps Chi (via the `humachi` adapter) and provides input validation, output serialization, and auto-generated OpenAPI schemas.

Chi does the actual HTTP dispatch. Huma adds structured types, validation, and docs on top of it.

## Handlers

All route handlers live in `handlers/handlers.go` and are registered via `RegisterRoutes(router chi.Router)`. This package is imported by both the local development server (`main.go`) and the Cloudflare Worker deployment target (`apps/cloudflare-deploy`).

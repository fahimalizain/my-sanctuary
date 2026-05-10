module my-sanctuary/apps/cloudflare-deploy

go 1.26.2

require (
	github.com/go-chi/chi/v5 v5.2.5
	github.com/syumai/workers v0.33.0
	my-sanctuary/apps/api v0.0.0
)

require (
	github.com/danielgtaylor/huma/v2 v2.37.3 // indirect
	github.com/go-chi/cors v1.2.2 // indirect
	github.com/gorilla/securecookie v1.1.2 // indirect
	github.com/gorilla/sessions v1.4.0 // indirect
	golang.org/x/oauth2 v0.36.0 // indirect
)

replace my-sanctuary/apps/api => ../api

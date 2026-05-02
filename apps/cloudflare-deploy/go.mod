module my-sanctuary/apps/cloudflare-deploy

go 1.26.2

require (
	github.com/go-chi/chi/v5 v5.2.5
	github.com/syumai/workers v0.32.0
	my-sanctuary/apps/api v0.0.0
)

require github.com/danielgtaylor/huma/v2 v2.37.3 // indirect

replace my-sanctuary/apps/api => ../api

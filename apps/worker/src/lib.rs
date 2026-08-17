use worker::*;

fn health(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    Response::from_json(&api_core::HealthResponse {
        status: "ok".to_string(),
    })
}

fn version(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    Response::from_json(&api_core::VersionResponse {
        version: env!("APP_VERSION").to_string(),
    })
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get("/health", health)
        .get("/version", version)
        .run(req, env)
        .await
}

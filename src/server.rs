use crate::{
    application::{ApiError, ApiResponse, Application, ErrorCode, Request as OperationRequest},
    core::{Core, SyncEvent},
    mcp::Mcp,
};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn router(core: Arc<Core>, port: u16) -> Router {
    let application = Arc::new(Application::new(core.clone()));
    let mcp = Mcp::new(application.clone());
    let service = StreamableHttpService::new(
        move || Ok(mcp.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_legacy_session_mode(false),
    );
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/logo.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../web/logo.svg"),
                )
            }),
        )
        .route(
            "/rust.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../web/rust.svg"),
                )
            }),
        )
        .route(
            "/typescript.svg",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("../web/typescript.svg"),
                )
            }),
        )
        .route(
            "/api/execute",
            post(
                move |body: Result<
                    Json<OperationRequest>,
                    axum::extract::rejection::JsonRejection,
                >| {
                    let application = application.clone();
                    async move {
                        let result = match body {
                            Ok(Json(request)) => application.execute(request).await,
                            Err(error) => Err(ApiError::invalid(error.body_text())),
                        };
                        match result {
                            Ok(data) => Json(ApiResponse::Success { data }).into_response(),
                            Err(error) => {
                                let status = match error.code {
                                    ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
                                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                                };
                                (status, Json(ApiResponse::Failure { error })).into_response()
                            }
                        }
                    }
                },
            ),
        )
        .route("/api/status", get(status))
        .route("/api/workspaces", post(attach))
        .route("/api/companion", post(sync))
        .nest_service("/mcp", service)
        .with_state(core)
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            origin_guard(request, next, port)
        }))
}
async fn origin_guard(request: Request, next: Next, port: u16) -> Response {
    let hosts = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if !request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| hosts.iter().any(|x| x == h))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = request.headers().get("origin")
        && !origin
            .to_str()
            .ok()
            .is_some_and(|o| hosts.iter().any(|h| o == format!("http://{h}")))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}
async fn status(State(core): State<Arc<Core>>) -> Json<Value> {
    Json(core.status().await)
}
async fn attach(State(core): State<Arc<Core>>, Json(value): Json<Value>) -> Response {
    match core
        .connect(value["workspace"].as_str().unwrap_or(""))
        .await
    {
        Ok(w) => Json(w.status().await).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}
async fn sync(State(core): State<Arc<Core>>, Json(event): Json<SyncEvent>) -> Response {
    let result = async { core.workspace(&event.workspace).await?.sync(event).await }.await;
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":e.to_string()})),
        )
            .into_response(),
    }
}

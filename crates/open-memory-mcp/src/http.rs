//! Streamable-HTTP MCP transport. Behind the `mcp-http` feature.
//!
//! Mounts a single POST endpoint at `/mcp`. Each request body is one
//! JSON-RPC 2.0 message; the response body is the matching JSON-RPC
//! response (or empty 204 for notifications). This is the simplest
//! profile of MCP-over-HTTP — no SSE, no session resumption — sufficient
//! for an OpenClaw HTTP client.
//!
//! For production deployments behind an HTTP proxy, configure CORS via
//! [`build_router`]'s extension point and run the returned axum router
//! under any tokio-friendly server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::OpenMemoryMcpServer;

/// Build the axum router for the HTTP transport. Wraps `server` in an
/// `Arc` so it can be cloned cheaply per request and serves both `POST
/// /mcp` (a JSON-RPC envelope) and `GET /healthz` (200 OK).
pub fn build_router(server: OpenMemoryMcpServer) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::POST, Method::GET])
        .allow_origin(Any)
        .allow_headers(Any);

    Router::new()
        .route("/mcp", post(handle_mcp))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(Arc::new(server))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}

/// Bind the router on `addr` and serve forever. Returns when the OS
/// shuts the listening socket.
pub async fn serve(server: OpenMemoryMcpServer, addr: SocketAddr) -> anyhow::Result<()> {
    let router = build_router(server);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn handle_mcp(
    State(server): State<Arc<OpenMemoryMcpServer>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !is_json_content_type(&headers) {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            JsonRpcError::invalid_params("Content-Type must be application/json"),
        );
    }

    let request: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(req) => req,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                JsonRpcError::parse_error(format!("parse error: {e}")),
            )
        }
    };

    match server.handle(request) {
        Some(response) => json_response(StatusCode::OK, response),
        None => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap(),
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("application/json"))
}

fn json_response(status: StatusCode, response: JsonRpcResponse) -> Response {
    let mut r = (status, Json(response)).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );
    r
}

fn error_response(status: StatusCode, err: JsonRpcError) -> Response {
    let body = JsonRpcResponse::error(None, err);
    json_response(status, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use open_memory_core::config::Config;
    use open_memory_graph::MemoryStore;

    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    async fn post_mcp(router: Router, body: &str) -> (StatusCode, String) {
        let resp = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
        (status, text)
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let router = build_router(server());
        let resp = router
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn initialize_round_trip() {
        let router = build_router(server());
        let (status, body) = post_mcp(
            router,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"protocolVersion\""));
        assert!(body.contains("\"name\":\"open-memory\""));
    }

    #[tokio::test]
    async fn tools_list_round_trip() {
        let router = build_router(server());
        let (status, body) = post_mcp(
            router,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("open_memory_remember"));
    }

    #[tokio::test]
    async fn notification_returns_204() {
        let router = build_router(server());
        let resp = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn invalid_json_body_returns_400() {
        let router = build_router(server());
        let (status, body) = post_mcp(router, "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("-32700"));
    }

    #[tokio::test]
    async fn missing_content_type_returns_415() {
        let router = build_router(server());
        let resp = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}

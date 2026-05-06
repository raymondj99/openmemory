//! Streamable-HTTP MCP transport. Behind the `mcp-http` feature.
//!
//! Mounts a single POST endpoint at `/mcp`. Each request body is one
//! JSON-RPC 2.0 message; the response body is the matching JSON-RPC
//! response (or empty 204 for notifications). This is the simplest
//! profile of MCP-over-HTTP — no SSE, no session resumption — sufficient
//! for an OpenClaw HTTP client.
//!
//! # Authentication
//!
//! By default, anyone reachable on the bind address can call `/mcp`.
//! For deployments behind anything other than `127.0.0.1`, set
//! [`BEARER_TOKEN_ENV`] (`OPEN_MEMORY_HTTP_TOKEN`) before launching the
//! server; [`serve`] then requires every `/mcp` request to carry a
//! matching `Authorization: Bearer <token>` header. The `/healthz`
//! endpoint is never gated so liveness probes from a load balancer
//! keep working without the secret. When the env var is unset (or
//! empty), the server logs a warning and serves unauthenticated.
//!
//! # Deployments
//!
//! The router exposed by [`build_router`] applies a permissive
//! [`tower_http::cors::CorsLayer`] suitable for development. For
//! production, wrap the returned router in your own middleware stack
//! (origin allow-list, rate limiter, request-size limit, …) and run
//! it under any tokio-friendly server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::OpenMemoryMcpServer;

/// Environment variable consulted by [`serve`] to decide whether to
/// require bearer-token auth on the HTTP transport. Set it to a
/// non-empty string to enable; unset or empty leaves the server
/// unauthenticated.
pub const BEARER_TOKEN_ENV: &str = "OPEN_MEMORY_HTTP_TOKEN";

/// Optional shared secret required on the `Authorization: Bearer …`
/// header for every `/mcp` request. Cloning is cheap (`Arc<str>`); the
/// stored token is compared against incoming headers in constant time.
#[derive(Clone)]
pub struct BearerToken {
    expected: Arc<str>,
}

impl BearerToken {
    /// Wrap a raw token. The token is stored verbatim — callers should
    /// trim whitespace and pre-validate format if needed.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            expected: Arc::from(token.into()),
        }
    }

    /// Read [`BEARER_TOKEN_ENV`] from the process environment. Returns
    /// `None` when the variable is unset, empty, or contains only
    /// whitespace; otherwise wraps the trimmed value.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        match std::env::var(BEARER_TOKEN_ENV) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(Self::new(trimmed))
                }
            }
            Err(_) => None,
        }
    }

    /// Constant-time comparison against `candidate`. The early
    /// length-mismatch return leaks the expected token's length, which
    /// is acceptable for a server-side bearer-token check.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.expected.as_bytes();
        let provided = candidate.as_bytes();
        if expected.len() != provided.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for (x, y) in expected.iter().zip(provided.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the secret. Fixed-shape Debug output keeps tracing
        // diagnostics safe even if a span captures the field.
        f.debug_struct("BearerToken")
            .field("expected", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
struct HttpState {
    server: Arc<OpenMemoryMcpServer>,
    auth: Option<BearerToken>,
}

/// Build the axum router for the HTTP transport.
///
/// Routes:
///
/// * `POST /mcp` — JSON-RPC 2.0 envelope, gated by bearer-token auth
///   when `auth` is `Some`.
/// * `GET /healthz` — always 200 OK, never auth-gated. Suitable for
///   load-balancer health checks.
///
/// The server is wrapped in an `Arc` so cloning the state per request
/// is cheap.
pub fn build_router(server: OpenMemoryMcpServer, auth: Option<BearerToken>) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([Method::POST, Method::GET])
        .allow_origin(Any)
        .allow_headers(Any);

    let state = HttpState {
        server: Arc::new(server),
        auth,
    };

    Router::new()
        .route("/mcp", post(handle_mcp))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
}

/// Bind the router on `addr` and serve forever. Reads
/// [`BEARER_TOKEN_ENV`] from the process environment to decide whether
/// to enable bearer-token auth, logs the resulting policy, and returns
/// when the OS shuts the listening socket.
pub async fn serve(server: OpenMemoryMcpServer, addr: SocketAddr) -> anyhow::Result<()> {
    let auth = BearerToken::from_env();
    if auth.is_some() {
        tracing::info!(
            target: "open_memory_mcp::http",
            env = BEARER_TOKEN_ENV,
            "HTTP transport: bearer-token auth enabled",
        );
    } else {
        tracing::warn!(
            target: "open_memory_mcp::http",
            env = BEARER_TOKEN_ENV,
            "HTTP transport: no bearer-token auth configured \
             (anyone reachable on the bind address can call /mcp); \
             set OPEN_MEMORY_HTTP_TOKEN to require auth",
        );
    }
    let router = build_router(server, auth);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn handle_mcp(State(state): State<HttpState>, headers: HeaderMap, body: String) -> Response {
    if let Some(expected) = state.auth.as_ref() {
        if !is_authorized(&headers, expected) {
            return unauthorized_response();
        }
    }

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

    match state.server.handle(request) {
        Some(response) => json_response(StatusCode::OK, response),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("application/json"))
}

fn is_authorized(headers: &HeaderMap, expected: &BearerToken) -> bool {
    let raw = match headers.get(header::AUTHORIZATION) {
        Some(v) => v,
        None => return false,
    };
    let header_str = match raw.to_str() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let token = match header_str.strip_prefix("Bearer ") {
        Some(t) => t.trim(),
        None => return false,
    };
    expected.matches(token)
}

fn json_response(status: StatusCode, response: JsonRpcResponse) -> Response {
    let mut r = (status, Json(response)).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

fn error_response(status: StatusCode, err: JsonRpcError) -> Response {
    let body = JsonRpcResponse::error(None, err);
    json_response(status, body)
}

fn unauthorized_response() -> Response {
    let body = JsonRpcResponse::error(
        None,
        JsonRpcError::invalid_request("missing or invalid bearer token"),
    );
    let mut r = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    let h = r.headers_mut();
    h.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use open_memory_core::config::Config;
    use open_memory_graph::MemoryStore;

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn server() -> OpenMemoryMcpServer {
        let store = MemoryStore::open_in_memory(&Config::default()).unwrap();
        OpenMemoryMcpServer::from_memory(Config::default(), Arc::new(store))
    }

    fn unauth_router() -> Router {
        build_router(server(), None)
    }

    fn auth_router(token: &str) -> Router {
        build_router(server(), Some(BearerToken::new(token)))
    }

    async fn post_mcp_with(
        router: Router,
        body: &str,
        authorization: Option<&str>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("Content-Type", "application/json");
        if let Some(value) = authorization {
            builder = builder.header("Authorization", value);
        }
        let resp = router
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap_or_default();
        (status, text)
    }

    async fn post_mcp(router: Router, body: &str) -> (StatusCode, String) {
        post_mcp_with(router, body, None).await
    }

    const INIT_REQ: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let resp = unauth_router()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn initialize_round_trip() {
        let (status, body) = post_mcp(unauth_router(), INIT_REQ).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"protocolVersion\""));
        assert!(body.contains("\"name\":\"open-memory\""));
    }

    #[tokio::test]
    async fn tools_list_round_trip() {
        let (status, body) = post_mcp(
            unauth_router(),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("open_memory_remember"));
    }

    #[tokio::test]
    async fn notification_returns_204() {
        let resp = unauth_router()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn invalid_json_body_returns_400() {
        let (status, body) = post_mcp(unauth_router(), "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("-32700"));
    }

    #[tokio::test]
    async fn missing_content_type_returns_415() {
        let resp = unauth_router()
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

    // ----- bearer-token auth -----

    #[tokio::test]
    async fn auth_required_request_without_header_returns_401() {
        let (status, body) = post_mcp_with(auth_router("super-secret"), INIT_REQ, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Must surface a JSON-RPC envelope so MCP clients can parse the
        // response and react.
        assert!(body.contains("-32600"));
        assert!(body.contains("missing or invalid bearer token"));
    }

    #[tokio::test]
    async fn auth_required_request_with_wrong_token_returns_401() {
        let (status, _) = post_mcp_with(
            auth_router("super-secret"),
            INIT_REQ,
            Some("Bearer not-the-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_required_empty_bearer_token_returns_401() {
        // `Authorization: Bearer ` (trailing space, no token) and the
        // unspaced `Authorization: Bearer` should both fail closed —
        // they leave the candidate empty, which never matches a
        // configured non-empty token.
        for header in ["Bearer ", "Bearer", "Bearer    "] {
            let (status, _) =
                post_mcp_with(auth_router("super-secret"), INIT_REQ, Some(header)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "header {header:?} should not authenticate"
            );
        }
    }

    #[tokio::test]
    async fn auth_required_request_without_bearer_prefix_returns_401() {
        let (status, _) = post_mcp_with(
            auth_router("super-secret"),
            INIT_REQ,
            Some("Basic c3VwZXItc2VjcmV0"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_required_request_with_correct_token_returns_200() {
        let (status, body) = post_mcp_with(
            auth_router("super-secret"),
            INIT_REQ,
            Some("Bearer super-secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"protocolVersion\""));
    }

    #[tokio::test]
    async fn auth_required_unauthorized_response_advertises_bearer() {
        let resp = auth_router("super-secret")
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header("Content-Type", "application/json")
                    .body(Body::from(INIT_REQ.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get(header::WWW_AUTHENTICATE)
                .map(|v| v.to_str().unwrap_or_default()),
            Some("Bearer"),
        );
    }

    #[tokio::test]
    async fn healthz_skips_auth_check() {
        let resp = auth_router("super-secret")
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn bearer_token_matches_constant_time() {
        let t = BearerToken::new("hunter2");
        assert!(t.matches("hunter2"));
        assert!(!t.matches("hunter3"));
        // Length mismatch.
        assert!(!t.matches("hunter22"));
        assert!(!t.matches("hunter"));
        // Empty.
        assert!(!t.matches(""));
    }

    #[test]
    fn bearer_token_debug_redacts_secret() {
        let t = BearerToken::new("super-secret");
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("super-secret"));
        assert!(dbg.contains("redacted"));
    }
}

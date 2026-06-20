// MCP SSE + Streamable HTTP transport layer.
// Reuses the core MCP JSON-RPC handler from crate::mcp.
//
// Uses static globals instead of axum Router state because axum 0.7's serve()
// only accepts Router<()>. State is set once via init_transport_state() before
// the server starts.

use axum::{
    extract::{Query, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        Json, Response,
    },
    routing::{get, post},
    Router,
};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};

use crate::db::Database;
use crate::mcp::{self, JsonRpcRequest, MCPState};

/// Transport mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportMode {
    Sse,
    Http,
}

/// Shared state for the MCP HTTP transport, stored as a global static.
struct TransportState {
    db: Arc<Mutex<Database>>,
    mcp_state: Arc<Mutex<MCPState>>,
    sse_tx: broadcast::Sender<String>,
}

static TRANSPORT_STATE: OnceLock<TransportState> = OnceLock::new();

/// Initialize the global transport state. Must be called before starting the server.
pub fn init_transport_state(db: Arc<Mutex<Database>>) {
    let (sse_tx, _) = broadcast::channel::<String>(256);
    let state = TransportState {
        db,
        mcp_state: Arc::new(Mutex::new(MCPState::new())),
        sse_tx,
    };
    TRANSPORT_STATE.set(state).ok();
}

/// Query params for POST /message
#[derive(Debug, Deserialize)]
struct MessageParams {
    #[serde(default)]
    #[allow(dead_code)]
    session_id: Option<String>,
}

/// Query params for GET /sse
#[derive(Debug, Deserialize)]
struct SseParams {
    #[serde(default)]
    #[allow(dead_code)]
    session_id: Option<String>,
}

/// Build the MCP HTTP transport router.
///
/// When `auth_token` is `Some`, every route requires a matching
/// `Authorization: Bearer <token>` header and returns 401 otherwise.
/// When `None`, auth is skipped entirely (backward compatible).
pub fn build_transport_router(mode: TransportMode, auth_token: Option<String>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let mut router = Router::new().route("/message", post(handle_message));

    if mode == TransportMode::Sse {
        router = router.route("/sse", get(handle_sse));
    }

    router
        .route_layer(middleware::from_fn_with_state(auth_token, auth_middleware))
        .layer(cors)
}

/// Middleware: require a Bearer token if one is configured.
/// Skips auth when `auth_token` is `None`.
async fn auth_middleware(
    State(auth_token): State<Option<String>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = match auth_token {
        Some(token) => token,
        None => return Ok(next.run(request).await),
    };

    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    if let Some(auth) = auth_header {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if token == expected {
                return Ok(next.run(request).await);
            }
        }
    }

    let mut response = Response::new(axum::body::Body::from(
        json!({"error": "unauthorized", "message": "Valid Bearer token required"}).to_string(),
    ));
    *response.status_mut() = StatusCode::UNAUTHORIZED;
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static("Bearer"),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

/// Helper to get a reference to the global state.
fn get_state() -> Result<&'static TransportState, StatusCode> {
    TRANSPORT_STATE.get().ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// Handle POST /message — JSON-RPC request → JSON-RPC response.
async fn handle_message(
    Query(params): Query<MessageParams>,
    axum::Json(request): axum::Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let req: JsonRpcRequest = match serde_json::from_value(request) {
        Ok(r) => r,
        Err(e) => {
            return Ok(Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32700, "message": format!("Parse error: {}", e)}
            })));
        }
    };

    let state = get_state()?;
    let mut mcp_state = state
        .mcp_state
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let response = mcp::handle_request(&req, &mut mcp_state, &db);

    match response {
        Some(resp) => {
            if params.session_id.is_some() {
                let resp_str = serde_json::to_string(&resp).unwrap_or_default();
                let _ = state.sse_tx.send(resp_str);
            }
            Ok(Json(
                serde_json::to_value(resp)
                    .unwrap_or(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Serialization error"}})),
            ))
        }
        None => Ok(Json(json!({"jsonrpc": "2.0", "id": null, "result": null}))),
    }
}

/// Handle GET /sse — Server-Sent Events stream.
async fn handle_sse(
    Query(_params): Query<SseParams>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode> {
    let state = get_state()?;
    let rx = state.sse_tx.subscribe();

    let stream = async_stream::stream! {
        yield Ok(Event::default()
            .event("endpoint")
            .data("/message"));

        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    yield Ok(Event::default()
                        .event("message")
                        .data(msg));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    ))
}


#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt; // for `oneshot`

    fn message_request(auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/message")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = auth {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {}", token));
        }
        builder
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn no_token_configured_allows_request() {
        // When auth_token is None, requests pass through (state may be missing,
        // which yields 503 — but crucially NOT 401).
        let router = build_transport_router(TransportMode::Http, None);
        let resp = router.oneshot(message_request(None)).await.unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_token_is_rejected() {
        let router = build_transport_router(TransportMode::Http, Some("secret".to_string()));
        let resp = router.oneshot(message_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_token_is_rejected() {
        let router = build_transport_router(TransportMode::Http, Some("secret".to_string()));
        let resp = router
            .oneshot(message_request(Some("wrong")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn correct_token_passes_auth() {
        // A correct token must clear the auth layer. State isn't initialized in
        // this unit test, so the handler returns 503 — the point is it is NOT 401.
        let router = build_transport_router(TransportMode::Http, Some("secret".to_string()));
        let resp = router
            .oneshot(message_request(Some("secret")))
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthorized_response_sets_www_authenticate() {
        let router = build_transport_router(TransportMode::Http, Some("secret".to_string()));
        let resp = router.oneshot(message_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().contains_key(header::WWW_AUTHENTICATE));
    }
}

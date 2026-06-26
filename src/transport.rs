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
use std::sync::{Arc, OnceLock};
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
    // #210: no Mutex — Database is now Sync (internally pooled), and MCPState
    // tracks `initialized` via an AtomicBool, so concurrent requests share these
    // by shared reference and read in parallel instead of serializing on a lock.
    db: Arc<Database>,
    mcp_state: Arc<MCPState>,
    sse_tx: broadcast::Sender<String>,
}

static TRANSPORT_STATE: OnceLock<TransportState> = OnceLock::new();

/// Initialize the global transport state. Must be called before starting the server.
pub fn init_transport_state(db: Arc<Database>) {
    let (sse_tx, _) = broadcast::channel::<String>(256);
    let state = TransportState {
        db,
        mcp_state: Arc::new(MCPState::new()),
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
    // #210: the handler is blocking and can make synchronous LLM round-trips
    // (mimir_ask / mimir_synthesize), so run it on the blocking thread pool to
    // keep the Tokio async workers (SSE streams, connection accept) free (#217).
    // No locks: the DB checks out its own pooled connection and MCPState's
    // `initialized` is atomic, so concurrent requests run in parallel.
    let response = tokio::task::spawn_blocking(move || {
        mcp::handle_request(&req, &state.mcp_state, &state.db)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

    /// #223: concurrent-client load test for the DB connection pool, driven
    /// through the REAL HTTP transport — the same `init_transport_state` +
    /// `build_transport_router` + `axum::serve` path `main.rs` uses — rather
    /// than direct `Database` calls. This exercises the full request path under
    /// contention: `handle_message` -> `spawn_blocking` -> `mcp::handle_request`
    /// -> `call_tool` -> a pooled connection.
    ///
    /// `#[ignore]` on purpose: this is a load/soak test, not a CI correctness
    /// gate (the durability/throughput characteristics under contention "can't
    /// be proven by CI" — see #223). Run it explicitly and sweep the pool knobs:
    ///
    /// ```text
    /// cargo test --release pool_load_test_http_transport -- --ignored --nocapture
    ///
    /// # sweep: small pool, default busy_timeout, more clients
    /// MIMIR_POOL_MAX_SIZE=4 MIMIR_BUSY_TIMEOUT_MS=5000 MIMIR_LOADTEST_CLIENTS=32 \
    ///   cargo test --release pool_load_test_http_transport -- --ignored --nocapture
    /// ```
    ///
    /// Tunables (env): `MIMIR_LOADTEST_CLIENTS` (default 16),
    /// `MIMIR_LOADTEST_WRITES` / `MIMIR_LOADTEST_READS` per client (default 25 / 75),
    /// plus the pool's `MIMIR_POOL_MAX_SIZE` / `MIMIR_BUSY_TIMEOUT_MS`
    /// (consumed by `Database::open`).
    ///
    /// Asserts the four properties #223 calls out: no `database is locked` /
    /// `SQLITE_BUSY` after the busy_timeout, no lost writes (final row count ==
    /// writes that returned success), no deadlock (the run completes and joins),
    /// and reports p50/p99/max latency so the operator can judge tail behavior.
    #[test]
    #[ignore = "load test: run explicitly with --ignored --nocapture"]
    fn pool_load_test_http_transport() {
        use crate::db::Database;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        use std::time::Instant;

        fn env_usize(key: &str, default: usize) -> usize {
            std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
        }

        // Classify one response body. A pooled-write/lock failure surfaces as an
        // MCP tool error (`isError:true`) whose text carries rusqlite's message,
        // so we scan for the SQLITE_BUSY signature explicitly and bucket the rest.
        fn classify(
            text: &str,
            lock: &AtomicU64,
            other: &AtomicU64,
            writes_ok: &AtomicU64,
            is_write: bool,
        ) {
            let lower = text.to_lowercase();
            let is_lock =
                lower.contains("database is locked") || lower.contains("sqlite_busy");
            let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            let is_err = is_lock
                || text.starts_with("TRANSPORT_ERROR")
                || v.get("error").is_some()
                || v.pointer("/result/isError").and_then(|b| b.as_bool()).unwrap_or(false);
            if is_lock {
                lock.fetch_add(1, Ordering::Relaxed);
            } else if is_err {
                other.fetch_add(1, Ordering::Relaxed);
            } else if is_write {
                writes_ok.fetch_add(1, Ordering::Relaxed);
            }
        }

        let clients = env_usize("MIMIR_LOADTEST_CLIENTS", 16);
        let writes_per = env_usize("MIMIR_LOADTEST_WRITES", 25);
        let reads_per = env_usize("MIMIR_LOADTEST_READS", 75);

        let path = std::env::temp_dir()
            .join(format!("mimir-loadtest-{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap().to_string();
        let db = Database::open(&path_str).expect("open load-test db");
        init_transport_state(Arc::new(db));

        // Real HTTP server on an ephemeral port (mirrors main.rs wiring).
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind ephemeral port");
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            let router = build_transport_router(TransportMode::Http, None);
            axum::serve(listener, router).await.unwrap();
        });
        let addr = addr_rx.recv().expect("server address");
        let base = format!("http://{}/message", addr);

        // One MCP handshake; `initialized` lives in the shared global state, so a
        // single initialize unblocks tools/call for every client.
        let init = ureq::post(&base)
            .set("Content-Type", "application/json")
            .send_string(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}
                })
                .to_string(),
            );
        assert!(init.is_ok(), "initialize failed: {:?}", init.err());

        let lock_errors = Arc::new(AtomicU64::new(0));
        let other_errors = Arc::new(AtomicU64::new(0));
        let writes_ok = Arc::new(AtomicU64::new(0));

        let start = Instant::now();
        let mut handles = Vec::new();
        for c in 0..clients {
            let base = base.clone();
            let lock_errors = Arc::clone(&lock_errors);
            let other_errors = Arc::clone(&other_errors);
            let writes_ok = Arc::clone(&writes_ok);
            handles.push(std::thread::spawn(move || {
                let mut latencies: Vec<u128> = Vec::with_capacity(writes_per + 2 * reads_per);
                let call = |name: &str, args: serde_json::Value| -> (String, u128) {
                    let t = Instant::now();
                    let body = serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": name, "arguments": args}
                    });
                    let text = match ureq::post(&base)
                        .set("Content-Type", "application/json")
                        .send_string(&body.to_string())
                    {
                        Ok(resp) => resp.into_string().unwrap_or_default(),
                        Err(ureq::Error::Status(_, resp)) => {
                            resp.into_string().unwrap_or_default()
                        }
                        Err(e) => format!("TRANSPORT_ERROR: {}", e),
                    };
                    (text, t.elapsed().as_micros())
                };

                // Interleave writes and reads so the two contend on the pool.
                let ops = writes_per.max(reads_per);
                for i in 0..ops {
                    if i < writes_per {
                        // High-entropy unique content so each write is a real
                        // create — mimir_remember dedups bodies above 70% trigram
                        // similarity, so near-identical payloads would collapse
                        // and `persisted == issued` would no longer test durability.
                        let nonce = format!(
                            "{}{}",
                            uuid::Uuid::new_v4().simple(),
                            uuid::Uuid::new_v4().simple()
                        );
                        let (text, us) = call("mimir_remember", serde_json::json!({
                            "category": "loadtest",
                            "key": format!("c{}-w{}", c, i),
                            "body_json": format!("{{\"content\":\"{}\"}}", nonce),
                        }));
                        latencies.push(us);
                        classify(&text, &lock_errors, &other_errors, &writes_ok, true);
                    }
                    if i < reads_per {
                        let (text, us) = call("mimir_recall", serde_json::json!({
                            "query": "client", "category": "loadtest", "limit": 10
                        }));
                        latencies.push(us);
                        classify(&text, &lock_errors, &other_errors, &writes_ok, false);

                        let (text2, us2) = call("mimir_context", serde_json::json!({}));
                        latencies.push(us2);
                        classify(&text2, &lock_errors, &other_errors, &writes_ok, false);
                    }
                }
                latencies
            }));
        }

        let mut all: Vec<u128> = Vec::new();
        for h in handles {
            all.extend(h.join().expect("client thread panicked (possible deadlock)"));
        }
        let elapsed = start.elapsed();

        all.sort_unstable();
        let pct = |p: f64| -> u128 {
            if all.is_empty() {
                return 0;
            }
            let idx = (((all.len() - 1) as f64) * p).round() as usize;
            all[idx]
        };
        let lock = lock_errors.load(Ordering::Relaxed);
        let other = other_errors.load(Ordering::Relaxed);
        let ok_writes = writes_ok.load(Ordering::Relaxed);
        let issued_writes = (clients * writes_per) as u64;

        // Independently verify no lost writes: reopen the file with a raw
        // connection and count the rows that actually persisted.
        let verify = rusqlite::Connection::open(&path_str)
            .expect("reopen for verification");
        let persisted: i64 = verify
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE category = 'loadtest'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        drop(verify);

        eprintln!(
            "\n#223 pool load test\n\
             clients={clients} writes/client={writes_per} reads/client={reads_per}\n\
             pool max_size={} busy_timeout={}ms\n\
             requests={} wall={:.2}s throughput={:.0} req/s\n\
             latency p50={}us p99={}us max={}us\n\
             lock_errors={lock} other_errors={other}\n\
             writes: issued={issued_writes} ok={ok_writes} persisted={persisted}",
            std::env::var("MIMIR_POOL_MAX_SIZE").unwrap_or_else(|_| "16".into()),
            std::env::var("MIMIR_BUSY_TIMEOUT_MS").unwrap_or_else(|_| "5000".into()),
            all.len(),
            elapsed.as_secs_f64(),
            all.len() as f64 / elapsed.as_secs_f64().max(1e-9),
            pct(0.50),
            pct(0.99),
            all.last().copied().unwrap_or(0),
        );

        let _ = std::fs::remove_file(&path_str);

        // The four properties #223 asks us to prove:
        assert_eq!(lock, 0, "SQLITE_BUSY / 'database is locked' after busy_timeout");
        assert_eq!(other, 0, "unexpected tool/transport errors under load");
        assert_eq!(
            persisted, issued_writes as i64,
            "lost writes: {issued_writes} issued, {persisted} persisted"
        );
        assert_eq!(
            ok_writes, issued_writes,
            "every issued write should have returned success"
        );
        // (Reaching here at all proves no deadlock — all client threads joined.)
    }
}

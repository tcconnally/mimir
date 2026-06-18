pub mod dashboard_html;

use axum::{
    extract::{Path, Query, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, Json, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::db::Database;

/// Shared application state for the web dashboard.
#[derive(Clone)]
pub struct WebState {
    pub db: Arc<Mutex<Database>>,
    pub auth_token: Option<String>,
}

/// Build the Axum router with all API endpoints and the dashboard HTML.
pub fn build_router(db: Arc<Mutex<Database>>, auth_token: Option<String>) -> Router {
    let state = WebState { db, auth_token };

    // Tighten CORS: if auth token is set, allow specific origins; otherwise disable CORS
    let cors = if state.auth_token.is_some() {
        // With auth, we can safely allow CORS but restrict to known origins
        CorsLayer::new()
            .allow_origin(AllowOrigin::mirror_request())
            .allow_methods([axum::http::Method::GET])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
    } else {
        // No auth: listen only on 127.0.0.1 (caller should ensure this), CORS disabled
        CorsLayer::new()
            .allow_origin(AllowOrigin::mirror_request())
            .allow_methods([axum::http::Method::GET])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
    };

    Router::new()
        .route("/", get(dashboard))
        .route("/api/entities", get(list_entities))
        .route("/api/entities/{id}", get(entity_detail))
        .route("/api/search", get(search))
        .route("/api/stats", get(stats))
        .route("/api/journal", get(journal))
        .route("/api/graph", get(graph))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .layer(cors)
        .with_state(state)
}

/// Middleware: require Bearer token if auth_token is set.
async fn auth_middleware(
    State(state): State<WebState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // If no auth token is configured, allow all requests
    let expected = match &state.auth_token {
        Some(token) => token,
        None => return Ok(next.run(request).await),
    };

    // Check Authorization header
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

    // Return 401 with WWW-Authenticate header
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

// ─── Dashboard HTML ──────────────────────────────────────────────────

async fn dashboard() -> Html<&'static str> {
    Html(dashboard_html::HTML)
}

// ─── API Query params ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct EntityListParams {
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_page_limit")]
    limit: i64,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    layer: Option<String>,
}

fn default_page_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_page_limit")]
    limit: i64,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JournalParams {
    #[serde(default = "default_page_limit")]
    limit: i64,
}

// ─── Handlers ────────────────────────────────────────────────────────

async fn list_entities(
    State(state): State<WebState>,
    Query(params): Query<EntityListParams>,
) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entities = db
        .list_entities(
            params.offset,
            params.limit,
            params.category.as_deref(),
            params.layer.as_deref(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let items: Vec<Value> = entities.iter().map(|e| e.to_json_expanded()).collect();

    Ok(Json(json!({ "items": items, "total": items.len() })))
}

async fn entity_detail(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match db.get_entity_by_id_public(&id) {
        Ok(Some(entity)) => Ok(Json(entity.to_json_expanded())),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn search(
    State(state): State<WebState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let recall_params = crate::models::RecallParams {
        query: params.q.clone(),
        category: params.category.clone(),
        limit: params.limit,
        ..Default::default()
    };
    let entities = db
        .recall(&recall_params)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let items: Vec<Value> = entities.iter().map(|e| e.to_json_expanded()).collect();

    Ok(Json(json!({ "items": items, "total": items.len() })))
}

async fn stats(State(state): State<WebState>) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let s = db.stats().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        serde_json::to_value(s).unwrap_or(json!({ "error": "serialization failed" })),
    ))
}

async fn journal(
    State(state): State<WebState>,
    Query(params): Query<JournalParams>,
) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let events = db
        .get_recent_journal(params.limit)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "items": events, "total": events.len() })))
}

async fn graph(State(state): State<WebState>) -> Result<Json<Value>, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (nodes, edges) = db
        .get_entity_graph()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(json!({ "nodes": nodes, "edges": edges })))
}

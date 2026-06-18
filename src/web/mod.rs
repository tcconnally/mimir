pub mod dashboard_html;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, Json},
    routing::get,
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower_http::cors::{Any, CorsLayer};

use crate::db::Database;

/// Shared application state for the web dashboard.
#[derive(Clone)]
pub struct WebState {
    pub db: Arc<Mutex<Database>>,
}

/// Build the Axum router with all API endpoints and the dashboard HTML.
pub fn build_router(db: Arc<Mutex<Database>>) -> Router {
    let state = WebState { db };
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(dashboard))
        .route("/api/entities", get(list_entities))
        .route("/api/entities/{id}", get(entity_detail))
        .route("/api/search", get(search))
        .route("/api/stats", get(stats))
        .route("/api/journal", get(journal))
        .route("/api/graph", get(graph))
        .layer(cors)
        .with_state(state)
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

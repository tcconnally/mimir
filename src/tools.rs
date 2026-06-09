use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{now_ms, Database, MemoryItem, MemoryLink};

#[derive(Debug, Deserialize)]
pub struct RecallArgs {
    pub query: String,
    #[serde(default)]
    pub memory_types: Vec<String>,
    #[serde(default = "default_max_results")]
    pub max_results: i64,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    #[serde(default)]
    pub include_federation: bool,
    #[serde(default)]
    pub filters: Option<Value>,
    #[serde(default)]
    pub min_decay_score: f64,
    #[serde(default)]
    pub topic_path: Option<String>,
}

fn default_max_results() -> i64 {
    10
}

#[derive(Debug, Deserialize)]
pub struct StoreArgs {
    pub content: String,
    #[serde(default = "default_memory_type")]
    pub memory_type: String,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    #[serde(default)]
    pub tags: Option<Value>,
    #[serde(default)]
    pub links: Option<Vec<LinkArg>>,
    #[serde(default = "default_importance")]
    pub importance: f64,
    #[serde(default)]
    pub topic_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinkArg {
    pub target_id: String,
    #[serde(default)]
    pub relationship: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_memory_type() -> String {
    "insight".to_string()
}

fn default_importance() -> f64 {
    0.5
}

fn default_weight() -> f64 {
    0.5
}

pub fn handle_recall(db: &Database, args: Value) -> Result<String, String> {
    let recall_args: RecallArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid recall arguments: {}", e))?;

    let items = db
        .recall(
            &recall_args.query,
            &recall_args.memory_types,
            recall_args.max_results,
            &recall_args.workspace_hash,
            recall_args.include_federation,
            &recall_args.filters,
            recall_args.min_decay_score,
            &recall_args.topic_path,
        )
        .map_err(|e| format!("Recall failed: {}", e))?;

    let result = json!({ "items": items });
    Ok(result.to_string())
}

pub fn handle_store(db: &Database, args: Value) -> Result<String, String> {
    let store_args: StoreArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid store arguments: {}", e))?;

    let raw_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let id = format!("mem-{}", &raw_id[..12]);

    let tags = store_args.tags.unwrap_or(json!({}));
    let links: Vec<MemoryLink> = store_args
        .links
        .unwrap_or_default()
        .into_iter()
        .map(|l| MemoryLink {
            target_id: l.target_id,
            relationship: l.relationship,
            weight: l.weight,
        })
        .collect();

    let summary = if store_args.content.len() > 80 {
        Some(store_args.content[..80].to_string())
    } else {
        Some(store_args.content.clone())
    };

    let now = now_ms();
    let item = MemoryItem {
        id: id.clone(),
        content: store_args.content,
        memory_type: store_args.memory_type,
        summary,
        relevance: store_args.importance,
        decay_score: 1.0,
        retrieval_count: 0,
        layer: "working".to_string(),
        topic_path: store_args.topic_path.unwrap_or_default(),
        created_at_unix_ms: now,
        last_accessed_unix_ms: now,
        links,
        workspace_hash: store_args.workspace_hash.unwrap_or_default(),
        tags,
        source: "mneme".to_string(),
        verified: false,
    };

    db.store(&item)
        .map_err(|e| format!("Store failed: {}", e))?;

    let result = json!({ "success": true, "id": id });
    Ok(result.to_string())
}

pub fn handle_health(db: &Database) -> String {
    if db.health_check() {
        json!({ "status": "healthy" }).to_string()
    } else {
        json!({ "status": "unhealthy" }).to_string()
    }
}

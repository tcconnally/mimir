use serde::{Deserialize, Serialize};

/// An entity stored in the entities table.
/// Idempotent by UNIQUE(category, key) — INSERT OR REPLACE semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub category: String,
    pub key: String,
    pub body_json: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(rename = "type", default = "default_entity_type")]
    pub entity_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_decay_score")]
    pub decay_score: f64,
    #[serde(default)]
    pub retrieval_count: i64,
    #[serde(default = "default_layer")]
    pub layer: String,
    #[serde(default)]
    pub topic_path: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub archive_reason: String,
    #[serde(default)]
    pub links: Vec<MemoryLink>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default = "default_source")]
    pub source: String,
    pub created_at_unix_ms: i64,
    pub last_accessed_unix_ms: i64,
}

impl Entity {
    pub fn to_json_expanded(&self) -> serde_json::Value {
        let mut val = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        if let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(&self.body_json)
        {
            if let Some(obj) = val.as_object_mut() {
                for (k, v) in map {
                    if k != "id" && k != "category" && k != "key" && k != "body_json" && k != "type"
                    {
                        obj.insert(k, v);
                    }
                }
            }
        }
        val
    }
}

fn default_status() -> String {
    "active".to_string()
}

fn default_entity_type() -> String {
    "insight".to_string()
}

fn default_decay_score() -> f64 {
    1.0
}

fn default_layer() -> String {
    "working".to_string()
}

fn default_source() -> String {
    "agent".to_string()
}

/// A link between two entities. Stored as JSON array in entities.links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    pub target_id: String,
    #[serde(default)]
    pub relationship: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    0.5
}

/// A journal event — append-only log entry.
/// Structured as: what was evaluated → what was done → what's next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEvent {
    pub id: String,
    #[serde(default = "default_event_type")]
    pub event_type: String,
    #[serde(default)]
    pub evaluated_json: String,
    #[serde(default)]
    pub acted_json: String,
    #[serde(default)]
    pub forward_json: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub entity_id: String,
    pub created_at_unix_ms: i64,
}

fn default_event_type() -> String {
    "decision".to_string()
}

/// A key-value state entry with optional TTL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    pub key: String,
    #[serde(default)]
    pub value_json: String,
    pub expires_at_unix_ms: Option<i64>,
    pub created_at_unix_ms: i64,
}

/// Parameters for entity recall queries.
pub struct RecallParams {
    pub query: String,
    pub category: Option<String>,
    pub entity_type: Option<String>,
    pub limit: i64,
    pub min_decay: f64,
    pub topic_path: Option<String>,
    pub include_archived: bool,
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            category: None,
            entity_type: None,
            limit: 10,
            min_decay: 0.0,
            topic_path: None,
            include_archived: false,
        }
    }
}

/// Parameters for timeline queries over the journal.
pub struct TimelineParams {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub event_type: Option<String>,
    pub category: Option<String>,
    pub entity_id: Option<String>,
    pub limit: i64,
}

impl Default for TimelineParams {
    fn default() -> Self {
        Self {
            from_ms: None,
            to_ms: None,
            event_type: None,
            category: None,
            entity_id: None,
            limit: 50,
        }
    }
}

/// Migration report from v0.1.x → v0.2.0.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub total_old_memories: i64,
    pub entities_created: i64,
    pub entities_updated: i64,
    pub errors: Vec<String>,
    pub completed_at_unix_ms: i64,
}

/// Vault export/import report.
#[derive(Debug, Clone, Serialize)]
pub struct VaultReport {
    pub files_created: i64,
    pub files_updated: i64,
    pub errors: Vec<String>,
    pub vault_dir: String,
    pub completed_at_unix_ms: i64,
}

/// Decay tick report.
#[derive(Debug, Clone, Serialize)]
pub struct DecayReport {
    pub entities_checked: i64,
    pub entities_updated: i64,
    pub auto_archived: i64,
    pub completed_at_unix_ms: i64,
}

/// Compact report.
#[derive(Debug, Clone, Serialize)]
pub struct CompactReport {
    pub entities_archived: i64,
    pub entities_examined: i64,
    pub dry_run: bool,
    pub completed_at_unix_ms: i64,
}

/// Full database statistics.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total_entities: i64,
    pub by_category: serde_json::Value,
    pub by_type: serde_json::Value,
    pub by_layer: serde_json::Value,
    pub total_journal_events: i64,
    pub total_state_entries: i64,
    pub db_file_size_bytes: u64,
    pub oldest_unix_ms: Option<i64>,
    pub newest_unix_ms: Option<i64>,
}

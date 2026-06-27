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
    #[serde(default)]
    pub always_on: bool,
    /// Certainty for typed entities (0.0-1.0). Used by mimir_conflicts:
    /// low-certainty entities on the same topic are a conflict signal.
    #[serde(default = "default_certainty")]
    pub certainty: f64,
    /// Workspace scope identifier (v1.2.0). Empty = global/unscoped.
    /// Entities are invisible across workspaces when a scope is set.
    #[serde(default)]
    pub workspace_hash: String,
    /// Agent identity (v1.2.0). Tracks which agent wrote this entity.
    /// Used for agent attribution and context filtering.
    #[serde(default)]
    pub agent_id: String,
    /// Visibility: 'private', 'workspace', or 'public' (v1.2.0)
    #[serde(default = "default_visibility")]
    pub visibility: String,
    pub created_at_unix_ms: i64,
    pub last_accessed_unix_ms: i64,
    #[serde(skip)]
    #[allow(dead_code)]
    pub embedding: Option<Vec<f32>>,
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

fn default_certainty() -> f64 {
    0.5
}

/// Default recall trust weight. Non-zero so verified sources are preferred
/// over unverified AI drafts everywhere by default; kept low so it acts as a
/// tie-breaker rather than overriding relevance/recency.
pub fn default_trust_weight() -> f64 {
    0.15
}

fn default_visibility() -> String {
    "workspace".to_string()
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
    pub agent_id: String,
    /// Visibility: 'private', 'workspace', or 'public' (v1.2.0)
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
    pub offset: i64,
    pub min_decay: f64,
    pub topic_path: Option<String>,
    pub include_archived: bool,
    pub skip_side_effects: bool,
    pub mode: SearchMode,
    pub embedding: Option<Vec<f32>>,
    /// If set, truncate body_json at this many chars and append drill-down footer.
    /// BrainDB-inspired: prevents large bodies from silently flooding context.
    pub preview_cap: Option<i64>,
    /// If Some, only return entities where always_on matches (for context injection).
    pub always_on: Option<bool>,
    /// Additive boost weight for content witness signal (0.0 = disabled).
    /// Computes substring-match score against body_json, damped by body length.
    pub content_weight: f64,
    /// Additive boost weight for provenance/trust signal (0.0 = disabled).
    /// Verified sources are boosted fully; unverified entities are boosted in
    /// proportion to their certainty, so trusted sources outrank AI drafts on
    /// the same topic. Never penalizes.
    pub trust_weight: f64,
    /// Per-keyword halving quota for result diversity (1.0 = disabled).
    /// Each distinct matched keyword gets ceil(max_results × halving^n) slots.
    pub diversity_halving: f64,
    /// Per-query reservation share for multi-query diversity (0.0 = disabled).
    #[allow(dead_code)]
    pub diversity_per_query_share: f64,
    /// Recency half-life in seconds for time-aware hybrid ranking (#235).
    /// When `Some(hl)` with `hl > 0`, the RRF fusion score of each hybrid result
    /// is multiplied by a time-decay factor `0.5^(age / hl)`, where `age` is the
    /// time since the entity was created. A memory `hl` seconds old keeps half its
    /// fused weight, so recent context outranks an older but lexically-similar hit.
    /// `None` (default) preserves the existing relevance-only ranking exactly.
    /// Only applies to `SearchMode::Hybrid`; entities with an unset (<= 0)
    /// `created_at_unix_ms` are never penalized.
    pub recency_half_life_secs: Option<f64>,
    /// Workspace scope filter (v1.2.0). When Some, only entities with a
    /// matching workspace_hash are returned. None = no workspace filtering.
    pub workspace_hash: Option<String>,
    /// Agent identity filter (v1.2.0). When Some, only entities with a
    /// matching agent_id are returned. None = no agent filtering.
    pub agent_id: Option<String>,
    /// Visibility filter (v1.2.0). When Some, only entities with matching
    /// visibility are returned. None = no visibility filter.
    // Reserved: the recall query does not yet apply this filter and the MCP
    // RecallArgs has no visibility field, so it is always None in practice.
    // Kept so the filter can be wired without a signature change.
    #[allow(dead_code)]
    pub visibility: Option<String>,
}

/// Search mode for recall: FTS5 keyword, dense vector, or hybrid fusion.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub enum SearchMode {
    #[default]
    Fts5,
    Dense,
    Hybrid,
}

/// Configuration for FTS5 query expansion using stemming variants.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct QueryExpansionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_n_variants")]
    pub n_variants: usize,
}

fn default_n_variants() -> usize {
    1
}

/// Configuration for AES-256-GCM encryption at rest.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct EncryptionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_key_file")]
    pub key_file: String,
}

#[allow(dead_code)]
fn default_key_file() -> String {
    "~/.mimir/secret.key".to_string()
}

impl Default for RecallParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            category: None,
            entity_type: None,
            limit: 10,
            offset: 0,
            min_decay: 0.0,
            topic_path: None,
            include_archived: false,
            skip_side_effects: false,
            mode: SearchMode::Fts5,
            embedding: None,
            preview_cap: None,
            always_on: None,
            content_weight: 0.0,
            trust_weight: default_trust_weight(),
            diversity_halving: 1.0,
            diversity_per_query_share: 0.0,
            recency_half_life_secs: None,
            workspace_hash: None,
            agent_id: None,
            visibility: None,
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
    pub offset: i64,
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
            offset: 0,
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

/// Purge report — permanently deletes archived entities and runs VACUUM.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeReport {
    pub entities_deleted: i64,
    pub bytes_freed: i64,
    pub dry_run: bool,
    pub completed_at_unix_ms: i64,
}

/// Parameters for the coherence daemon pass.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CohereParams {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_max_links")]
    pub max_links: usize,
    #[serde(default)]
    pub promote_threshold: i64,
    #[serde(default = "default_archive_threshold")]
    pub archive_threshold: f64,
}

fn default_max_links() -> usize {
    20
}
fn default_archive_threshold() -> f64 {
    0.05
}
#[allow(dead_code)]
fn default_promote_threshold() -> i64 {
    3
}

/// Coherence daemon report — results of an auto-grooming pass.
#[derive(Debug, Clone, Serialize)]
pub struct CohereReport {
    pub promoted: i64,
    pub decayed: i64,
    pub linked: i64,
    pub archived: i64,
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

/// Graph node for entity link visualization.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub category: String,
}

/// Graph edge for entity link visualization.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub relationship: String,
}

/// Parameters for the mimir_ask RAG tool.
#[derive(Debug, Deserialize)]
pub struct AskParams {
    pub query: String,
    #[serde(default = "default_ask_limit")]
    pub top_k: usize,
}

fn default_ask_limit() -> usize {
    5
}

/// Result from mimir_ask: a grounded answer with cited sources.
#[derive(Debug, Serialize)]
pub struct AskResult {
    pub answer: String,
    pub sources: Vec<AskSource>,
}

/// A cited source entity in an ask result.
#[derive(Debug, Serialize)]
pub struct AskSource {
    pub key: String,
    pub category: String,
    pub score: f64,
    pub snippet: String,
}

/// Parameters for the mimir_ingest connector sync tool.
#[derive(Debug, Deserialize)]
pub struct IngestParams {
    /// Specific connector to run (None = all enabled connectors).
    pub connector: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// A raw document from an external connector before it becomes an entity.
#[derive(Debug, Clone)]
pub struct RawDocument {
    pub key: String,
    pub category: String,
    pub body_json: String,
    pub tags: Vec<String>,
}

/// Parameters for the mimir_embed tool — generate and store dense embeddings.
#[derive(Debug, Deserialize)]
pub struct EmbedParams {
    /// Text to embed and store on the entity (uses entity's body_json if omitted).
    pub text: Option<String>,
    /// Entity category (required).
    pub category: Option<String>,
    /// Entity key (required).
    pub key: Option<String>,
    /// Embed all entities matching this category that lack embeddings.
    #[serde(default)]
    pub batch_category: Option<String>,
    /// Max entities to embed in batch mode (default: 100).
    #[serde(default = "default_batch_limit")]
    pub batch_limit: usize,
}

fn default_batch_limit() -> usize {
    100
}

/// Parameters for the mimir_prune tool — bulk archive entities.
#[derive(Debug, Deserialize)]
pub struct PruneParams {
    /// Archive entities in this category.
    pub category: Option<String>,
    /// Archive entities with decay_score below this threshold.
    pub min_decay: Option<f64>,
    /// Archive entities older than this many days.
    pub older_than_days: Option<u32>,
    /// Max entities to prune (default: 100, use 0 for unlimited).
    #[serde(default = "default_prune_limit")]
    pub limit: usize,
    #[serde(default)]
    pub dry_run: bool,
    /// Explicitly archive everything in the category (no threshold required).
    #[serde(default)]
    pub purge_all: bool,
}

fn default_prune_limit() -> usize {
    100
}

/// Report from mimir_prune.
#[derive(Debug, Serialize)]
pub struct PruneReport {
    pub archived: usize,
    pub examined: usize,
    pub dry_run: bool,
    pub reason: String,
}

/// Parameters for the mimir_correct tool — structured correction capture.
/// Stores what went wrong, what the user said, and what to do instead.
#[derive(Debug, Deserialize)]
pub struct CorrectParams {
    pub wrong_approach: String,
    pub user_correction: String,
    pub task_context: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

/// Result from mimir_correct.
#[derive(Debug, Serialize)]
pub struct CorrectResult {
    pub entity_id: String,
    pub journal_id: String,
    pub category: String,
    pub key: String,
    pub created_at_unix_ms: i64,
}

/// Parameters for the mimir_synthesize tool — LLM-driven session synthesis.
/// Reviews session content and extracts structured lessons learned.
#[derive(Debug, Deserialize)]
pub struct SynthesizeParams {
    pub session_content: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub visibility: String,
}

/// A single synthesized lesson from session content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizedLesson {
    pub lesson_type: String,  // "success", "failure", "correction", "dead_end", "decision", "insight"
    pub summary: String,
    pub evidence: String,
    pub confidence: f64,
}

/// Result from mimir_synthesize.
#[derive(Debug, Serialize)]
pub struct SynthesizeResult {
    pub lessons: Vec<SynthesizedLesson>,
    pub entities_created: i64,
    pub journal_id: String,
    pub dry_run: bool,
    pub completed_at_unix_ms: i64,
}

/// Parameters for mimir_bench — performance metrics tracking.
#[derive(Debug, Deserialize)]
pub struct BenchParams {
    pub task_description: String,
    pub turns_taken: i64,
    pub tokens_used: i64,
    pub memory_recall_used: bool,
    pub recall_count: i64,
    #[serde(default)]
    pub task_success: bool,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Result from mimir_bench.
#[derive(Debug, Serialize)]
pub struct BenchResult {
    pub entity_id: String,
    pub created_at_unix_ms: i64,
}


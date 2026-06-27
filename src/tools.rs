use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::{now_ms, Database};
use crate::models::{
    AskParams, EmbedParams, Entity, IngestParams, JournalEvent, PruneParams, RecallParams,
    SearchMode, StateEntry, TimelineParams,
};

// ─── Deserialization structs ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RememberArgs {
    pub category: String,
    pub key: String,
    pub body_json: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default = "default_entity_type")]
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_importance")]
    pub importance: f64,
    #[serde(default)]
    pub topic_path: String,
    #[serde(default)]
    pub recall_when: Vec<String>,
    #[serde(default)]
    pub always_on: bool,
    #[serde(default = "default_certainty")]
    pub certainty: f64,
    #[serde(default)]
    pub workspace_hash: String,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default = "default_visibility")]
    pub visibility: String,
}

fn default_certainty() -> f64 {
    0.5
}

fn default_visibility() -> String {
    "workspace".to_string()
}

fn default_status() -> String {
    "active".to_string()
}

fn default_entity_type() -> String {
    "insight".to_string()
}

fn default_importance() -> f64 {
    0.5
}

#[derive(Debug, Deserialize)]
pub struct RecallArgs {
    pub query: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub min_decay: f64,
    #[serde(default)]
    pub topic_path: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub expansion: crate::models::QueryExpansionConfig,
    #[serde(default)]
    pub mode: String, // "fts5", "dense", or "hybrid"
    #[serde(default)]
    pub preview_cap: Option<i64>,
    #[serde(default)]
    pub always_on: Option<bool>,
    #[serde(default)]
    pub content_weight: f64,
    #[serde(default = "crate::models::default_trust_weight")]
    pub trust_weight: f64,
    #[serde(default = "default_halving")]
    pub diversity_halving: f64,
    /// Recency half-life in seconds for time-aware hybrid ranking (#235).
    /// Omit (default) for relevance-only ranking; set to bias toward recent memories.
    #[serde(default)]
    pub recency_half_life_secs: Option<f64>,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

fn default_halving() -> f64 {
    1.0
}

fn default_limit() -> i64 {
    10
}

#[derive(Debug, Deserialize)]
pub struct ForgetArgs {
    pub category: String,
    pub key: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct LinkArgs {
    pub from_category: String,
    pub from_key: String,
    pub to_id: String,
    #[serde(default)]
    pub relationship: String,
}

#[derive(Debug, Deserialize)]
pub struct UnlinkArgs {
    pub from_category: String,
    pub from_key: String,
    pub to_id: String,
}

#[derive(Debug, Deserialize)]
pub struct JournalArgs {
    #[serde(default = "default_event_type")]
    pub event_type: String,
    #[serde(default)]
    pub evaluated: Value,
    #[serde(default)]
    pub acted: Value,
    #[serde(default)]
    pub forward: Value,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub agent_id: String,
}

fn default_event_type() -> String {
    "decision".to_string()
}

#[derive(Debug, Deserialize)]
pub struct TimelineArgs {
    #[serde(default)]
    pub from_ms: Option<i64>,
    #[serde(default)]
    pub to_ms: Option<i64>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub entity_id: Option<String>,
    #[serde(default = "default_timeline_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_timeline_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct StateSetArgs {
    pub key: String,
    pub value_json: String,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct StateGetArgs {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct StateDeleteArgs {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct StateListArgs {
    #[serde(default)]
    pub prefix: String,
}

#[derive(Debug, Deserialize)]
pub struct CompactArgs {
    #[serde(default = "default_min_decay")]
    pub min_decay: f64,
    #[serde(default)]
    pub dry_run: bool,
}

fn default_min_decay() -> f64 {
    0.1
}

#[derive(Debug, Deserialize)]
pub struct MigrateArgs {
    pub from_path: String,
}

#[derive(Debug, Deserialize)]
pub struct ContextArgs {
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default = "default_context_limit")]
    pub limit: i64,
}

fn default_context_limit() -> i64 {
    10
}

#[derive(Debug, Deserialize)]
pub struct ExtractArgs {
    /// Raw text to extract from. If empty, `category` + `key` of a stored entity
    /// are used instead.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Extractor strategy: "rule_based" (default, local heuristics) or "none" (no-op).
    #[serde(default = "default_extract_strategy")]
    pub strategy: String,
}

fn default_extract_strategy() -> String {
    "rule_based".to_string()
}

// ─── Tool handlers ──────────────────────────────────────────────

pub fn handle_remember(db: &Database, args: Value) -> Result<String, String> {
    let a: RememberArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid remember arguments: {}", e))?;

    // Validate body_json is valid JSON
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&a.body_json) {
        return Err(format!("body_json is not valid JSON: {}", e));
    }

    // Merge recall_when into body_json if provided
    let body = if a.recall_when.is_empty() {
        a.body_json
    } else {
        let mut obj: serde_json::Value =
            serde_json::from_str(&a.body_json).unwrap_or(serde_json::json!({}));
        if let Some(map) = obj.as_object_mut() {
            let triggers: Vec<serde_json::Value> = a
                .recall_when
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect();
            map.insert(
                "recall_when".to_string(),
                serde_json::Value::Array(triggers),
            );
        }
        serde_json::to_string(&obj).unwrap_or(a.body_json)
    };

    let raw_id = Uuid::new_v4().to_string().replace('-', "");
    let id = format!("mem-{}", &raw_id[..12.min(raw_id.len())]);
    let now = now_ms();

    let entity = Entity {
        id,
        category: a.category,
        key: a.key,
        body_json: body,
        status: a.status,
        entity_type: a.entity_type,
        tags: a.tags,
        decay_score: a.importance,
        retrieval_count: 0,
        layer: "buffer".to_string(),
        topic_path: a.topic_path,
        archived: false,
        archive_reason: String::new(),
        links: vec![],
        verified: false,
        source: "agent".to_string(),
        always_on: a.always_on,
        certainty: a.certainty,
        workspace_hash: a.workspace_hash.clone(),
        agent_id: a.agent_id.clone(),
        visibility: a.visibility.clone(),
        created_at_unix_ms: now,
        last_accessed_unix_ms: now,
        embedding: None,
    };

    let (eid, action) = db
        .remember(&entity)
        .map_err(|e| format!("Remember failed: {}", e))?;

    let result = json!({
        "id": eid,
        "action": action,
        "category": entity.category,
        "key": entity.key,
    });
    Ok(result.to_string())
}

pub fn handle_recall(db: &Database, args: Value) -> Result<String, String> {
    let a: RecallArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid recall arguments: {}", e))?;

    let mode = match a.mode.as_str() {
        "dense" => SearchMode::Dense,
        "hybrid" => SearchMode::Hybrid,
        _ => SearchMode::Fts5,
    };

    // If query expansion is enabled, generate stemming variants and merge results
    if a.expansion.enabled && !a.query.is_empty() && mode == SearchMode::Fts5 {
        return handle_recall_with_expansion(db, &a);
    }

    let params = RecallParams {
        query: a.query,
        category: a.category,
        entity_type: a.entity_type,
        limit: a.limit,
        offset: a.offset,
        min_decay: a.min_decay,
        topic_path: a.topic_path,
        include_archived: a.include_archived,
        skip_side_effects: false,
        mode,
        embedding: None,
        preview_cap: a.preview_cap,
        always_on: a.always_on,
        content_weight: a.content_weight,
        trust_weight: a.trust_weight,
        diversity_halving: a.diversity_halving,
        diversity_per_query_share: 0.0,
        recency_half_life_secs: a.recency_half_life_secs,
        workspace_hash: a.workspace_hash.clone(),
        agent_id: a.agent_id.clone(),
        visibility: None,
    };

    let entities = db
        .recall(&params)
        .map_err(|e| format!("Recall failed: {}", e))?;

    let items_expanded: Vec<serde_json::Value> =
        entities.iter().map(|e| e.to_json_expanded()).collect();

    let result = json!({
        "items": items_expanded,
        "total": items_expanded.len(),
    });
    Ok(result.to_string())
}

/// Run recall with stemming-based query expansion, merging results from
/// the original query and up to `n_variants` stemmed alternatives.
fn handle_recall_with_expansion(db: &Database, a: &RecallArgs) -> Result<String, String> {
    use rust_stemmers::{Algorithm, Stemmer};
    use std::collections::HashMap;

    let stemmer = Stemmer::create(Algorithm::English);
    let tokens: Vec<&str> = a
        .query
        .split_whitespace()
        .filter(|w| !w.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err("Query expansion requires at least one token".to_string());
    }

    // Build variants: original query + stemmed alternatives
    let mut variants: Vec<String> = vec![a.query.clone()];
    for (i, &token) in tokens.iter().enumerate() {
        if variants.len() > a.expansion.n_variants {
            break;
        }
        let stemmed = stemmer.stem(token).to_string();
        if stemmed != token {
            let mut alt_tokens: Vec<&str> = tokens.clone();
            alt_tokens[i] = &stemmed;
            variants.push(alt_tokens.join(" "));
        }
    }

    // Collect results from all variants, keeping the highest-score version of each entity
    let mut best: HashMap<String, (crate::models::Entity, f64)> = HashMap::new();

    for variant in &variants {
        let params = RecallParams {
            query: variant.clone(),
            category: a.category.clone(),
            entity_type: a.entity_type.clone(),
            limit: a.limit.max(50), // fetch more per variant to have good merge pool
            offset: 0,
            min_decay: a.min_decay,
            topic_path: a.topic_path.clone(),
            include_archived: a.include_archived,
            // #207: suppress per-variant side-effects; a single recall must bump
            // each returned entity once, not once per matching variant. We apply
            // the batched side-effect below on the final merged result set.
            skip_side_effects: true,
            mode: SearchMode::Fts5,
            embedding: None,
            preview_cap: a.preview_cap,
            always_on: a.always_on,
            content_weight: a.content_weight,
            trust_weight: a.trust_weight,
            diversity_halving: a.diversity_halving,
            diversity_per_query_share: 0.0,
            // Query expansion runs in Fts5 mode only, so recency (a hybrid-fusion
            // re-weighting) never applies on this path.
            recency_half_life_secs: None,
            workspace_hash: a.workspace_hash.clone(),
            agent_id: a.agent_id.clone(),
            visibility: None,
        };

        if let Ok(entities) = db.recall(&params) {
            for entity in entities {
                let score = entity.decay_score;
                best.entry(entity.id.clone())
                    .and_modify(|(existing, existing_score)| {
                        if score > *existing_score {
                            *existing = entity.clone();
                            *existing_score = score;
                        }
                    })
                    .or_insert((entity, score));
            }
        }
    }

    // Sort by score descending, then truncate to limit
    let mut merged: Vec<_> = best.into_values().collect();
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(a.limit as usize);

    // #207: apply recall side-effects once, to the entities actually returned,
    // in one batched write — rather than once per variant inside the loop above.
    let hit_ids: Vec<String> = merged.iter().map(|(e, _)| e.id.clone()).collect();
    let _ = db.apply_recall_side_effects(&hit_ids);

    let items_expanded: Vec<serde_json::Value> = merged
        .iter()
        .map(|(entity, _)| entity.to_json_expanded())
        .collect();

    let result = json!({
        "items": items_expanded,
        "total": items_expanded.len(),
        "variants": variants.len(),
    });
    Ok(result.to_string())
}

/// #103: Get a single entity by ID with full body (for drill-down after preview cap).
pub fn handle_get_entity(db: &Database, args: Value) -> Result<String, String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'id' parameter".to_string())?;

    let entity = db
        .get_entity_by_id_public(id)
        .map_err(|e| format!("Get entity failed: {}", e))?
        .ok_or_else(|| format!("Entity not found: {}", id))?;

    let result = json!({
        "id": entity.id,
        "category": entity.category,
        "key": entity.key,
        "body_json": entity.body_json,
        "status": entity.status,
        "entity_type": entity.entity_type,
        "tags": entity.tags,
        "decay_score": entity.decay_score,
        "retrieval_count": entity.retrieval_count,
        "layer": entity.layer,
        "always_on": entity.always_on,
        "certainty": entity.certainty,
        "created_at_unix_ms": entity.created_at_unix_ms,
        "last_accessed_unix_ms": entity.last_accessed_unix_ms,
    });
    Ok(result.to_string())
}

pub fn handle_as_of(db: &Database, args: Value) -> Result<String, String> {
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'category' parameter".to_string())?;
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'key' parameter".to_string())?;
    let as_of = args
        .get("as_of_unix_ms")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "Missing 'as_of_unix_ms' parameter (integer unix ms)".to_string())?;

    let found = db
        .as_of(category, key, as_of)
        .map_err(|e| format!("as_of failed: {}", e))?;

    let result = match found {
        Some(e) => json!({
            "found": true,
            "id": e.id,
            "category": e.category,
            "key": e.key,
            "body_json": e.body_json,
            "status": e.status,
            "entity_type": e.entity_type,
            "as_of_unix_ms": as_of,
        }),
        None => json!({
            "found": false,
            "category": category,
            "key": key,
            "as_of_unix_ms": as_of,
        }),
    };
    Ok(result.to_string())
}

pub fn handle_forget(db: &Database, args: Value) -> Result<String, String> {
    let a: ForgetArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid forget arguments: {}", e))?;

    let reason = if a.reason.is_empty() {
        "manual".to_string()
    } else {
        a.reason
    };

    let found = db
        .forget(&a.category, &a.key, &reason)
        .map_err(|e| format!("Forget failed: {}", e))?;

    let result = json!({
        "found": found,
        "category": a.category,
        "key": a.key,
    });
    Ok(result.to_string())
}

pub fn handle_link(db: &Database, args: Value) -> Result<String, String> {
    let a: LinkArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid link arguments: {}", e))?;

    let rel = if a.relationship.is_empty() {
        "related".to_string()
    } else {
        a.relationship
    };

    db.link(&a.from_category, &a.from_key, &a.to_id, &rel)
        .map_err(|e| format!("Link failed: {}", e))?;

    let result = json!({
        "success": true,
        "from": format!("{}/{}", a.from_category, a.from_key),
        "to": a.to_id,
        "relationship": rel,
    });
    Ok(result.to_string())
}

pub fn handle_unlink(db: &Database, args: Value) -> Result<String, String> {
    let a: UnlinkArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid unlink arguments: {}", e))?;

    db.unlink(&a.from_category, &a.from_key, &a.to_id)
        .map_err(|e| format!("Unlink failed: {}", e))?;

    let result = json!({
        "success": true,
        "from": format!("{}/{}", a.from_category, a.from_key),
        "to": a.to_id,
    });
    Ok(result.to_string())
}

pub fn handle_journal(db: &Database, args: Value) -> Result<String, String> {
    let a: JournalArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid journal arguments: {}", e))?;

    // Enforce size limits on journal fields
    const MAX_FIELD_BYTES: usize = 64 * 1024; // 64KB per field
    if a.evaluated.to_string().len() > MAX_FIELD_BYTES
        || a.acted.to_string().len() > MAX_FIELD_BYTES
        || a.forward.to_string().len() > MAX_FIELD_BYTES
    {
        return Err(format!(
            "Journal field exceeds {}KB limit",
            MAX_FIELD_BYTES / 1024
        ));
    }

    let raw_id = Uuid::new_v4().to_string().replace('-', "");
    let id = format!("jrn-{}", &raw_id[..12.min(raw_id.len())]);

    let event = JournalEvent {
        id,
        event_type: a.event_type,
        evaluated_json: a.evaluated.to_string(),
        acted_json: a.acted.to_string(),
        forward_json: a.forward.to_string(),
        category: a.category,
        key: a.key,
        entity_id: a.entity_id,
        agent_id: a.agent_id,
        created_at_unix_ms: now_ms(),
    };

    db.journal(&event)
        .map_err(|e| format!("Journal failed: {}", e))?;

    let result = json!({
        "id": event.id,
        "event_type": event.event_type,
        "created_at_unix_ms": event.created_at_unix_ms,
    });
    Ok(result.to_string())
}

pub fn handle_timeline(db: &Database, args: Value) -> Result<String, String> {
    let a: TimelineArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid timeline arguments: {}", e))?;

    let params = TimelineParams {
        from_ms: a.from_ms,
        to_ms: a.to_ms,
        event_type: a.event_type,
        category: a.category,
        entity_id: a.entity_id,
        limit: a.limit,
        offset: a.offset,
    };

    let events = db
        .timeline(&params)
        .map_err(|e| format!("Timeline failed: {}", e))?;

    let result = json!({
        "items": events,
        "total": events.len(),
    });
    Ok(result.to_string())
}

pub fn handle_state_set(db: &Database, args: Value) -> Result<String, String> {
    let a: StateSetArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid state_set arguments: {}", e))?;

    let now = now_ms();
    let expires_at = a.ttl_seconds.map(|ttl| now + (ttl * 1000));

    let entry = StateEntry {
        key: a.key.clone(),
        value_json: a.value_json,
        expires_at_unix_ms: expires_at,
        created_at_unix_ms: now,
    };

    db.state_set(&entry)
        .map_err(|e| format!("State set failed: {}", e))?;

    let result = json!({
        "key": a.key,
        "ttl_seconds": a.ttl_seconds,
        "expires_at_unix_ms": expires_at,
    });
    Ok(result.to_string())
}

pub fn handle_state_get(db: &Database, args: Value) -> Result<String, String> {
    let a: StateGetArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid state_get arguments: {}", e))?;

    let entry = db
        .state_get(&a.key)
        .map_err(|e| format!("State get failed: {}", e))?;

    match entry {
        Some(e) => {
            let result = json!({
                "found": true,
                "key": e.key,
                "value": e.value_json,
                "expires_at_unix_ms": e.expires_at_unix_ms,
                "created_at_unix_ms": e.created_at_unix_ms,
            });
            Ok(result.to_string())
        }
        None => {
            let result = json!({
                "found": false,
                "key": a.key,
            });
            Ok(result.to_string())
        }
    }
}

pub fn handle_state_delete(db: &Database, args: Value) -> Result<String, String> {
    let a: StateDeleteArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid state_delete arguments: {}", e))?;

    let found = db
        .state_delete(&a.key)
        .map_err(|e| format!("State delete failed: {}", e))?;

    let result = json!({
        "found": found,
        "key": a.key,
    });
    Ok(result.to_string())
}

pub fn handle_state_list(db: &Database, args: Value) -> Result<String, String> {
    let a: StateListArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid state_list arguments: {}", e))?;

    let keys = db
        .state_list(&a.prefix)
        .map_err(|e| format!("State list failed: {}", e))?;

    let result = json!({
        "keys": keys,
        "total": keys.len(),
    });
    Ok(result.to_string())
}

pub fn handle_health(db: &Database) -> String {
    if db.health_check() {
        json!({ "status": "healthy" }).to_string()
    } else {
        json!({ "status": "unhealthy" }).to_string()
    }
}

pub fn handle_stats(db: &Database) -> String {
    match db.stats() {
        Ok(stats) => serde_json::to_string(&stats).unwrap_or_else(|e| {
            json!({ "error": format!("Stats serialization failed: {}", e) }).to_string()
        }),
        Err(e) => json!({"error": format!("Stats failed: {}", e)}).to_string(),
    }
}

pub fn handle_compact(db: &Database, args: Value) -> String {
    let a: CompactArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return json!({"error": format!("Invalid compact arguments: {}", e)}).to_string(),
    };

    match db.compact(a.min_decay, a.dry_run) {
        Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
            json!({"error": format!("Compact report serialization failed: {}", e)}).to_string()
        }),
        Err(e) => json!({"error": format!("Compact failed: {}", e)}).to_string(),
    }
}

pub fn handle_migrate(db: &Database, args: Value) -> String {
    let a: MigrateArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return json!({"error": format!("Invalid migrate arguments: {}", e)}).to_string(),
    };

    match db.migrate_from_v0_1(&a.from_path) {
        Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
            json!({"error": format!("Migration report serialization failed: {}", e)}).to_string()
        }),
        Err(e) => json!({"error": format!("Migration failed: {}", e)}).to_string(),
    }
}

pub fn handle_context(db: &Database, args: Value) -> String {
    let a: ContextArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return json!({"error": format!("Invalid context arguments: {}", e)}).to_string(),
    };

    match db.context(&a.categories, a.limit) {
        Ok(markdown) => {
            let total_chars = markdown.len();
            json!({"markdown": markdown, "total_chars": total_chars}).to_string()
        }
        Err(e) => json!({"error": format!("Context generation failed: {}", e)}).to_string(),
    }
}

/// Extract structured knowledge (facts/preferences/temporal events/episodes) from
/// raw text or a stored entity, using a local, deterministic extractor (#234).
/// Read-only: this never writes to the store, so the zero-dependency / air-gapped
/// path is preserved and extraction stays strictly opt-in.
pub fn handle_extract(db: &Database, args: Value) -> Result<String, String> {
    let a: ExtractArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid extract arguments: {}", e))?;

    // Resolve the source text: explicit `text`, else a stored entity's body.
    let text = if !a.text.trim().is_empty() {
        a.text.clone()
    } else if let (Some(cat), Some(key)) = (a.category.as_ref(), a.key.as_ref()) {
        match db
            .get_entity(cat, key)
            .map_err(|e| format!("get_entity failed: {}", e))?
        {
            Some(ent) => ent.body_json,
            None => return Err(format!("Entity not found: {}/{}", cat, key)),
        }
    } else {
        return Err(
            "extract requires `text`, or `category` + `key` of a stored entity".to_string(),
        );
    };

    let extractor = crate::extraction::extractor_for(&a.strategy);
    let items = extractor.extract(&text);
    let items_json = serde_json::to_value(&items).unwrap_or_else(|_| json!([]));
    Ok(json!({
        "items": items_json,
        "total": items.len(),
        "strategy": a.strategy,
    })
    .to_string())
}

#[derive(Debug, Deserialize)]
pub struct VaultExportArgs {
    pub vault_dir: String,
    #[serde(default)]
    pub workspace_hash: Option<String>,
}

pub fn handle_vault_export(db: &Database, args: Value) -> String {
    let a: VaultExportArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return json!({"error": format!("Invalid vault_export arguments: {}", e)}).to_string()
        }
    };
    let dir = if a.vault_dir.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/root".to_string());
        a.vault_dir.replacen("~", &home, 1)
    } else {
        a.vault_dir.clone()
    };
    match db.vault_export(&dir, a.workspace_hash.as_deref()) {
        Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
            json!({"error": format!("Serialization failed: {}", e)}).to_string()
        }),
        Err(e) => json!({"error": format!("Vault export failed: {}", e)}).to_string(),
    }
}

pub fn handle_vault_import(db: &Database, args: Value) -> String {
    let a: VaultExportArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return json!({"error": format!("Invalid vault_import arguments: {}", e)}).to_string()
        }
    };
    let dir = if a.vault_dir.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/root".to_string());
        a.vault_dir.replacen("~", &home, 1)
    } else {
        a.vault_dir.clone()
    };
    match db.vault_import(&dir) {
        Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
            json!({"error": format!("Serialization failed: {}", e)}).to_string()
        }),
        Err(e) => json!({"error": format!("Vault import failed: {}", e)}).to_string(),
    }
}

#[derive(Debug, Deserialize)]
pub struct TraverseArgs {
    pub category: String,
    pub key: String,
    #[serde(default = "default_depth")]
    pub max_depth: i64,
    #[serde(default = "default_max_nodes")]
    pub max_nodes: i64,
}

fn default_depth() -> i64 {
    3
}

fn default_max_nodes() -> i64 {
    100
}

pub fn handle_traverse(db: &Database, args: Value) -> String {
    let a: TraverseArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return json!({"error": format!("Invalid traverse arguments: {}", e)}).to_string()
        }
    };
    match db.traverse_chain(&a.category, &a.key, a.max_depth, a.max_nodes) {
        Ok(chain) => serde_json::to_string(&chain)
            .unwrap_or_else(|e| json!({"error": format!("{}", e)}).to_string()),
        Err(e) => json!({"error": format!("Traverse failed: {}", e)}).to_string(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScoreArgs {
    pub category: String,
    pub key: String,
    pub score: f64,
}

pub fn handle_score(db: &Database, args: Value) -> String {
    let a: ScoreArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return json!({"error": format!("Invalid score arguments: {}", e)}).to_string(),
    };
    match db.score_entity(&a.category, &a.key, a.score) {
        Ok(found) => {
            json!({"found": found, "category": a.category, "key": a.key, "score": a.score})
                .to_string()
        }
        Err(e) => json!({"error": format!("Score failed: {}", e)}).to_string(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ConflictArgs {
    pub category: String,
    #[serde(default = "default_conflict_threshold")]
    pub threshold: f64,
    #[serde(default = "default_conflict_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_conflict_threshold() -> f64 {
    0.4
}
fn default_conflict_limit() -> i64 {
    10
}

pub fn handle_conflicts(db: &Database, args: Value) -> String {
    let a: ConflictArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return json!({"error": format!("Invalid conflicts arguments: {}", e)}).to_string()
        }
    };
    match db.detect_conflicts(&a.category, a.threshold, a.limit, a.offset) {
        Ok(report) => serde_json::to_string(&report)
            .unwrap_or_else(|e| json!({"error": format!("{}", e)}).to_string()),
        Err(e) => json!({"error": format!("Conflict detection failed: {}", e)}).to_string(),
    }
}

pub fn handle_decay(db: &Database, _args: Value) -> String {
    match db.decay_tick() {
        Ok(report) => serde_json::to_string(&report).unwrap_or_else(|e| {
            json!({"error": format!("Decay report serialization failed: {}", e)}).to_string()
        }),
        Err(e) => json!({"error": format!("Decay tick failed: {}", e)}).to_string(),
    }
}

pub fn handle_reindex(db: &Database, _args: Value) -> String {
    match db.reindex_fts() {
        Ok(n) => json!({"reindexed": n}).to_string(),
        Err(e) => json!({"error": format!("Reindex failed: {}", e)}).to_string(),
    }
}

pub fn handle_ask(db: &Database, args: Value) -> Result<String, String> {
    let params: AskParams =
        serde_json::from_value(args).map_err(|e| format!("Invalid ask arguments: {}", e))?;

    if !db.llm_enabled() {
        return Err("LLM is not enabled. Set --llm-endpoint to enable mimir_ask.".to_string());
    }

    match db.ask(&params) {
        Ok(result) => {
            serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
        }
        Err(e) => Err(format!("Ask failed: {}", e)),
    }
}

pub fn handle_ingest(db: &Database, args: Value) -> Result<String, String> {
    let params: IngestParams =
        serde_json::from_value(args).map_err(|e| format!("Invalid ingest arguments: {}", e))?;

    match db.ingest(&params) {
        Ok(result) => Ok(result.to_string()),
        Err(e) => Err(format!("Ingest failed: {}", e)),
    }
}

#[derive(Debug, Deserialize)]
pub struct IngestFileArgs {
    /// Path to the document file to ingest.
    pub path: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Ingest a document file into memory by extracting its text **locally** (#236).
/// Plaintext/markdown work in any build; DOCX/PDF need `--features multimodal`.
/// The extracted text is stored as a normal entity (category default "document",
/// key default = file name) so it is recallable like any other memory.
pub fn handle_ingest_file(db: &Database, args: Value) -> Result<String, String> {
    let a: IngestFileArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid ingest_file arguments: {}", e))?;
    let path = std::path::Path::new(&a.path);

    let text = crate::multimodal::extract_text(path)?;
    let char_count = text.chars().count();

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    let category = a.category.unwrap_or_else(|| "document".to_string());
    let key = a.key.unwrap_or(file_name);

    let body = json!({ "content": text, "source_path": a.path }).to_string();
    let now = now_ms();
    let raw_id = Uuid::new_v4().to_string().replace('-', "");
    let id = format!("mem-{}", &raw_id[..12.min(raw_id.len())]);
    let entity = Entity {
        id,
        category,
        key,
        body_json: body,
        status: "active".to_string(),
        entity_type: "document".to_string(),
        tags: a.tags,
        decay_score: 1.0,
        retrieval_count: 0,
        layer: "buffer".to_string(),
        topic_path: String::new(),
        archived: false,
        archive_reason: String::new(),
        links: vec![],
        verified: false,
        source: "ingest_file".to_string(),
        always_on: false,
        certainty: 0.5,
        workspace_hash: String::new(),
        agent_id: String::new(),
        visibility: "workspace".to_string(),
        created_at_unix_ms: now,
        last_accessed_unix_ms: now,
        embedding: None,
    };
    let (eid, action) = db
        .remember(&entity)
        .map_err(|e| format!("Remember failed: {}", e))?;
    Ok(json!({
        "id": eid,
        "action": action,
        "category": entity.category,
        "key": entity.key,
        "chars": char_count,
    })
    .to_string())
}

pub fn handle_embed(db: &Database, args: Value) -> Result<String, String> {
    let params: EmbedParams =
        serde_json::from_value(args).map_err(|e| format!("Invalid embed arguments: {}", e))?;
    match db.embed_entity(&params) {
        Ok(result) => Ok(result.to_string()),
        Err(e) => Err(format!("Embed failed: {}", e)),
    }
}

pub fn handle_prune(db: &Database, args: Value) -> Result<String, String> {
    let params: PruneParams =
        serde_json::from_value(args).map_err(|e| format!("Invalid prune arguments: {}", e))?;

    // #202: require a threshold or explicit purge_all — category alone is a footgun
    if !params.purge_all && params.min_decay.is_none() && params.older_than_days.is_none() {
        return Err(
            "prune requires min_decay, older_than_days, or purge_all=true to archive the whole category"
                .to_string(),
        );
    }

    match db.prune(&params) {
        Ok(report) => {
            serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
        }
        Err(e) => Err(format!("Prune failed: {}", e)),
    }
}

pub fn handle_federate(db: &Database, args: Value) -> Result<String, String> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct FederateArgs {
        from_workspace: String,
        to_workspace: String,
        #[serde(default)]
        vault_dir: String,
    }
    let a: FederateArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid federate arguments: {}", e))?;

    let vault_dir = if a.vault_dir.is_empty() {
        "/tmp/mimir-federate".to_string()
    } else {
        a.vault_dir
    };

    // Export from source workspace
    let export_report = db.vault_export(&vault_dir, Some(&a.from_workspace))
        .map_err(|e| format!("Federate export failed: {}", e))?;

    // Remap entities: overwrite workspace_hash to target
    let mut remapped = 0i64;
    for entry in std::fs::read_dir(&vault_dir).map_err(|e| format!("Read vault dir: {}", e))? {
        let entry = entry.map_err(|e| format!("Read entry: {}", e))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Read {}: {}", path.display(), e))?;
        let remapped_content =
            content.replace(&format!("workspace_hash: {}", a.from_workspace),
                            &format!("workspace_hash: {}", a.to_workspace));
        if remapped_content != content {
            std::fs::write(&path, remapped_content)
                .map_err(|e| format!("Write {}: {}", path.display(), e))?;
            remapped += 1;
        }
    }

    // Import into target workspace
    let import_report = db.vault_import(&vault_dir)
        .map_err(|e| format!("Federate import failed: {}", e))?;

    let result = json!({
        "exported": export_report.files_created + export_report.files_updated,
        "remapped": remapped,
        "imported": import_report.files_created + import_report.files_updated,
        "import_errors": import_report.errors,
    });
    Ok(result.to_string())
}

pub fn handle_share(db: &Database, args: Value) -> Result<String, String> {
    #[derive(Deserialize)]
    struct ShareArgs {
        category: String,
        key: String,
        to_workspace: String,
    }
    let a: ShareArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid share arguments: {}", e))?;

    // Find the entity
    // Recall by category first, then filter by key (FTS5 searches body_json,
    // not the key column, so we can't use key as a query term reliably).
    let entities = db.recall(&crate::models::RecallParams {
        query: String::new(),
        category: Some(a.category.clone()),
        entity_type: None,
        limit: 100,
        offset: 0,
        min_decay: 0.0,
        topic_path: None,
        include_archived: false,
        skip_side_effects: true,
        ..crate::models::RecallParams::default()
    }).map_err(|e| format!("Recall failed: {}", e))?;

    let src = entities.iter()
        .find(|e| e.key == a.key)
        .ok_or_else(|| format!("Entity not found: {}/{}", a.category, a.key))?;

    // Clone entity into target workspace
    let mut clone = src.clone();
    clone.workspace_hash = a.to_workspace.clone();
    // Force a new id so it doesn't collide
    let raw_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    clone.id = format!("mem-{}", &raw_id[..12.min(raw_id.len())]);
    clone.retrieval_count = 0;
    clone.layer = "buffer".to_string();

    let (eid, action) = db.remember(&clone)
        .map_err(|e| format!("Share failed: {}", e))?;

    Ok(json!({"shared_id": eid, "action": action, "from_workspace": src.workspace_hash, "to_workspace": a.to_workspace}).to_string())
}

pub fn handle_workspace_list(db: &Database) -> String {
    match db.workspace_list_categories() {
        Ok(cats) => json!({"categories": cats, "total": cats.len()}).to_string(),
        Err(e) => json!({"error": format!("Workspace list failed: {}", e)}).to_string(),
    }
}

// ─── New: autocohere, recall_when + cohere handlers ─────────────────────────

#[derive(Debug, Deserialize)]
pub struct AutocohereArgs {
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct RecallWhenArgs {
    pub context: String,
    #[serde(default = "default_rw_limit")]
    pub limit: i64,
}

fn default_rw_limit() -> i64 {
    10
}

#[derive(Debug, Deserialize)]
pub struct CohereArgs {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_max_links_cohere")]
    pub max_links: usize,
}

fn default_max_links_cohere() -> usize {
    20
}

pub fn handle_recall_when(db: &Database, args: Value) -> Result<String, String> {
    let a: RecallWhenArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid recall_when arguments: {}", e))?;

    let entities = db
        .recall_when(&a.context, a.limit)
        .map_err(|e| format!("Recall_when failed: {}", e))?;

    let items_expanded: Vec<serde_json::Value> =
        entities.iter().map(|e| e.to_json_expanded()).collect();

    let result = json!({
        "items": items_expanded,
        "total": items_expanded.len(),
        "context": a.context,
    });
    Ok(result.to_string())
}

pub fn handle_autocohere(db: &Database, args: Value) -> Result<String, String> {
    let a: AutocohereArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid autocohere arguments: {}", e))?;

    let mut total_promoted = 0i64;
    let mut total_links = 0i64;
    let mut total_archived_cohere = 0i64;

    // 1. Run mimir_cohere (promote, link, archive)
    let cohere_params = crate::models::CohereParams {
        dry_run: a.dry_run,
        ..Default::default()
    };
    let cohere_report = db
        .cohere(&cohere_params)
        .map_err(|e| format!("Autocohere step (cohere) failed: {}", e))?;

    total_promoted += cohere_report.promoted;
    total_links += cohere_report.linked;
    total_archived_cohere += cohere_report.archived;

    // 2. Then mimir_decay (recalculate Ebbinghaus decay)
    let decay_report = db
        .decay_tick()
        .map_err(|e| format!("Autocohere step (decay) failed: {}", e))?;

    // 3. Then mimir_compact (archive below threshold)
    let compact_report = db
        .compact(0.1, a.dry_run)
        .map_err(|e| format!("Autocohere step (compact) failed: {}", e))?;

    let initial_db_size = db
        .file_size_bytes()
        .map_err(|e| format!("Failed to get initial DB size: {}", e))?;
    let final_db_size = if a.dry_run {
        initial_db_size
    } else {
        db.file_size_bytes()
            .map_err(|e| format!("Failed to get final DB size: {}", e))?
    };

    let result = json!({
        "promoted_entities": total_promoted,
        "links_created": total_links,
        "archived_entities": total_archived_cohere + compact_report.entities_archived,
        "decay_updates": decay_report.entities_updated,
        "compact_archived_count": compact_report.entities_archived,
        "db_size_delta_bytes": final_db_size as i64 - initial_db_size as i64,
        "dry_run": a.dry_run,
    });
    Ok(result.to_string())
}

pub fn handle_cohere(db: &Database, args: Value) -> Result<String, String> {
    let a: CohereArgs = serde_json::from_value(args).map_err(|e| format!("Invalid cohere arguments: {}", e))?;
    let params = crate::models::CohereParams {
        dry_run: a.dry_run,
        max_links: a.max_links,
        ..Default::default()
    };
    let report = db
        .cohere(&params)
        .map_err(|e| format!("Cohere failed: {}", e))?;

    serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
}

// ─── mimir_supersede handler ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SupersedeArgs {
    pub from_category: String,
    pub from_key: String,
    pub to_category: String,
    pub to_key: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default = "default_relationship")]
    pub relationship: String,
}

fn default_relationship() -> String {
    "supersedes".to_string()
}

pub fn handle_supersede(db: &Database, args: Value) -> Result<String, String> {
    let a: SupersedeArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid supersede arguments: {}", e))?;

    // Find the 'from' entity
    let from_entity = db
        .get_entity(&a.from_category, &a.from_key)
        .map_err(|e| format!("'From' entity lookup failed: {}", e))?
        .ok_or_else(|| format!("'From' entity not found: {}/{}", a.from_category, a.from_key))?;

    // Find the 'to' entity
    let to_entity = db
        .get_entity(&a.to_category, &a.to_key)
        .map_err(|e| format!("'To' entity lookup failed: {}", e))?
        .ok_or_else(|| format!("'To' entity not found: {}/{}", a.to_category, a.to_key))?;

    // 1. Create a "supersedes" relationship link
    db.link(
        &to_entity.category,
        &to_entity.key,
        &from_entity.id,
        &a.relationship,
    )
    .map_err(|e| format!("Supersede link failed: {}", e))?;

    // 2. Set the OLD entity's status to "deprecated"
    db.update_entity_status(&from_entity.id, "deprecated", &a.reason)
        .map_err(|e| format!("Failed to deprecate 'from' entity: {}", e))?;

    let result = json!({
        "from_entity_id": from_entity.id,
        "from_entity_category": from_entity.category,
        "from_entity_key": from_entity.key,
        "to_entity_id": to_entity.id,
        "to_entity_category": to_entity.category,
        "to_entity_key": to_entity.key,
        "relationship": a.relationship,
        "status_updated": "deprecated",
    });
    Ok(result.to_string())
}

// ─── mimir_maintenance handler ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MaintenanceArgs {
    #[serde(default)]
    pub dedup: bool,
    #[serde(default)]
    pub orphans: bool,
    #[serde(default)]
    pub vacuum: bool,
    #[serde(default)]
    pub reindex: bool,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub dry_run: bool,
}

pub fn handle_maintenance(db: &Database, args: Value) -> Result<String, String> {
    let a: MaintenanceArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid maintenance arguments: {}", e))?;

    let mut report = json!({
        "dedup_archived": 0,
        "orphan_journal_entries_found": 0,
        "orphan_links_found": 0,
        "vacuum_reclaimed_bytes": 0,
        "reindex_rows_affected": 0,
        "dry_run": a.dry_run,
        "errors": []
    });

    let current_db_size = db
        .file_size_bytes()
        .map_err(|e| format!("Failed to get DB size: {}", e))?;

    // Dedup
    if a.dedup || a.all {
        match db.deduplicate_entities(a.dry_run) {
            Ok(dedup_count) => {
                report["dedup_archived"] = json!(dedup_count);
            }
            Err(e) => report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("Dedup failed: {}", e))),
        }
    }

    // Orphans
    if a.orphans || a.all {
        match db.detect_orphan_journal_entries() {
            Ok(orphans_count) => {
                report["orphan_journal_entries_found"] = json!(orphans_count);
            }
            Err(e) => report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("Orphan journal detection failed: {}", e))),
        }
        match db.detect_orphan_links() {
            Ok(orphans_count) => {
                report["orphan_links_found"] = json!(orphans_count);
            }
            Err(e) => report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("Orphan link detection failed: {}", e))),
        }
    }

    // Vacuum
    if (a.vacuum || a.all) && !a.dry_run {
        match db.vacuum() {
            Ok(_) => {
                let after_vacuum_db_size = db
                    .file_size_bytes()
                    .map_err(|e| format!("Failed to get DB size after vacuum: {}", e))?;
                report["vacuum_reclaimed_bytes"] = json!(current_db_size as i64 - after_vacuum_db_size as i64);
            }
            Err(e) => report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("Vacuum failed: {}", e))),
        }
    }

    // Reindex
    if (a.reindex || a.all) && !a.dry_run {
        match db.reindex_fts() {
            Ok(n) => {
                report["reindex_rows_affected"] = json!(n);
            }
            Err(e) => report["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(format!("Reindex failed: {}", e))),
        }
    }

    Ok(report.to_string())
}

// ─── mimir_correct handler ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CorrectArgs {
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


pub fn handle_correct(db: &Database, args: Value) -> Result<String, String> {
    let a: CorrectArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid correct arguments: {}", e))?;

    let params = crate::models::CorrectParams {
        wrong_approach: a.wrong_approach,
        user_correction: a.user_correction,
        task_context: a.task_context,
        session_id: a.session_id,
        tags: a.tags,
        category: a.category,
        visibility: a.visibility,
    };

    let result = db.correct(&params)
        .map_err(|e| format!("Correct failed: {}", e))?;

    serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
}

// ─── mimir_synthesize handler ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SynthesizeArgs {
    pub session_content: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub visibility: String,
}

pub fn handle_synthesize(db: &Database, args: Value) -> Result<String, String> {
    let a: SynthesizeArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid synthesize arguments: {}", e))?;

    let params = crate::models::SynthesizeParams {
        session_content: a.session_content,
        session_id: a.session_id,
        tags: a.tags,
        visibility: a.visibility,
    };

    let result = db.synthesize(&params)
        .map_err(|e| format!("Synthesize failed: {}", e))?;

    serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
}


// ─── mimir_bench handler ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BenchArgs {
    pub task_description: String,
    pub turns_taken: i64,
    pub tokens_used: i64,
    pub memory_recall_used: bool,
    #[serde(default)]
    pub recall_count: i64,
    #[serde(default)]
    pub task_success: bool,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

pub fn handle_bench(db: &Database, args: Value) -> Result<String, String> {
    let a: BenchArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid bench arguments: {}", e))?;

    let params = crate::models::BenchParams {
        task_description: a.task_description,
        turns_taken: a.turns_taken,
        tokens_used: a.tokens_used,
        memory_recall_used: a.memory_recall_used,
        recall_count: a.recall_count,
        task_success: a.task_success,
        session_id: a.session_id,
        tags: a.tags,
    };

    let result = db.bench(&params)
        .map_err(|e| format!("Bench failed: {}", e))?;

    serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
}

/// Permanently delete all archived entities and VACUUM the database.
#[derive(Debug, Deserialize)]
pub struct PurgeArgs {
    #[serde(default)]
    pub dry_run: bool,
}

pub fn handle_purge(db: &Database, args: Value) -> Result<String, String> {
    let a: PurgeArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid purge arguments: {}", e))?;
    let report = db.purge(a.dry_run)
        .map_err(|e| format!("Purge failed: {}", e))?;
    serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
}


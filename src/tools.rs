use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::{now_ms, Database};
use crate::models::{
    AskParams, EmbedParams, Entity, IngestParams, JournalEvent, PruneParams, RecallParams,
    SearchMode, StateEntry, TimelineParams,
};

// ─── Deserialization structs ────────────────────────────────────

/// #330: many MCP clients send explicit JSON `null` for an optional field
/// they didn't set (rather than omitting the key), because the tool schema
/// lists the field as optional/defaulted. serde's `#[serde(default = "...")]`
/// only fires when the key is *absent*; a present `null` still hits the
/// field's real type and fails with a misleading "invalid type: null,
/// expected a string/boolean/f64/..." error that names the wrong field
/// entirely once combined with `#[serde(deny_unknown_fields)]`-style
/// confusion. This helper treats an explicit `null` the same as an absent
/// key by falling through to `Default::default()` for the field type; pair
/// it with `#[serde(default = "...", deserialize_with = "null_as_default")]`
/// when the field also needs a non-Default::default() default value.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub struct RememberArgs {
    pub category: String,
    pub key: String,
    pub body_json: String,
    #[serde(
        default = "default_status",
        deserialize_with = "null_as_default_status"
    )]
    pub status: String,
    #[serde(
        default = "default_entity_type",
        rename = "type",
        deserialize_with = "null_as_default_entity_type"
    )]
    pub entity_type: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tags: Vec<String>,
    #[serde(
        default = "default_importance",
        deserialize_with = "null_as_default_importance"
    )]
    pub importance: f64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub topic_path: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub recall_when: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub always_on: bool,
    #[serde(
        default = "default_certainty",
        deserialize_with = "null_as_default_certainty"
    )]
    pub certainty: f64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub workspace_hash: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub agent_id: String,
    #[serde(
        default = "default_visibility",
        deserialize_with = "null_as_default_visibility"
    )]
    pub visibility: String,
    #[serde(default)]
    pub layer: Option<String>,
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

/// #330: same null-tolerance as `null_as_default`, but falls through to a
/// named default function instead of `T::default()` for fields whose
/// "unset" value isn't the type's zero value (e.g. status="active", not "").
macro_rules! null_as_named_default {
    ($fn_name:ident, $ty:ty, $default_fn:ident) => {
        fn $fn_name<'de, D>(deserializer: D) -> Result<$ty, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            Ok(Option::<$ty>::deserialize(deserializer)?.unwrap_or_else($default_fn))
        }
    };
}

null_as_named_default!(null_as_default_status, String, default_status);
null_as_named_default!(null_as_default_entity_type, String, default_entity_type);
null_as_named_default!(null_as_default_importance, f64, default_importance);
null_as_named_default!(null_as_default_certainty, f64, default_certainty);
null_as_named_default!(null_as_default_visibility, String, default_visibility);

#[derive(Debug, Deserialize)]
pub struct RecallArgs {
    pub query: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(
        default = "default_limit",
        deserialize_with = "null_as_default_limit"
    )]
    pub limit: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub offset: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub min_decay: f64,
    #[serde(default)]
    pub topic_path: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub include_archived: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub expansion: crate::models::QueryExpansionConfig,
    #[serde(default, deserialize_with = "null_as_default")]
    pub mode: String, // "fts5", "dense", or "hybrid"
    #[serde(default)]
    pub preview_cap: Option<i64>,
    #[serde(default)]
    pub always_on: Option<bool>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub content_weight: f64,
    #[serde(
        default = "crate::models::default_trust_weight",
        deserialize_with = "null_as_default_trust_weight"
    )]
    pub trust_weight: f64,
    #[serde(
        default = "default_halving",
        deserialize_with = "null_as_default_halving"
    )]
    pub diversity_halving: f64,
    /// Recency half-life in seconds for time-aware hybrid ranking (#235).
    /// Omit (default) for relevance-only ranking; set to bias toward recent memories.
    #[serde(default)]
    pub recency_half_life_secs: Option<f64>,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub layer: Option<String>,
    /// #287: opt-in. When true, each result gets a normalized `confidence`
    /// (0.0–1.0) rolled up from rank, trust, and decay. Default false so
    /// existing callers and snapshot tests are unaffected; ranking is unchanged.
    #[serde(default, deserialize_with = "null_as_default")]
    pub include_confidence: bool,
    /// Opt-in reinforcement for dense/hybrid recall: bump retrieval stats on
    /// the returned hits so semantically-used memories resist decay. Default
    /// false — the semantic paths stay byte-deterministic (#247).
    #[serde(default, deserialize_with = "null_as_default")]
    pub reinforce: bool,
}

/// #287: presentation-layer confidence rollup over signals Mneme already has.
/// Does NOT affect ranking — purely a convenience score for the caller.
fn confidence_for(entity: &crate::models::Entity, rank: usize, total: usize) -> f64 {
    let relevance = if total > 1 {
        (total - rank) as f64 / total as f64
    } else {
        1.0
    };
    let trust = if entity.verified {
        1.0
    } else {
        entity.certainty.clamp(0.0, 1.0)
    };
    let freshness = entity.decay_score.clamp(0.0, 1.0);
    let c = 0.5 * relevance + 0.3 * trust + 0.2 * freshness;
    (c.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
}

/// Inject a `confidence` field into each already-serialized recall item.
fn apply_confidence(items: &mut [serde_json::Value], entities: &[crate::models::Entity]) {
    let total = entities.len();
    for (i, (item, ent)) in items.iter_mut().zip(entities.iter()).enumerate() {
        if let Some(obj) = item.as_object_mut() {
            obj.insert("confidence".to_string(), json!(confidence_for(ent, i, total)));
        }
    }
}

/// Map a biomimetic layer alias (world/episodic/semantic) to its canonical
/// storage layer (core/buffer/working). Any other value passes through, so
/// callers may also filter by the raw layer name.
fn canonical_layer(s: &str) -> String {
    match s {
        "world" => "core",
        "episodic" => "buffer",
        "semantic" => "working",
        other => other,
    }
    .to_string()
}

fn default_halving() -> f64 {
    1.0
}

fn default_limit() -> i64 {
    10
}

null_as_named_default!(null_as_default_limit, i64, default_limit);
null_as_named_default!(
    null_as_default_trust_weight,
    f64,
    default_trust_weight_wrapper
);
null_as_named_default!(null_as_default_halving, f64, default_halving);

fn default_trust_weight_wrapper() -> f64 {
    crate::models::default_trust_weight()
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
    #[serde(default)]
    pub workspace_hash: Option<String>,
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

    let layer = a.layer.map(|l| match l.as_str() {
        "world" => "core".to_string(),
        "episodic" => "buffer".to_string(),
        "semantic" => "working".to_string(),
        _ => l,
    }).unwrap_or_else(|| "buffer".to_string());

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
        layer,
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
        follow_count: 0,
        miss_count: 0,
        follow_rate: 0.0,
        efficacy_status: "unverified".to_string(),
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

    // #271: an unset `mode` ("" — the serde default) auto-selects the best
    // available strategy. When the embedding backend is on AND at least one
    // entity is embedded, default to Hybrid (deterministic dense + keyword RRF);
    // otherwise fall back to keyword FTS5 exactly as before. An explicit mode
    // always wins.
    let mode = match a.mode.as_str() {
        "dense" => SearchMode::Dense,
        "hybrid" => SearchMode::Hybrid,
        "fts5" => SearchMode::Fts5,
        "" => {
            if db.embedding_enabled() && db.embedding_coverage() > 0 {
                SearchMode::Hybrid
            } else {
                SearchMode::Fts5
            }
        }
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
        layer: a.layer.as_deref().filter(|s| !s.is_empty()).map(canonical_layer),
        reinforce: a.reinforce,
    };

    let entities = db
        .recall(&params)
        .map_err(|e| format!("Recall failed: {}", e))?;

    let mut items_expanded: Vec<serde_json::Value> =
        entities.iter().map(|e| e.to_json_expanded()).collect();

    if a.include_confidence {
        apply_confidence(&mut items_expanded, &entities);
    }

    let result = json!({
        "items": items_expanded,
        "total": items_expanded.len(),
    });
    Ok(result.to_string())
}

#[derive(Debug, Deserialize)]
pub struct SemanticSearchArgs {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// #271: `mimir_semantic_search` — dense-only semantic search shortcut. Unlike
/// `mimir_recall` (which fuses keyword + dense in hybrid mode), this runs the
/// pure dense vector arm with NO FTS5 fallback: results are ranked solely by
/// embedding cosine similarity. Requires an embedding backend (on by default via
/// the bundled in-process ONNX model). Errors clearly when no backend is
/// available rather than silently degrading to keyword search.
pub fn handle_semantic_search(db: &Database, args: Value) -> Result<String, String> {
    let a: SemanticSearchArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid semantic_search arguments: {}", e))?;

    let params = RecallParams {
        query: a.query,
        category: a.category,
        limit: a.limit,
        skip_side_effects: false,
        mode: SearchMode::Dense,
        workspace_hash: a.workspace_hash,
        agent_id: a.agent_id,
        ..RecallParams::default()
    };

    let entities = db
        .recall(&params)
        .map_err(|e| format!("Semantic search failed: {}", e))?;

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
            layer: a.layer.as_deref().filter(|s| !s.is_empty()).map(canonical_layer),
            // Fts5-only path: reinforcement is handled by the batched
            // side-effect below, not the per-variant recalls.
            reinforce: false,
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

    let mut items_expanded: Vec<serde_json::Value> = merged
        .iter()
        .map(|(entity, _)| entity.to_json_expanded())
        .collect();

    if a.include_confidence {
        let total = merged.len();
        for (i, (item, (entity, _))) in items_expanded.iter_mut().zip(merged.iter()).enumerate() {
            if let Some(obj) = item.as_object_mut() {
                obj.insert("confidence".to_string(), json!(confidence_for(entity, i, total)));
            }
        }
    }

    let result = json!({
        "items": items_expanded,
        "total": items_expanded.len(),
        "variants": variants.len(),
    });
    Ok(result.to_string())
}

#[derive(Debug, Deserialize)]
pub struct RecallLayerArgs {
    pub layer: String,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

pub fn handle_recall_layer(db: &Database, args: Value) -> Result<String, String> {
    let a: RecallLayerArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid recall_layer arguments: {}", e))?;

    let layer = match a.layer.as_str() {
        "world" => "core",
        "episodic" => "buffer",
        "semantic" => "working",
        _ => &a.layer,
    };

    let recall_args = json!({
        "query": "",
        "limit": a.limit,
        "layer": layer,
    });

    handle_recall(db, recall_args)
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

/// #269/#272 review follow-up: surface the bi-temporal version trail.
/// `history_versions` existed + was tested but no tool reached it.
pub fn handle_history(db: &Database, args: Value) -> Result<String, String> {
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'category' parameter".to_string())?;
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'key' parameter".to_string())?;

    let versions = db
        .history_versions(category, key)
        .map_err(|e| format!("history failed: {}", e))?;

    let items: Vec<serde_json::Value> = versions.iter().map(|e| e.to_json_expanded()).collect();
    let result = json!({
        "category": category,
        "key": key,
        "versions": items,
        "total": items.len(),
    });
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

    match db.context(&a.categories, a.limit, a.workspace_hash.as_deref()) {
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
pub struct FollowArgs {
    pub category: String,
    pub key: String,
    pub followed: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub context: Option<String>,
}

/// Record whether an entity (convention/insight/lesson) was actually FOLLOWED
/// or MISSED by the agent — the PMB-inspired "honest follow-rate" signal.
/// `context` is accepted for future auto-detection/audit use but not yet
/// persisted; the tool records a manual confirm/deny each call.
pub fn handle_follow(db: &Database, args: Value) -> Result<String, String> {
    let a: FollowArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid follow arguments: {}", e))?;

    let report = db
        .follow(&a.category, &a.key, a.followed)
        .map_err(|e| format!("Follow failed: {}", e))?;

    serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
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
    /// Opt-in: actively invalidate the lower-certainty side of clear conflicts
    /// (default false = read-only detection, the long-standing behavior).
    #[serde(default)]
    pub resolve: bool,
    /// When resolving, only report what would change unless explicitly false.
    /// Defaults true so an accidental `resolve:true` previews rather than mutates.
    #[serde(default = "default_true")]
    pub dry_run: bool,
    /// Minimum certainty gap to auto-resolve a conflict; closer pairs are
    /// skipped as ambiguous.
    #[serde(default = "default_certainty_margin")]
    pub certainty_margin: f64,
}

fn default_conflict_threshold() -> f64 {
    0.4
}
fn default_conflict_limit() -> i64 {
    10
}
fn default_true() -> bool {
    true
}
fn default_certainty_margin() -> f64 {
    0.2
}

pub fn handle_conflicts(db: &Database, args: Value) -> String {
    let a: ConflictArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => {
            return json!({"error": format!("Invalid conflicts arguments: {}", e)}).to_string()
        }
    };
    if a.resolve {
        return match db.resolve_conflicts(
            &a.category,
            a.threshold,
            a.limit,
            a.offset,
            a.certainty_margin,
            a.dry_run,
        ) {
            Ok(report) => serde_json::to_string(&report)
                .unwrap_or_else(|e| json!({"error": format!("{}", e)}).to_string()),
            Err(e) => json!({"error": format!("Conflict resolution failed: {}", e)}).to_string(),
        };
    }
    match db.detect_conflicts(&a.category, a.threshold, a.limit, a.offset) {
        Ok(report) => serde_json::to_string(&report)
            .unwrap_or_else(|e| json!({"error": format!("{}", e)}).to_string()),
        Err(e) => json!({"error": format!("Conflict detection failed: {}", e)}).to_string(),
    }
}

pub fn handle_consolidate(db: &Database, args: Value) -> String {
    let params: crate::models::ConsolidateParams = match serde_json::from_value(args) {
        Ok(p) => p,
        Err(e) => {
            return json!({"error": format!("Invalid consolidate arguments: {}", e)}).to_string()
        }
    };
    match db.consolidate(&params) {
        Ok(report) => serde_json::to_string(&report)
            .unwrap_or_else(|e| json!({"error": format!("{}", e)}).to_string()),
        Err(e) => json!({"error": format!("Consolidation failed: {}", e)}).to_string(),
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
        follow_count: 0,
        miss_count: 0,
        follow_rate: 0.0,
        efficacy_status: "unverified".to_string(),
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
    #[serde(default)]
    pub workspace_hash: Option<String>,
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
        .recall_when(&a.context, a.limit, a.workspace_hash.as_deref())
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

    // Snapshot the DB size BEFORE any mutation so db_size_delta_bytes is
    // meaningful — it was previously read after all three steps had run, so
    // the reported delta was always ≈0.
    let initial_db_size = db
        .file_size_bytes()
        .map_err(|e| format!("Failed to get initial DB size: {}", e))?;

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

    // 3. Then mimir_compact (archive below threshold). Use the same archive
    // threshold as decay_tick/cohere so "run everything" forgets at the same
    // point as the individual tools (was a hardcoded 0.1 → ~5 idle days sooner).
    let compact_report = db
        .compact(Database::ARCHIVE_DECAY_THRESHOLD, a.dry_run)
        .map_err(|e| format!("Autocohere step (compact) failed: {}", e))?;

    // 4. Consolidation ("local dreaming"): compress the coldest overlapping
    // memories in each category into evidence-tracked observations and retire
    // the merged sources — running in the BACKGROUND as part of "run
    // everything", instead of only when an agent thinks to call
    // mimir_consolidate. Bounded: a few observations per category per run,
    // over the same scan window the manual tool uses. Skips 'observation'
    // (no meta-observations / runaway recursion) and 'memories' (files from
    // the /memories adapter must never be similarity-merged).
    let mut observations_created = 0i64;
    let mut consolidate_sources_archived = 0i64;
    let categories = db
        .workspace_list_categories()
        .map_err(|e| format!("Autocohere step (consolidate: categories) failed: {}", e))?;
    for cat in categories {
        if cat == "observation" || cat == "memories" {
            continue;
        }
        let report = db
            .consolidate(&crate::models::ConsolidateParams {
                category: cat.clone(),
                similarity_threshold: 0.6,
                limit: 5,
                offset: 0,
                dry_run: a.dry_run,
                cold_first: true,
                archive_sources: true,
            })
            .map_err(|e| format!("Autocohere step (consolidate {}) failed: {}", cat, e))?;
        observations_created += report.observations_created;
        consolidate_sources_archived += report.sources_archived;
    }

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
        "observations_created": observations_created,
        "consolidate_sources_archived": consolidate_sources_archived,
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

// ─── /memories directory-convention adapter ──────────────────────
//
// Implements Anthropic's memory-tool convention (the `memory_20250818`
// command set: view / create / str_replace / insert / delete / rename over
// paths under /memories) on top of the entity store, so clients built
// against Claude's native memory tool can point at the vault unchanged.
// Files are entities in the reserved `memories` category with key = the
// path relative to /memories; bodies are the raw file text (FTS-indexed,
// encrypted at rest like any entity, and versioned through the normal
// bi-temporal history on edit).

const MEMORIES_CATEGORY: &str = "memories";

#[derive(Debug, Deserialize)]
pub struct MemoriesArgs {
    pub command: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub file_text: String,
    #[serde(default)]
    pub old_str: String,
    #[serde(default)]
    pub new_str: String,
    #[serde(default)]
    pub insert_line: i64,
    #[serde(default)]
    pub insert_text: String,
    #[serde(default)]
    pub old_path: String,
    #[serde(default)]
    pub new_path: String,
}

/// Normalize a /memories path to an entity key. Rejects traversal and
/// absolute-elsewhere paths rather than silently reinterpreting them.
fn memories_key(path: &str) -> Result<String, String> {
    let p = path.trim().replace('\\', "/");
    let rel = p
        .strip_prefix("/memories/")
        .or_else(|| p.strip_prefix("memories/"))
        .unwrap_or(p.trim_start_matches('/'));
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return Err("path must name a file under /memories".to_string());
    }
    if rel.split('/').any(|seg| seg == "." || seg == ".." || seg.is_empty()) {
        return Err(format!("invalid path: {}", path));
    }
    Ok(rel.to_string())
}

/// True when the path means the /memories directory itself.
fn is_memories_root(path: &str) -> bool {
    let p = path.trim().trim_end_matches('/');
    p.is_empty() || p == "/memories" || p == "memories" || p == "/"
}

fn memories_file(db: &Database, key: &str) -> Result<Option<crate::models::Entity>, String> {
    db.get_entity(MEMORIES_CATEGORY, key)
        .map_err(|e| format!("read failed: {}", e))
        .map(|opt| opt.filter(|e| !e.archived))
}

fn memories_write(
    db: &Database,
    key: &str,
    text: &str,
    existing: Option<&crate::models::Entity>,
) -> Result<(), String> {
    let now = crate::db::now_ms();
    let entity = match existing {
        // Preserve identity/stats on edit; remember()'s update path snapshots
        // the prior version into entity_history (versioned files for free).
        Some(prev) => crate::models::Entity {
            body_json: text.to_string(),
            archived: false,
            archive_reason: String::new(),
            last_accessed_unix_ms: now,
            ..prev.clone()
        },
        None => {
            let raw_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            crate::models::Entity {
                id: format!("memf-{}", &raw_id[..12.min(raw_id.len())]),
                category: MEMORIES_CATEGORY.to_string(),
                key: key.to_string(),
                body_json: text.to_string(),
                status: "active".to_string(),
                entity_type: "file".to_string(),
                tags: vec!["memories".to_string()],
                decay_score: 1.0,
                retrieval_count: 0,
                layer: "working".to_string(),
                topic_path: String::new(),
                archived: false,
                archive_reason: String::new(),
                links: vec![],
                verified: false,
                source: "memories-adapter".to_string(),
                always_on: false,
                certainty: 0.5,
                workspace_hash: String::new(),
                agent_id: String::new(),
                visibility: "workspace".to_string(),
                created_at_unix_ms: now,
                last_accessed_unix_ms: now,
                follow_count: 0,
                miss_count: 0,
                follow_rate: 0.0,
                efficacy_status: "unverified".to_string(),
                embedding: None,
            }
        }
    };
    // skip_dedup: a deliberate file write must create THIS path even when a
    // similar file already exists under another path.
    db.remember_skip_dedup(&entity)
        .map(|_| ())
        .map_err(|e| format!("write failed: {}", e))
}

pub fn handle_memories(db: &Database, args: Value) -> Result<String, String> {
    let a: MemoriesArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid memories arguments: {}", e))?;

    match a.command.as_str() {
        "view" => {
            if is_memories_root(&a.path) {
                // No workspace filter: the adapter writes files with the
                // global ('') workspace, and #346's list_entities gained a
                // workspace_hash arg after this call was written.
                let entries = db
                    .list_entities(0, 1000, Some(MEMORIES_CATEGORY), None, None)
                    .map_err(|e| format!("list failed: {}", e))?;
                let mut names: Vec<String> =
                    entries.iter().map(|e| e.key.clone()).collect();
                names.sort();
                return Ok(json!({
                    "directory": "/memories",
                    "files": names,
                    "total": names.len(),
                })
                .to_string());
            }
            let key = memories_key(&a.path)?;
            let file = memories_file(db, &key)?
                .ok_or_else(|| format!("file not found: /memories/{}", key))?;
            // cat -n style numbering, matching the native memory tool's view.
            let numbered: String = file
                .body_json
                .lines()
                .enumerate()
                .map(|(i, l)| format!("{:>6}\t{}\n", i + 1, l))
                .collect();
            Ok(json!({
                "path": format!("/memories/{}", key),
                "content": numbered,
            })
            .to_string())
        }
        "create" => {
            let key = memories_key(&a.path)?;
            // Anthropic semantics: create overwrites an existing file.
            let existing = memories_file(db, &key)?;
            memories_write(db, &key, &a.file_text, existing.as_ref())?;
            Ok(json!({"path": format!("/memories/{}", key), "action": "created"}).to_string())
        }
        "str_replace" => {
            let key = memories_key(&a.path)?;
            let file = memories_file(db, &key)?
                .ok_or_else(|| format!("file not found: /memories/{}", key))?;
            let occurrences = file.body_json.matches(&a.old_str).count();
            if a.old_str.is_empty() {
                return Err("old_str must not be empty".to_string());
            }
            if occurrences == 0 {
                return Err(format!("old_str not found in /memories/{}", key));
            }
            if occurrences > 1 {
                return Err(format!(
                    "old_str occurs {} times in /memories/{} — must be unique",
                    occurrences, key
                ));
            }
            let updated = file.body_json.replacen(&a.old_str, &a.new_str, 1);
            memories_write(db, &key, &updated, Some(&file))?;
            Ok(json!({"path": format!("/memories/{}", key), "action": "replaced"}).to_string())
        }
        "insert" => {
            let key = memories_key(&a.path)?;
            let file = memories_file(db, &key)?
                .ok_or_else(|| format!("file not found: /memories/{}", key))?;
            let mut lines: Vec<&str> = file.body_json.lines().collect();
            let at = a.insert_line.clamp(0, lines.len() as i64) as usize;
            lines.insert(at, &a.insert_text);
            let updated = lines.join("\n");
            memories_write(db, &key, &updated, Some(&file))?;
            Ok(json!({
                "path": format!("/memories/{}", key),
                "action": "inserted",
                "at_line": at,
            })
            .to_string())
        }
        "delete" => {
            let key = memories_key(&a.path)?;
            let removed = db
                .forget(MEMORIES_CATEGORY, &key, "memories: delete command")
                .map_err(|e| format!("delete failed: {}", e))?;
            if !removed {
                return Err(format!("file not found: /memories/{}", key));
            }
            Ok(json!({"path": format!("/memories/{}", key), "action": "deleted"}).to_string())
        }
        "rename" => {
            let old_key = memories_key(&a.old_path)?;
            let new_key = memories_key(&a.new_path)?;
            let file = memories_file(db, &old_key)?
                .ok_or_else(|| format!("file not found: /memories/{}", old_key))?;
            if memories_file(db, &new_key)?.is_some() {
                return Err(format!("destination exists: /memories/{}", new_key));
            }
            memories_write(db, &new_key, &file.body_json, None)?;
            db.forget(MEMORIES_CATEGORY, &old_key, "memories: renamed")
                .map_err(|e| format!("rename cleanup failed: {}", e))?;
            Ok(json!({
                "from": format!("/memories/{}", old_key),
                "to": format!("/memories/{}", new_key),
                "action": "renamed",
            })
            .to_string())
        }
        other => Err(format!(
            "unknown command '{}' (expected view/create/str_replace/insert/delete/rename)",
            other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #330: mimir_remember rejected the documented optional `topic_path`
    // field (and other optional fields with custom defaults) whenever a
    // caller sent explicit JSON `null` instead of omitting the key. Many
    // MCP clients do this because the tool schema lists the field as
    // optional/defaulted, not because they're being unusual.

    #[test]
    fn remember_args_accepts_null_topic_path() {
        let v = json!({
            "category": "reference",
            "key": "example-key",
            "body_json": "{}",
            "topic_path": null
        });
        let a: RememberArgs = serde_json::from_value(v).expect("null topic_path must deserialize");
        assert_eq!(a.topic_path, "");
    }

    #[test]
    fn remember_args_accepts_null_for_every_optional_field_with_custom_default() {
        // Explicit null on each of these must fall back to that field's
        // documented default, not fail deserialization.
        for field in [
            "status",
            "type",
            "tags",
            "importance",
            "topic_path",
            "recall_when",
            "always_on",
            "certainty",
            "workspace_hash",
            "agent_id",
            "visibility",
        ] {
            let mut v = json!({
                "category": "reference",
                "key": "example-key",
                "body_json": "{}",
            });
            v.as_object_mut()
                .unwrap()
                .insert(field.to_string(), Value::Null);
            let result: Result<RememberArgs, _> = serde_json::from_value(v);
            assert!(
                result.is_ok(),
                "field `{}` with explicit null should deserialize, got {:?}",
                field,
                result.err()
            );
        }
    }

    #[test]
    fn remember_args_still_reports_missing_category_correctly() {
        // Regression guard: fixing the null-tolerance bug must not break the
        // genuinely-missing-required-field error path (the original bug
        // report's error message pointed at the wrong field — `category` —
        // when the real offender was `topic_path: null`; once null is
        // handled, a real missing `category` must still be reported as such).
        let v = json!({ "key": "example-key", "body_json": "{}" });
        let result: Result<RememberArgs, _> = serde_json::from_value(v);
        let err = result.expect_err("missing category must fail").to_string();
        assert!(
            err.contains("category"),
            "error should name the actually-missing field `category`, got: {}",
            err
        );
    }

    #[test]
    fn recall_args_accepts_null_for_every_optional_field_with_custom_default() {
        for field in [
            "limit",
            "offset",
            "min_decay",
            "include_archived",
            "expansion",
            "mode",
            "content_weight",
            "trust_weight",
            "diversity_halving",
            "include_confidence",
        ] {
            let mut v = json!({ "query": "test" });
            v.as_object_mut()
                .unwrap()
                .insert(field.to_string(), Value::Null);
            let result: Result<RecallArgs, _> = serde_json::from_value(v);
            assert!(
                result.is_ok(),
                "field `{}` with explicit null should deserialize, got {:?}",
                field,
                result.err()
            );
        }
    }

    #[test]
    fn recall_args_null_limit_falls_back_to_default_ten() {
        let v = json!({ "query": "test", "limit": null });
        let a: RecallArgs = serde_json::from_value(v).unwrap();
        assert_eq!(a.limit, 10);
    }
}



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
    /// Application-time period (#363, SQL:2011 APPLICATION_TIME): when the
    /// fact became true in the world. Defaults to transaction time. Set in
    /// the past for retroactive facts ("this was true last week").
    #[serde(default)]
    pub valid_from_unix_ms: Option<i64>,
    /// When the fact stopped being true. Omit for "still true" (unbounded).
    #[serde(default)]
    pub valid_to_unix_ms: Option<i64>,
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
    /// #363: valid-time instant filter — only return facts whose application-
    /// time period [valid_from, valid_to) contains this world-instant.
    #[serde(default)]
    pub valid_at: Option<i64>,
    /// #363: valid-time period filter start (pair with valid_to_unix_ms and
    /// valid_op). Ignored when valid_at is set.
    #[serde(default)]
    pub valid_from_unix_ms: Option<i64>,
    /// #363: valid-time period filter end (half-open; omit = unbounded).
    #[serde(default)]
    pub valid_to_unix_ms: Option<i64>,
    /// #363: SQL:2011 period predicate for the period filter: "overlaps"
    /// (default — periods share an instant) or "contains" (the fact's period
    /// contains the whole queried period).
    #[serde(default, deserialize_with = "null_as_default")]
    pub valid_op: String,
}

/// #363: post-search valid-time filter shared by the plain and expansion
/// recall paths. Applied AFTER ranking/limit, so it only ever narrows the
/// result set (no re-ranking): callers that never pass valid-time filters get
/// byte-identical output. No-op when no filter is requested.
fn valid_time_retain(
    db: &Database,
    valid_at: Option<i64>,
    valid_from: Option<i64>,
    valid_to: Option<i64>,
    valid_op: &str,
    entities: &mut Vec<crate::models::Entity>,
) -> Result<(), String> {
    if valid_at.is_none() && valid_from.is_none() && valid_to.is_none() {
        return Ok(());
    }
    let ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
    let periods = db
        .valid_periods_for_ids(&ids)
        .map_err(|e| format!("valid-time filter failed: {}", e))?;
    entities.retain(|e| {
        let Some(&(row_from, row_to)) = periods.get(&e.id) else {
            return false;
        };
        if let Some(t) = valid_at {
            return crate::db::valid_period_contains_instant(row_from, row_to, t);
        }
        // Period query: [from, to) with unbounded defaults on either side.
        crate::db::valid_period_matches(
            row_from,
            row_to,
            valid_from.unwrap_or(i64::MIN),
            valid_to,
            valid_op,
        )
    });
    Ok(())
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
    #[serde(default, deserialize_with = "null_as_default")]
    pub categories: Vec<String>,
    #[serde(default = "default_context_limit")]
    pub limit: i64,
    #[serde(default)]
    pub workspace_hash: Option<String>,
    /// Current task/message text — the relevance gate for recall-first
    /// injection (#356). Without it, on_demand mode injects no topical
    /// entities (compact pointer + capped always-on set only).
    #[serde(default)]
    pub query: Option<String>,
    /// "on_demand" (default, recall-first) or "always_inject" (legacy
    /// unconditional dump, opt-in) (#366).
    #[serde(default)]
    pub mode: Option<String>,
    /// Host model name for budget-profile resolution (#366).
    #[serde(default)]
    pub model: Option<String>,
    /// Explicit character budget; overrides the model profile (#366).
    #[serde(default)]
    pub max_context_chars: Option<i64>,
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

    // #363: half-open [valid_from, valid_to) must be a real interval.
    if let (Some(vf), Some(vt)) = (a.valid_from_unix_ms, a.valid_to_unix_ms) {
        if vt <= vf {
            return Err(format!(
                "valid_to_unix_ms ({vt}) must be greater than valid_from_unix_ms ({vf})"
            ));
        }
    }

    let (eid, action) = db
        .remember_with_validity(&entity, a.valid_from_unix_ms, a.valid_to_unix_ms)
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

    // #363 review: valid_op is a closed SQL:2011 enum — reject unknown strings
    // instead of silently treating them as 'overlaps'. Validated up front so
    // the expansion path is covered too. "" is the serde default (= overlaps).
    match a.valid_op.as_str() {
        "" | "overlaps" | "contains" => {}
        other => {
            return Err(format!(
                "Invalid valid_op '{other}': expected 'overlaps' or 'contains'"
            ))
        }
    }

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

    // #363: captured before RecallParams moves fields out of `a`.
    let (valid_at, valid_from, valid_to) = (a.valid_at, a.valid_from_unix_ms, a.valid_to_unix_ms);
    let valid_op = a.valid_op.clone();
    let temporal_filtering = valid_at.is_some() || valid_from.is_some() || valid_to.is_some();
    let mode_for_side_effects = mode.clone();
    let reinforce_requested = a.reinforce;

    let params = RecallParams {
        query: a.query,
        category: a.category,
        entity_type: a.entity_type,
        limit: a.limit,
        offset: a.offset,
        min_decay: a.min_decay,
        topic_path: a.topic_path,
        include_archived: a.include_archived,
        // #363 review (a #356-class value inversion): with a valid-time filter
        // present, the inner recall must be a PURE read — the fts5 path (and
        // dense/hybrid with reinforce) otherwise reinforces every matched row,
        // including the ones the filter is about to hide, so repeatedly asking
        // "what was true at T" would make the invisible entities immortal.
        // Side-effects are applied below to the SURVIVING hits only, mirroring
        // the expansion path. Unfiltered calls keep the original behavior.
        skip_side_effects: temporal_filtering,
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

    let mut entities = db
        .recall(&params)
        .map_err(|e| format!("Recall failed: {}", e))?;

    // #363: valid-time filters (no-op unless requested).
    valid_time_retain(db, valid_at, valid_from, valid_to, &valid_op, &mut entities)?;

    // #363 review: re-apply the deferred recall side-effects to the survivors,
    // under exactly the conditions the un-filtered path would have reinforced:
    // fts5 always does; dense/hybrid only when the caller opted in.
    if temporal_filtering
        && (mode_for_side_effects == SearchMode::Fts5 || reinforce_requested)
        && !entities.is_empty()
    {
        let ids: Vec<String> = entities.iter().map(|e| e.id.clone()).collect();
        let _ = db.apply_recall_side_effects(&ids);
    }

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

    // #363: valid-time filters, before side-effects so filtered-out entities
    // are not reinforced. No-op unless a filter was requested.
    if a.valid_at.is_some() || a.valid_from_unix_ms.is_some() || a.valid_to_unix_ms.is_some() {
        let mut ents: Vec<crate::models::Entity> =
            merged.iter().map(|(e, _)| e.clone()).collect();
        valid_time_retain(
            db,
            a.valid_at,
            a.valid_from_unix_ms,
            a.valid_to_unix_ms,
            &a.valid_op,
            &mut ents,
        )?;
        let keep: std::collections::HashSet<String> = ents.into_iter().map(|e| e.id).collect();
        merged.retain(|(e, _)| keep.contains(&e.id));
    }

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

/// Serialize a TemporalVersion into the shared found=true response shape used
/// by mimir_valid_at and mimir_bitemporal (#363).
fn temporal_version_json(v: &crate::db::TemporalVersion) -> serde_json::Value {
    json!({
        "found": true,
        "id": v.entity.id,
        "category": v.entity.category,
        "key": v.entity.key,
        "body_json": v.entity.body_json,
        "status": v.entity.status,
        "entity_type": v.entity.entity_type,
        "valid_from_unix_ms": v.valid_from_unix_ms,
        "valid_to_unix_ms": v.valid_to_unix_ms,
        "recorded_at_unix_ms": v.recorded_at_unix_ms,
        "invalidated_at_unix_ms": v.invalidated_at_unix_ms,
        "is_live_version": v.invalidated_at_unix_ms.is_none(),
    })
}

/// #363: mimir_valid_at — the valid-time axis. "What was actually true in the
/// world at instant T, per current knowledge?" Orthogonal to mimir_as_of.
pub fn handle_valid_at(db: &Database, args: Value) -> Result<String, String> {
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'category' parameter".to_string())?;
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'key' parameter".to_string())?;
    let valid_at = args
        .get("valid_at_unix_ms")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "Missing 'valid_at_unix_ms' parameter (integer unix ms)".to_string())?;

    let found = db
        .valid_at(category, key, valid_at)
        .map_err(|e| format!("valid_at failed: {}", e))?;

    let result = match found {
        Some(v) => {
            let mut r = temporal_version_json(&v);
            r["valid_at_unix_ms"] = json!(valid_at);
            r
        }
        None => json!({
            "found": false,
            "category": category,
            "key": key,
            "valid_at_unix_ms": valid_at,
        }),
    };
    Ok(result.to_string())
}

/// #363: mimir_bitemporal — the full 2-axis query. "As of transaction time
/// tx_at, what did we believe was true in the world at valid time valid_at?"
pub fn handle_bitemporal(db: &Database, args: Value) -> Result<String, String> {
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'category' parameter".to_string())?;
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing 'key' parameter".to_string())?;
    let tx_at = args
        .get("tx_at_unix_ms")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "Missing 'tx_at_unix_ms' parameter (integer unix ms)".to_string())?;
    let valid_at = args
        .get("valid_at_unix_ms")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "Missing 'valid_at_unix_ms' parameter (integer unix ms)".to_string())?;

    let found = db
        .bitemporal_at(category, key, tx_at, valid_at)
        .map_err(|e| format!("bitemporal failed: {}", e))?;

    let result = match found {
        Some(v) => {
            let mut r = temporal_version_json(&v);
            r["tx_at_unix_ms"] = json!(tx_at);
            r["valid_at_unix_ms"] = json!(valid_at);
            r
        }
        None => json!({
            "found": false,
            "category": category,
            "key": key,
            "tx_at_unix_ms": tx_at,
            "valid_at_unix_ms": valid_at,
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

    // #366: recall-first is the default posture; the legacy unconditional
    // dump is an explicit opt-in.
    let mode = match a.mode.as_deref().unwrap_or("on_demand") {
        "" | "on_demand" => crate::models::ContextMode::OnDemand,
        "always_inject" | "legacy" => crate::models::ContextMode::AlwaysInject,
        other => {
            return json!({"error": format!(
                "Invalid context mode '{}': expected 'on_demand' (default) or 'always_inject'",
                other
            )})
            .to_string()
        }
    };

    let opts = crate::models::ContextOptions {
        categories: a.categories,
        limit: a.limit,
        workspace_hash: a.workspace_hash,
        query: a.query,
        mode,
        max_context_chars: a.max_context_chars,
        model: a.model,
        exclude_ids: Vec::new(),
    };

    match db.context_block(&opts) {
        Ok(block) => {
            let total_chars = block.markdown.len();
            json!({
                "markdown": block.markdown,
                "total_chars": total_chars,
                "mode": block.mode,
                "budget_chars": block.budget_chars,
                "entities_injected": block.entities_injected,
                "warnings": block.warnings,
            })
            .to_string()
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

// ─── GraphRAG community tools (#365) ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CommunitiesArgs {
    #[serde(default)]
    pub workspace_hash: String,
    /// 'label_prop' (default) or 'louvain'.
    #[serde(default)]
    pub algorithm: String,
    /// Minimum community size to keep (isolated nodes never form communities).
    #[serde(default = "default_min_community_size")]
    pub min_size: usize,
}

fn default_min_community_size() -> usize {
    2
}

/// Detect (and persist) communities over the workspace's link graph.
pub fn handle_communities(db: &Database, args: Value) -> Result<String, String> {
    let a: CommunitiesArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid communities arguments: {}", e))?;
    let report = db
        .detect_communities(&a.workspace_hash, &a.algorithm, a.min_size)
        .map_err(|e| format!("Community detection failed: {}", e))?;
    serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
}

#[derive(Debug, Deserialize)]
pub struct CommunitySummaryArgs {
    pub community_id: String,
    /// Optional LLM polish; extractive summary is always the fallback.
    #[serde(default)]
    pub use_llm: bool,
    /// Force regeneration even when a cached summary entity exists.
    #[serde(default)]
    pub refresh: bool,
}

/// Return (and materialize) the summary for one detected community.
pub fn handle_community_summary(db: &Database, args: Value) -> Result<String, String> {
    let a: CommunitySummaryArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid community_summary arguments: {}", e))?;
    let result = db
        .community_summary(&a.community_id, a.use_llm, a.refresh)
        .map_err(|e| format!("Community summary failed: {}", e))?;
    serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
}

/// GraphRAG global recall: breadth over community summaries, then depth into
/// the best communities' members.
pub fn handle_global_recall(db: &Database, args: Value) -> Result<String, String> {
    let params: crate::communities::GlobalRecallParams = serde_json::from_value(args)
        .map_err(|e| format!("Invalid global_recall arguments: {}", e))?;
    let result = db
        .global_recall(&params)
        .map_err(|e| format!("Global recall failed: {}", e))?;
    serde_json::to_string(&result).map_err(|e| format!("Serialization failed: {}", e))
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

// ─── mimir_dream handler ─────────────────────────────────────────

/// Wire args for mimir_dream: DreamParams plus the handler-level
/// `fallback_consolidate` switch (LLM-less environments can opt into the
/// mechanical consolidate pass instead of an error).
#[derive(Debug, Deserialize)]
pub struct DreamArgs {
    #[serde(flatten)]
    pub params: crate::models::DreamParams,
    /// When the LLM endpoint is not configured: instead of a clean error,
    /// fall back to the non-LLM mimir_consolidate (cold_first) over the same
    /// categories. Off by default — dreaming and mechanical merging produce
    /// different artifacts, so the substitution must be explicit.
    #[serde(default)]
    pub fallback_consolidate: bool,
}

pub fn handle_dream(db: &Database, args: Value) -> Result<String, String> {
    let a: DreamArgs =
        serde_json::from_value(args).map_err(|e| format!("Invalid dream arguments: {}", e))?;

    if !db.llm_enabled() && a.fallback_consolidate {
        // Graceful no-LLM fallback: run the mechanical consolidation pass
        // (cold_first, same archive-safety rules) per category and report it
        // AS a fallback so callers can tell nothing was LLM-reasoned.
        let categories: Vec<String> = match a.params.category {
            Some(ref c) => vec![c.clone()],
            None => db
                .workspace_list_categories()
                .map_err(|e| format!("Dream fallback (categories) failed: {}", e))?
                .into_iter()
                .filter(|c| {
                    c != "insight" && c != "observation" && c != "synthesis" && c != "memories"
                })
                .collect(),
        };
        let mut observations_created = 0i64;
        let mut sources_archived = 0i64;
        let mut entities_examined = 0i64;
        for cat in &categories {
            let report = db
                .consolidate(&crate::models::ConsolidateParams {
                    category: cat.clone(),
                    similarity_threshold: 0.6,
                    limit: a.params.max_clusters,
                    offset: 0,
                    dry_run: a.params.dry_run,
                    cold_first: true,
                    archive_sources: a.params.archive_sources,
                })
                .map_err(|e| format!("Dream fallback (consolidate {}) failed: {}", cat, e))?;
            observations_created += report.observations_created;
            sources_archived += report.sources_archived;
            entities_examined += report.entities_examined;
        }
        return Ok(json!({
            "fallback": "consolidate",
            "note": "LLM endpoint not configured — ran the non-LLM mimir_consolidate (cold_first) pass instead. Set --llm-endpoint for real dreaming.",
            "categories_scanned": categories,
            "entities_examined": entities_examined,
            "observations_created": observations_created,
            "sources_archived": sources_archived,
            "dry_run": a.params.dry_run,
        })
        .to_string());
    }

    let report = db
        .dream(&a.params)
        .map_err(|e| format!("Dream failed: {}", e))?;
    serde_json::to_string(&report).map_err(|e| format!("Serialization failed: {}", e))
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
    /// #363: when the OLD fact stopped being true in the world. Defaults to
    /// transaction time (now) — superseding a fact ends its validity.
    #[serde(default)]
    pub valid_to_unix_ms: Option<i64>,
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

    // #363 review: validate an EXPLICIT valid_to against the old fact's stored
    // period BEFORE any mutation, so a rejected close can't leave a half-done
    // supersede (link created, status flipped, period untouched).
    //   * it must not invert the period (vt <= valid_from), and
    //   * it must not EXTEND an already-closed period — a fact that ended
    //     stays ended; superseding may only tighten.
    let periods = db
        .valid_periods_for_ids(&[from_entity.id.clone()])
        .map_err(|e| format!("'From' entity valid-period lookup failed: {}", e))?;
    let (eff_from, cur_to) = periods
        .get(&from_entity.id)
        .copied()
        .unwrap_or((from_entity.created_at_unix_ms, None));
    if let Some(vt) = a.valid_to_unix_ms {
        if vt <= eff_from {
            return Err(format!(
                "valid_to_unix_ms ({vt}) must be greater than the superseded fact's valid_from ({eff_from})"
            ));
        }
        if let Some(cur) = cur_to {
            if vt > cur {
                return Err(format!(
                    "valid_to_unix_ms ({vt}) would extend the superseded fact's already-closed \
                     valid period (valid_to {cur}); superseding may only tighten it"
                ));
            }
        }
    }

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

    // 3. Close the OLD entity's valid-time period (#363): superseding a fact
    // records when it stopped being true in the world — at transaction time
    // unless the caller says when. The default close is bumped strictly past
    // valid_from so a fact superseded within its creation millisecond still
    // gets a non-inverted (if degenerate-width) period. set_valid_to itself
    // never extends an already-closed period; the effective close (possibly
    // the earlier stored one) is what gets reported.
    let requested = a
        .valid_to_unix_ms
        .unwrap_or_else(|| now_ms().max(eff_from + 1));
    let valid_to = db
        .set_valid_to(&from_entity.id, requested)
        .map_err(|e| format!("Failed to close 'from' entity's valid period: {}", e))?;

    let result = json!({
        "from_entity_id": from_entity.id,
        "from_entity_category": from_entity.category,
        "from_entity_key": from_entity.key,
        "from_valid_to_unix_ms": valid_to,
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
    /// #363: application-time period of the corrected fact (optional).
    #[serde(default)]
    pub valid_from_unix_ms: Option<i64>,
    #[serde(default)]
    pub valid_to_unix_ms: Option<i64>,
}


pub fn handle_correct(db: &Database, args: Value) -> Result<String, String> {
    let a: CorrectArgs = serde_json::from_value(args)
        .map_err(|e| format!("Invalid correct arguments: {}", e))?;

    // #363 review: same inverted-period rejection as mimir_remember — an
    // inverted period would shadow older versions in bitemporal_at while
    // never matching itself, making the fact unanswerable.
    if let (Some(vf), Some(vt)) = (a.valid_from_unix_ms, a.valid_to_unix_ms) {
        if vt <= vf {
            return Err(format!(
                "valid_to_unix_ms ({vt}) must be greater than valid_from_unix_ms ({vf})"
            ));
        }
    }

    let params = crate::models::CorrectParams {
        wrong_approach: a.wrong_approach,
        user_correction: a.user_correction,
        task_context: a.task_context,
        session_id: a.session_id,
        tags: a.tags,
        category: a.category,
        visibility: a.visibility,
        valid_from_unix_ms: a.valid_from_unix_ms,
        valid_to_unix_ms: a.valid_to_unix_ms,
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

    fn temp_db() -> (Database, String) {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mimir-test-tools-{}.db", Uuid::new_v4()));
        let path_str = path.to_str().unwrap().to_string();
        let db = Database::open(&path_str).expect("open test db");
        (db, path_str)
    }

    // ─── Bi-temporal valid-time tools (#363) ─────────────────────

    #[test]
    fn valid_at_tool_roundtrips_a_retroactive_fact() {
        let (db, path) = temp_db();
        let now = now_ms();
        let vf = now - 7 * 24 * 3600 * 1000; // true since last week

        handle_remember(
            &db,
            json!({"category": "facts", "key": "retro", "body_json": "{\"note\":\"was true last week\"}",
                   "valid_from_unix_ms": vf}),
        )
        .expect("remember with valid_from");

        // Found for instants >= valid_from…
        for t in [vf, vf + 1000, now] {
            let r = handle_valid_at(
                &db,
                json!({"category": "facts", "key": "retro", "valid_at_unix_ms": t}),
            )
            .expect("valid_at");
            let v: Value = serde_json::from_str(&r).unwrap();
            assert_eq!(v["found"], json!(true), "t={t}: {r}");
            assert_eq!(v["valid_from_unix_ms"], json!(vf));
            assert_eq!(v["is_live_version"], json!(true));
        }
        // …found=false strictly before.
        let r = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "retro", "valid_at_unix_ms": vf - 1}),
        )
        .expect("valid_at before");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(false), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remember_rejects_inverted_valid_period() {
        let (db, path) = temp_db();
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "bad", "body_json": "{}",
                   "valid_from_unix_ms": 200, "valid_to_unix_ms": 100}),
        )
        .expect_err("inverted period must be rejected");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remember_rejects_one_sided_past_valid_to() {
        // #363 review (round 2): with valid_from omitted it defaults to "now"
        // (new entity / content change) or the stored period (identical
        // re-assert) — a past valid_to would silently store an inverted
        // period that valid_at can never match while still shadowing older
        // versions in bitemporal_at.
        let (db, path) = temp_db();
        let past = now_ms() - 60_000;

        // (a) New entity: effective period would be [now, past).
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "one-sided", "body_json": "{\"note\":\"v1\"}",
                   "valid_to_unix_ms": past}),
        )
        .expect_err("one-sided past valid_to on a new entity must be rejected");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        // Nothing was written.
        assert!(
            db.get_entity("facts", "one-sided").unwrap().is_none(),
            "rejected remember must not create an entity"
        );

        // (b) Existing entity, content change: the new version's valid_from
        // defaults to now — same inversion.
        handle_remember(
            &db,
            json!({"category": "facts", "key": "one-sided", "body_json": "{\"note\":\"v1\"}"}),
        )
        .expect("baseline");
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "one-sided", "body_json": "{\"note\":\"v2\"}",
                   "valid_to_unix_ms": past}),
        )
        .expect_err("one-sided past valid_to on a content change must be rejected");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        // Rejected BEFORE mutation: v1 is still the live body.
        let body = db.get_entity("facts", "one-sided").unwrap().unwrap().body_json;
        assert!(body.contains("v1"), "rejected write must not update the entity: {body}");

        // (c) Existing entity, identical body (COALESCE re-assert path):
        // valid_to is validated against the STORED valid_from — and the
        // rejected write must leave the STORED PERIOD untouched.
        let stored_period = |db: &Database| -> (Value, Value) {
            let r = handle_valid_at(
                &db,
                json!({"category": "facts", "key": "one-sided", "valid_at_unix_ms": now_ms()}),
            )
            .expect("valid_at");
            let v: Value = serde_json::from_str(&r).unwrap();
            assert_eq!(v["found"], json!(true), "{r}");
            (v["valid_from_unix_ms"].clone(), v["valid_to_unix_ms"].clone())
        };
        let before = stored_period(&db);
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "one-sided", "body_json": "{\"note\":\"v1\"}",
                   "valid_to_unix_ms": past}),
        )
        .expect_err("one-sided past valid_to on an identical re-assert must be rejected");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        assert_eq!(
            stored_period(&db),
            before,
            "rejected re-assert must not change the stored valid period"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reassert_rejects_one_sided_valid_from_at_or_after_stored_valid_to() {
        // #363 review (round 3): the mirror-image hole. On an identical-body
        // re-assert the UPDATE takes the caller's valid_from via COALESCE
        // while KEEPING the stored valid_to — so a one-sided valid_from
        // at/after the stored close would store [vf, stored_to): inverted,
        // unanswerable at every instant.
        let (db, path) = temp_db();
        let now = now_ms();
        let vf = now - 100_000;
        let vt = now - 50_000;
        let body = "{\"note\":\"bounded\"}";

        handle_remember(
            &db,
            json!({"category": "facts", "key": "mirror", "body_json": body,
                   "valid_from_unix_ms": vf, "valid_to_unix_ms": vt}),
        )
        .expect("bounded fact");

        let stored_period = |db: &Database| -> (Value, Value) {
            let r = handle_valid_at(
                &db,
                json!({"category": "facts", "key": "mirror", "valid_at_unix_ms": vt - 1_000}),
            )
            .expect("valid_at");
            let v: Value = serde_json::from_str(&r).unwrap();
            assert_eq!(v["found"], json!(true), "{r}");
            (v["valid_from_unix_ms"].clone(), v["valid_to_unix_ms"].clone())
        };
        assert_eq!(stored_period(&db), (json!(vf), json!(vt)));

        // (a) valid_from strictly after the stored close: inverted, rejected.
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "mirror", "body_json": body,
                   "valid_from_unix_ms": vt + 10_000}),
        )
        .expect_err("one-sided valid_from after the stored valid_to must be rejected");
        assert!(err.contains("valid_from_unix_ms"), "got: {err}");

        // (b) valid_from exactly AT the stored close: empty period, rejected.
        let err = handle_remember(
            &db,
            json!({"category": "facts", "key": "mirror", "body_json": body,
                   "valid_from_unix_ms": vt}),
        )
        .expect_err("one-sided valid_from at the stored valid_to must be rejected");
        assert!(err.contains("valid_from_unix_ms"), "got: {err}");

        // Rejected writes left the stored period untouched.
        assert_eq!(
            stored_period(&db),
            (json!(vf), json!(vt)),
            "rejected re-asserts must not change the stored valid period"
        );

        // (c) Legitimate one-sided valid_from strictly BEFORE the stored
        // close: accepted, and it moves the open while keeping the close.
        let new_vf = vf - 10_000;
        handle_remember(
            &db,
            json!({"category": "facts", "key": "mirror", "body_json": body,
                   "valid_from_unix_ms": new_vf}),
        )
        .expect("one-sided valid_from before the stored valid_to must be accepted");
        assert_eq!(stored_period(&db), (json!(new_vf), json!(vt)));

        // (d) No stored valid_to (unbounded fact): any one-sided valid_from
        // yields [vf, infinity) — accepted.
        handle_remember(
            &db,
            json!({"category": "facts", "key": "unbounded", "body_json": body}),
        )
        .expect("unbounded fact");
        handle_remember(
            &db,
            json!({"category": "facts", "key": "unbounded", "body_json": body,
                   "valid_from_unix_ms": now + 60_000}),
        )
        .expect("one-sided valid_from on an unbounded fact must be accepted");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remember_accepts_one_sided_future_valid_to() {
        // A one-sided FUTURE valid_to is a real interval [now, future) — an
        // expiring fact — and must keep working.
        let (db, path) = temp_db();
        let future = now_ms() + 3_600_000;

        handle_remember(
            &db,
            json!({"category": "facts", "key": "expiring", "body_json": "{\"note\":\"v1\"}",
                   "valid_to_unix_ms": future}),
        )
        .expect("one-sided future valid_to on a new entity must be accepted");
        handle_remember(
            &db,
            json!({"category": "facts", "key": "expiring", "body_json": "{\"note\":\"v2\"}",
                   "valid_to_unix_ms": future}),
        )
        .expect("one-sided future valid_to on a content change must be accepted");

        // The stored period is answerable right now and carries the bound.
        let r = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "expiring", "valid_at_unix_ms": now_ms()}),
        )
        .expect("valid_at");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(future), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reassert_extending_closed_period_is_audited() {
        // #371: an identical-body re-assert MAY deliberately extend a period
        // that was closed via set_valid_to (intended semantics, unchanged),
        // but the change must be AUDITED — the pre-extension period is
        // snapshotted to entity_history and both periods stay reconstructable.
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();
        let now = now_ms();
        let vf = now - 100_000;
        let body = "{\"note\":\"audited extension\"}";

        handle_remember(
            &db,
            json!({"category": "facts", "key": "audited", "body_json": body,
                   "valid_from_unix_ms": vf}),
        )
        .expect("baseline fact");
        let ent = db.get_entity("facts", "audited").unwrap().unwrap();

        // Close the fact (audit-relevant precondition: stored valid_to non-NULL).
        let t2 = now - 50_000;
        assert_eq!(db.set_valid_to(&ent.id, t2).expect("close"), t2);
        let hist_before = db.history_versions("facts", "audited").unwrap().len();

        sleep(Duration::from_millis(5));
        let tx_closed = now_ms(); // transaction instant while the close was current knowledge
        sleep(Duration::from_millis(5));

        // Identical body, valid_to extending PAST the close: accepted (option
        // (b) semantics) AND snapshotted.
        let t3 = now + 50_000;
        handle_remember(
            &db,
            json!({"category": "facts", "key": "audited", "body_json": body,
                   "valid_to_unix_ms": t3}),
        )
        .expect("identical-body re-assert extending a closed period is accepted");

        // Exactly one new history snapshot, carrying the pre-extension close.
        let hist = db.history_versions("facts", "audited").unwrap();
        assert_eq!(
            hist.len(),
            hist_before + 1,
            "audited re-assert must snapshot the pre-extension version"
        );

        // Live period now reaches t3: an instant past the old close answers.
        let probe = t2 + 1_000;
        let r = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "audited", "valid_at_unix_ms": probe}),
        )
        .expect("valid_at");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(t3), "{r}");
        assert_eq!(v["is_live_version"], json!(true), "{r}");

        // Reconstruction shows BOTH periods across transaction time:
        // (a) as of tx_closed, the fact had ended at t2 — the probe instant is
        //     unanswerable and an in-period instant reports the old close…
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "audited",
                   "tx_at_unix_ms": tx_closed, "valid_at_unix_ms": probe}),
        )
        .expect("bitemporal old-knowledge cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(false), "pre-extension knowledge must keep the close: {r}");
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "audited",
                   "tx_at_unix_ms": tx_closed, "valid_at_unix_ms": t2 - 1_000}),
        )
        .expect("bitemporal old-knowledge in-period cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(t2), "{r}");
        // (b) …while current knowledge answers the probe with the extension.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "audited",
                   "tx_at_unix_ms": now_ms(), "valid_at_unix_ms": probe}),
        )
        .expect("bitemporal new-knowledge cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(t3), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reassert_moving_valid_from_on_closed_period_is_audited() {
        // #371 (review follow-up): the one-sided valid_from flavor. A closed
        // fact [t0, t5) re-asserted with ONLY valid_from = t1 (legal: t1 < t5,
        // COALESCE keeps the stored close) moves the opening — accepted, and
        // audited exactly like the valid_to extension: one snapshot preserving
        // [t0, t5), live period now [t1, t5).
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();
        let now = now_ms();
        let t0 = now - 100_000;
        let t1 = now - 80_000;
        let t5 = now - 50_000;
        let body = "{\"note\":\"opening moved\"}";

        handle_remember(
            &db,
            json!({"category": "facts", "key": "moved-open", "body_json": body,
                   "valid_from_unix_ms": t0, "valid_to_unix_ms": t5}),
        )
        .expect("closed fact [t0, t5)");
        let hist_before = db.history_versions("facts", "moved-open").unwrap().len();

        sleep(Duration::from_millis(5));
        let tx_before = now_ms(); // while [t0, t5) was current knowledge
        sleep(Duration::from_millis(5));

        handle_remember(
            &db,
            json!({"category": "facts", "key": "moved-open", "body_json": body,
                   "valid_from_unix_ms": t1}),
        )
        .expect("one-sided valid_from before the stored close is accepted");

        // Exactly one new snapshot.
        assert_eq!(
            db.history_versions("facts", "moved-open").unwrap().len(),
            hist_before + 1,
            "audited valid_from move must snapshot the pre-change version"
        );

        // Live period is now [t1, t5): an in-period instant answers live…
        let r = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "moved-open", "valid_at_unix_ms": t1 + 1_000}),
        )
        .expect("valid_at in the moved period");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_from_unix_ms"], json!(t1), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(t5), "{r}");
        assert_eq!(v["is_live_version"], json!(true), "{r}");

        // …while as-of tx_before the history row still answers with the
        // original [t0, t5) — the pre-change period is fully preserved.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "moved-open",
                   "tx_at_unix_ms": tx_before, "valid_at_unix_ms": t0 + 1_000}),
        )
        .expect("bitemporal old-knowledge cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_from_unix_ms"], json!(t0), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(t5), "{r}");
        assert_eq!(v["is_live_version"], json!(false), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reassert_with_unchanged_period_writes_no_snapshot() {
        // #371: the audit snapshot fires only when the effective period
        // actually CHANGES — an identical-body re-assert with the same stored
        // bounds, or with bounds omitted, must not write spurious history.
        let (db, path) = temp_db();
        let now = now_ms();
        let vf = now - 100_000;
        let vt = now - 50_000;
        let body = "{\"note\":\"no spurious history\"}";

        handle_remember(
            &db,
            json!({"category": "facts", "key": "quiet", "body_json": body,
                   "valid_from_unix_ms": vf, "valid_to_unix_ms": vt}),
        )
        .expect("closed fact");
        let hist_before = db.history_versions("facts", "quiet").unwrap().len();

        // (a) Bounds omitted: COALESCE keeps the stored period.
        handle_remember(
            &db,
            json!({"category": "facts", "key": "quiet", "body_json": body}),
        )
        .expect("re-assert without bounds");
        // (b) Same bounds re-sent explicitly: effective period unchanged.
        handle_remember(
            &db,
            json!({"category": "facts", "key": "quiet", "body_json": body,
                   "valid_from_unix_ms": vf, "valid_to_unix_ms": vt}),
        )
        .expect("re-assert with identical bounds");

        assert_eq!(
            db.history_versions("facts", "quiet").unwrap().len(),
            hist_before,
            "period-unchanged re-asserts must not snapshot"
        );
        // Stored period untouched.
        let r = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "quiet", "valid_at_unix_ms": vt - 1_000}),
        )
        .expect("valid_at");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_from_unix_ms"], json!(vf), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(vt), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_valid_to_close_is_audited_and_noop_is_not() {
        // #373: set_valid_to previously wrote no entity_history snapshot, so a
        // close was invisible to transaction-time reconstruction — queries at
        // a tx instant BEFORE the close reported the close anyway. An
        // effective close must snapshot the pre-close (open) version; a no-op
        // (stored close kept) must not.
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();

        handle_remember(
            &db,
            json!({"category": "facts", "key": "svt", "body_json": "{\"note\":\"open fact\"}"}),
        )
        .expect("open fact");
        let ent = db.get_entity("facts", "svt").unwrap().unwrap();
        assert!(db.history_versions("facts", "svt").unwrap().is_empty());

        sleep(Duration::from_millis(5));
        let tx_open = now_ms(); // while the fact was still believed open
        sleep(Duration::from_millis(5));

        let closed = db.set_valid_to(&ent.id, now_ms()).expect("close");
        assert_eq!(
            db.history_versions("facts", "svt").unwrap().len(),
            1,
            "an effective close must write exactly one snapshot"
        );

        // As of tx_open the fact reconstructs OPEN (the pre-close snapshot
        // answers, valid_to unbounded)…
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "svt",
                   "tx_at_unix_ms": tx_open, "valid_at_unix_ms": closed + 60_000}),
        )
        .expect("bitemporal pre-close knowledge");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "pre-close knowledge must not show the close: {r}");
        assert_eq!(v["valid_to_unix_ms"], Value::Null, "{r}");
        assert_eq!(v["is_live_version"], json!(false), "{r}");
        // …while current knowledge shows the close: same instant unanswerable,
        // an in-period instant reports valid_to = closed.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "svt",
                   "tx_at_unix_ms": now_ms(), "valid_at_unix_ms": closed + 60_000}),
        )
        .expect("bitemporal post-close knowledge");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(false), "{r}");
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "svt",
                   "tx_at_unix_ms": now_ms(), "valid_at_unix_ms": closed - 1}),
        )
        .expect("bitemporal in-period cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], json!(closed), "{r}");

        // No-op calls (same value, or a later one — the earlier close is
        // kept) write NO snapshot.
        assert_eq!(db.set_valid_to(&ent.id, closed).expect("same-value no-op"), closed);
        assert_eq!(
            db.set_valid_to(&ent.id, closed + 10_000).expect("would-extend no-op"),
            closed
        );
        assert_eq!(
            db.history_versions("facts", "svt").unwrap().len(),
            1,
            "no-op set_valid_to must not snapshot"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn supersede_close_inherits_the_audit_snapshot() {
        // #373: mimir_supersede funnels through set_valid_to, so closing the
        // old fact's period now snapshots it — the pre-supersede open version
        // stays reconstructable at earlier transaction instants.
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();

        handle_remember(
            &db,
            json!({"category": "facts", "key": "sup-old", "body_json": "{\"note\":\"old\"}"}),
        )
        .expect("old");
        handle_remember(
            &db,
            json!({"category": "facts", "key": "sup-new", "body_json": "{\"note\":\"new\"}"}),
        )
        .expect("new");
        assert!(db.history_versions("facts", "sup-old").unwrap().is_empty());

        sleep(Duration::from_millis(5));
        let tx_open = now_ms();
        sleep(Duration::from_millis(5));

        let r = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "sup-old",
                   "to_category": "facts", "to_key": "sup-new"}),
        )
        .expect("supersede");
        let v: Value = serde_json::from_str(&r).unwrap();
        let closed_at = v["from_valid_to_unix_ms"].as_i64().expect("close instant");

        assert_eq!(
            db.history_versions("facts", "sup-old").unwrap().len(),
            1,
            "supersede's close must inherit the audit snapshot"
        );
        // Pre-supersede knowledge still believes the old fact open at an
        // instant the close later excluded.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "sup-old",
                   "tx_at_unix_ms": tx_open, "valid_at_unix_ms": closed_at + 60_000}),
        )
        .expect("bitemporal pre-supersede knowledge");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert_eq!(v["valid_to_unix_ms"], Value::Null, "{r}");
        // Current knowledge: closed.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "sup-old",
                   "tx_at_unix_ms": now_ms(), "valid_at_unix_ms": closed_at + 60_000}),
        )
        .expect("bitemporal post-supersede knowledge");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(false), "{r}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bitemporal_tool_reports_both_axes() {
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();

        handle_remember(
            &db,
            json!({"category": "facts", "key": "two-axis", "body_json": "{\"note\":\"v1\"}"}),
        )
        .expect("v1");
        sleep(Duration::from_millis(5));
        let tx_mid = now_ms();
        sleep(Duration::from_millis(5));
        let vf2 = now_ms() - 60_000; // retroactive
        handle_remember(
            &db,
            json!({"category": "facts", "key": "two-axis", "body_json": "{\"note\":\"v2\"}",
                   "valid_from_unix_ms": vf2}),
        )
        .expect("v2");

        // At tx_mid we believed v1 — even for a world-instant v2 now covers.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "two-axis",
                   "tx_at_unix_ms": tx_mid, "valid_at_unix_ms": tx_mid}),
        )
        .expect("bitemporal old-knowledge cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert!(v["body_json"].as_str().unwrap().contains("v1"), "{r}");

        // With current knowledge the same world-instant belongs to v2.
        let r = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "two-axis",
                   "tx_at_unix_ms": now_ms() + 60_000, "valid_at_unix_ms": tx_mid}),
        )
        .expect("bitemporal new-knowledge cell");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["found"], json!(true), "{r}");
        assert!(v["body_json"].as_str().unwrap().contains("v2"), "{r}");

        // Missing parameter errors are named.
        let err = handle_bitemporal(
            &db,
            json!({"category": "facts", "key": "two-axis", "valid_at_unix_ms": 1}),
        )
        .expect_err("missing tx_at must error");
        assert!(err.contains("tx_at_unix_ms"), "got: {err}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recall_valid_time_filters_narrow_results() {
        let (db, path) = temp_db();
        let now = now_ms();

        // Fact A: valid only during a past window (ended).
        handle_remember(
            &db,
            json!({"category": "ops", "key": "window-a", "body_json": "{\"note\":\"ceasefire window alpha\"}",
                   "valid_from_unix_ms": now - 100_000, "valid_to_unix_ms": now - 50_000}),
        )
        .expect("A");
        // Fact B: valid from now, unbounded.
        handle_remember(
            &db,
            json!({"category": "ops", "key": "window-b", "body_json": "{\"note\":\"ceasefire window bravo\"}"}),
        )
        .expect("B");

        let keys = |resp: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(resp).unwrap();
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["key"].as_str().unwrap().to_string())
                .collect()
        };

        // No filter: both.
        let all = handle_recall(&db, json!({"query": "ceasefire", "mode": "fts5"})).unwrap();
        assert_eq!(keys(&all).len(), 2, "{all}");

        // valid_at inside A's window: only A.
        let past = handle_recall(
            &db,
            json!({"query": "ceasefire", "mode": "fts5", "valid_at": now - 75_000}),
        )
        .unwrap();
        assert_eq!(keys(&past), vec!["window-a".to_string()], "{past}");

        // valid_at after both writes: only B (A ended). +10s guards against
        // B's creation landing a few ms after the captured `now`.
        let current = handle_recall(
            &db,
            json!({"query": "ceasefire", "mode": "fts5", "valid_at": now + 10_000}),
        )
        .unwrap();
        assert_eq!(keys(&current), vec!["window-b".to_string()], "{current}");

        // Period OVERLAPS spanning A's window and beyond: both.
        let overlap = handle_recall(
            &db,
            json!({"query": "ceasefire", "mode": "fts5",
                   "valid_from_unix_ms": now - 80_000, "valid_to_unix_ms": now + 80_000,
                   "valid_op": "overlaps"}),
        )
        .unwrap();
        assert_eq!(keys(&overlap).len(), 2, "{overlap}");

        // Period CONTAINS a slice strictly inside A's window: only A… and only
        // if A's period contains the whole queried slice.
        let contains = handle_recall(
            &db,
            json!({"query": "ceasefire", "mode": "fts5",
                   "valid_from_unix_ms": now - 90_000, "valid_to_unix_ms": now - 60_000,
                   "valid_op": "contains"}),
        )
        .unwrap();
        assert_eq!(keys(&contains), vec!["window-a".to_string()], "{contains}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn supersede_closes_the_old_facts_valid_period() {
        let (db, path) = temp_db();
        handle_remember(
            &db,
            json!({"category": "facts", "key": "old-roe", "body_json": "{\"note\":\"roe v1\"}"}),
        )
        .expect("old");
        handle_remember(
            &db,
            json!({"category": "facts", "key": "new-roe", "body_json": "{\"note\":\"roe v2\"}"}),
        )
        .expect("new");

        // Ensure the close instant lands strictly after the fact's valid_from
        // (now_ms has 1ms resolution).
        std::thread::sleep(std::time::Duration::from_millis(5));
        let r = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "old-roe",
                   "to_category": "facts", "to_key": "new-roe"}),
        )
        .expect("supersede");
        let v: Value = serde_json::from_str(&r).unwrap();
        let closed_at = v["from_valid_to_unix_ms"].as_i64().expect("close instant reported");

        // The old fact is no longer "true in the world" from the close on.
        let after = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "old-roe", "valid_at_unix_ms": closed_at}),
        )
        .unwrap();
        let av: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(av["found"], json!(false), "{after}");
        // But it WAS true just before.
        let before = handle_valid_at(
            &db,
            json!({"category": "facts", "key": "old-roe", "valid_at_unix_ms": closed_at - 1}),
        )
        .unwrap();
        let bv: Value = serde_json::from_str(&before).unwrap();
        assert_eq!(bv["found"], json!(true), "{before}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn temporal_filtered_recall_does_not_reinforce_hidden_entities() {
        // #363 review, #356-class value inversion: a recall with a valid-time
        // filter must NOT apply retrieval side-effects to entities the filter
        // hides — otherwise repeated "what was true at T" queries reinforce
        // (and eventually make immortal) entities that are never returned.
        let (db, path) = temp_db();
        let now = now_ms();

        // A: valid only in a past window (always filtered out below).
        handle_remember(
            &db,
            json!({"category": "ops", "key": "hidden-a", "body_json": "{\"note\":\"embargo period alpha\"}",
                   "valid_from_unix_ms": now - 100_000, "valid_to_unix_ms": now - 50_000}),
        )
        .expect("A");
        // B: currently valid (always survives).
        handle_remember(
            &db,
            json!({"category": "ops", "key": "visible-b", "body_json": "{\"note\":\"embargo period bravo\"}"}),
        )
        .expect("B");

        let count_of = |key: &str| -> i64 {
            db.get_entity("ops", key).unwrap().unwrap().retrieval_count
        };
        assert_eq!(count_of("hidden-a"), 0);
        assert_eq!(count_of("visible-b"), 0);

        // Three temporal-filtered recalls: only B survives each time.
        for _ in 0..3 {
            let r = handle_recall(
                &db,
                json!({"query": "embargo", "mode": "fts5", "valid_at": now + 10_000}),
            )
            .expect("filtered recall");
            let v: Value = serde_json::from_str(&r).unwrap();
            assert_eq!(v["total"], json!(1), "{r}");
        }

        assert_eq!(
            count_of("hidden-a"),
            0,
            "filtered-out entity must NOT be reinforced by temporal recalls"
        );
        assert_eq!(
            count_of("visible-b"),
            3,
            "surviving entity must still be reinforced once per recall"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unfiltered_recall_side_effects_and_output_are_unchanged() {
        // #363 review: the pure-read deferral only engages when a valid-time
        // filter is present. An unfiltered recall must keep the original
        // behavior — side-effects applied to every hit — and an always-true
        // filter must return the identical item set.
        let (db, path) = temp_db();
        handle_remember(
            &db,
            json!({"category": "ops", "key": "u1", "body_json": "{\"note\":\"quorum call one\"}"}),
        )
        .expect("u1");
        handle_remember(
            &db,
            json!({"category": "ops", "key": "u2", "body_json": "{\"note\":\"quorum call two\"}"}),
        )
        .expect("u2");

        let unfiltered =
            handle_recall(&db, json!({"query": "quorum", "mode": "fts5"})).expect("recall");
        let uv: Value = serde_json::from_str(&unfiltered).unwrap();
        assert_eq!(uv["total"], json!(2), "{unfiltered}");
        for key in ["u1", "u2"] {
            assert_eq!(
                db.get_entity("ops", key).unwrap().unwrap().retrieval_count,
                1,
                "unfiltered recall must still reinforce every hit ({key})"
            );
        }

        // An always-true valid filter returns the same keys.
        let filtered = handle_recall(
            &db,
            json!({"query": "quorum", "mode": "fts5", "valid_at": now_ms() + 60_000}),
        )
        .expect("filtered recall");
        let fv: Value = serde_json::from_str(&filtered).unwrap();
        // Compare as SETS (sorted), not ordered lists: the first (unfiltered)
        // recall reinforces both hits, which legitimately changes the ranking
        // inputs (retrieval_count, last_accessed) before the second call, and
        // the final `id ASC` tie-break is over random UUIDs — so cross-call
        // ORDER is not a stable property. Membership is.
        let keys = |v: &Value| -> Vec<String> {
            let mut k: Vec<String> = v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["key"].as_str().unwrap().to_string())
                .collect();
            k.sort();
            k
        };
        assert_eq!(keys(&uv), keys(&fv), "always-true filter must not change the result set");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recall_rejects_unknown_valid_op() {
        let (db, path) = temp_db();
        let err = handle_recall(
            &db,
            json!({"query": "x", "valid_from_unix_ms": 1, "valid_op": "during"}),
        )
        .expect_err("unknown valid_op must be rejected");
        assert!(err.contains("valid_op") && err.contains("during"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn correct_rejects_inverted_valid_period() {
        let (db, path) = temp_db();
        let err = handle_correct(
            &db,
            json!({"wrong_approach": "w", "user_correction": "c", "task_context": "t",
                   "valid_from_unix_ms": 200, "valid_to_unix_ms": 100}),
        )
        .expect_err("inverted period must be rejected on correct");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        // Nothing was written.
        let r = handle_recall(&db, json!({"query": "", "category": "correction", "mode": "fts5"}))
            .unwrap_or_else(|_| "{\"total\":0}".to_string());
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["total"], json!(0), "rejected correct must not create an entity: {r}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn correct_rejects_one_sided_past_valid_to() {
        // #363 review (round 2): same one-sided guard on the correct surface —
        // valid_from omitted defaults to now, so a past valid_to inverts.
        let (db, path) = temp_db();
        let err = handle_correct(
            &db,
            json!({"wrong_approach": "w", "user_correction": "c", "task_context": "t",
                   "valid_to_unix_ms": now_ms() - 60_000}),
        )
        .expect_err("one-sided past valid_to must be rejected on correct");
        assert!(err.contains("valid_to_unix_ms"), "got: {err}");
        // Nothing was written.
        let r = handle_recall(&db, json!({"query": "", "category": "correction", "mode": "fts5"}))
            .unwrap_or_else(|_| "{\"total\":0}".to_string());
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["total"], json!(0), "rejected correct must not create an entity: {r}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn supersede_valid_to_cannot_invert_or_extend_a_closed_period() {
        // #363 review: an explicit valid_to on supersede must be validated
        // against the old fact's stored period BEFORE any mutation, and a
        // default-now supersede of an already-ended fact must keep the
        // earlier close (never retroactively extend validity).
        let (db, path) = temp_db();
        let now = now_ms();
        let vf = now - 100_000;
        let vt = now - 50_000;
        handle_remember(
            &db,
            json!({"category": "facts", "key": "ended", "body_json": "{\"note\":\"old bounded\"}",
                   "valid_from_unix_ms": vf, "valid_to_unix_ms": vt}),
        )
        .expect("bounded old fact");
        handle_remember(
            &db,
            json!({"category": "facts", "key": "successor", "body_json": "{\"note\":\"new\"}"}),
        )
        .expect("successor");

        // (a) Inverted: valid_to at/before the old fact's valid_from.
        let err = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "ended",
                   "to_category": "facts", "to_key": "successor",
                   "valid_to_unix_ms": vf - 1}),
        )
        .expect_err("inverting valid_to must be rejected");
        assert!(err.contains("valid_from"), "got: {err}");
        // Rejected BEFORE mutation: the old fact is not deprecated.
        let status = db.get_entity("facts", "ended").unwrap().unwrap().status;
        assert_eq!(status, "active", "rejected supersede must not mutate status");

        // (b) Extension: valid_to after the already-stored close.
        let err = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "ended",
                   "to_category": "facts", "to_key": "successor",
                   "valid_to_unix_ms": now}),
        )
        .expect_err("extending a closed period must be rejected");
        assert!(err.contains("tighten"), "got: {err}");

        // (c) Default-now supersede of the already-ended fact: succeeds and
        // KEEPS the earlier close instead of extending it.
        let r = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "ended",
                   "to_category": "facts", "to_key": "successor"}),
        )
        .expect("default supersede");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(
            v["from_valid_to_unix_ms"],
            json!(vt),
            "an ended fact must keep its earlier close: {r}"
        );

        // (d) Tightening to an earlier close is allowed.
        let r = handle_supersede(
            &db,
            json!({"from_category": "facts", "from_key": "ended",
                   "to_category": "facts", "to_key": "successor",
                   "valid_to_unix_ms": vt - 10_000}),
        )
        .expect("tightening supersede");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["from_valid_to_unix_ms"], json!(vt - 10_000), "{r}");

        let _ = std::fs::remove_file(&path);
    }

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

    #[test]
    fn context_args_accept_null_for_new_optional_fields() {
        // #356/#366 args must follow the same explicit-null tolerance rule
        // as the rest of the tool surface (#330).
        for field in ["query", "mode", "model", "max_context_chars", "workspace_hash", "categories"] {
            let mut v = json!({});
            v.as_object_mut()
                .unwrap()
                .insert(field.to_string(), Value::Null);
            let result: Result<ContextArgs, _> = serde_json::from_value(v);
            assert!(
                result.is_ok(),
                "field `{}` with explicit null should deserialize, got {:?}",
                field,
                result.err()
            );
        }
    }

    fn temp_tool_db() -> (Database, String) {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mimir-tools-test-{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap().to_string();
        let db = Database::open(&path_str).expect("open test db");
        (db, path_str)
    }

    #[test]
    fn handle_context_defaults_to_recall_first_on_demand() {
        let (db, path) = temp_tool_db();
        let out = handle_context(&db, json!({}));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["mode"], "on_demand", "recall-first must be the default: {out}");
        assert_eq!(v["budget_chars"], 1500);
        assert!(
            v["markdown"].as_str().unwrap().contains("Recall-first mode"),
            "no-query default output must be the retrieval pointer: {out}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn handle_context_rejects_unknown_mode() {
        let (db, path) = temp_tool_db();
        let out = handle_context(&db, json!({"mode": "firehose"}));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(
            v["error"].as_str().unwrap().contains("Invalid context mode"),
            "unknown mode must be rejected: {out}"
        );
        // The legacy opt-in spelling still parses.
        let legacy = handle_context(&db, json!({"mode": "always_inject"}));
        let lv: Value = serde_json::from_str(&legacy).unwrap();
        assert_eq!(lv["mode"], "always_inject");
        assert_eq!(lv["budget_chars"], 0);
        let _ = std::fs::remove_file(&path);
    }
}



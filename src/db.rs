use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::connectors::Connector;
use crate::encryption::EncryptionManager;
use crate::models::{
    AskParams, AskResult, AskSource, CompactReport, DecayReport, EmbedParams, Entity, GraphEdge,
    GraphNode, IngestParams, JournalEvent, MemoryLink, PruneParams, PruneReport, RecallParams,
    StateEntry, Stats, TimelineParams, VaultReport,
};
use crate::schema;

/// Format a unix timestamp in milliseconds as an ISO 8601 UTC string.
fn chrono_like(ms: i64) -> String {
    crate::util::format_iso8601(ms / 1000)
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub struct Database {
    conn: Connection,
    db_path: String,
    encryption: Option<EncryptionManager>,
    llm_config: LlmConfig,
    #[allow(dead_code)]
    embedding_config: crate::embedding::EmbeddingConfig,
    connectors: Vec<Box<dyn Connector>>,
}

/// Configuration for the LLM integration (Ollama or OpenAI-compatible API).
#[derive(Clone)]
pub struct LlmConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub timeout_secs: u64,
    pub api_key: Option<String>,
    /// Separate embedding endpoint (defaults to Ollama /api/embed derived from endpoint).
    /// Supports OpenAI-compatible /v1/embeddings format.
    pub embedding_endpoint: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:11434/api/generate".to_string(),
            model: "llama3".to_string(),
            timeout_secs: 30,
            api_key: None,
            embedding_endpoint: None,
        }
    }
}

impl Database {
    /// Open a database at `path`, initializing the v0.2.0 schema if needed.
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;

        // Enable WAL for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=1000; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;

        // Initialize schema if this is a new database
        schema::initialize_schema(&conn)?;

        Ok(Database {
            conn,
            db_path: path.to_string(),
            encryption: None,
            llm_config: LlmConfig::default(),
            embedding_config: crate::embedding::EmbeddingConfig::default(),
            connectors: Vec::new(),
        })
    }

    /// Simple health check — verify the DB responds.
    pub fn health_check(&self) -> bool {
        self.conn.query_row("SELECT 1", [], |_| Ok(())).is_ok()
    }

    /// Enable encryption by loading the AES-256-GCM key from `key_file`.
    /// Returns an error if the key file cannot be read or is invalid.
    pub fn set_encryption(&mut self, key_file: &str) -> Result<(), String> {
        let mgr = EncryptionManager::from_key_file(key_file)?;
        self.encryption = Some(mgr);
        Ok(())
    }

    /// Returns true if encryption is enabled.
    #[allow(dead_code)]
    #[allow(dead_code)]
    pub fn encryption_enabled(&self) -> bool {
        self.encryption.is_some()
    }

    /// Replace the connector list (used at startup to load configured connectors).
    pub fn set_connectors(&mut self, connectors: Vec<Box<dyn Connector>>) {
        self.connectors = connectors;
    }

    /// Configure LLM integration for the mimir_ask tool.
    pub fn set_llm(
        &mut self,
        enabled: bool,
        endpoint: &str,
        model: &str,
        api_key: Option<&str>,
        embedding_endpoint: Option<&str>,
    ) {
        self.llm_config = LlmConfig {
            enabled,
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            timeout_secs: 30,
            api_key: api_key.map(|s| s.to_string()),
            embedding_endpoint: embedding_endpoint.map(|s| s.to_string()),
        };
    }

    /// Returns true if LLM integration is enabled.
    pub fn llm_enabled(&self) -> bool {
        self.llm_config.enabled
    }

    /// RAG: recall relevant entities, assemble context, ask Ollama for a grounded answer.
    pub fn ask(&self, params: &AskParams) -> Result<AskResult, Box<dyn std::error::Error>> {
        if !self.llm_config.enabled {
            return Err("LLM is not enabled. Set --llm-endpoint to enable mimir_ask.".into());
        }

        // Step 1: Recall top-k relevant entities
        let recall_params = RecallParams {
            query: params.query.clone(),
            limit: params.top_k as i64,
            skip_side_effects: true,
            ..Default::default()
        };
        let entities = self.recall(&recall_params)?;

        if entities.is_empty() {
            return Err("No matching memories found for this question.".into());
        }

        // Step 2: Assemble context (truncate bodies to ~600 chars each)
        let mut context_parts = Vec::new();
        let mut sources = Vec::new();
        for e in &entities {
            let snippet: String = e.body_json.chars().take(600).collect();
            context_parts.push(format!("[key: {}] {}", e.key, snippet));
            sources.push(AskSource {
                key: e.key.clone(),
                category: e.category.clone(),
                score: e.decay_score,
                snippet,
            });
        }
        let context = context_parts.join("\n\n");

        // Step 3: Build prompt
        let prompt = format!(
            "Answer the question based ONLY on the following context. Cite sources by their key.\n\nContext:\n{}\n\nQuestion: {}\n\nAnswer:",
            context, params.query
        );

        // Step 4: Call Ollama
        let body = serde_json::json!({
            "model": self.llm_config.model,
            "prompt": prompt,
            "stream": false,
        });

        let body_str = serde_json::to_string(&body)?;
        let mut request = ureq::post(&self.llm_config.endpoint)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.llm_config.timeout_secs));
        if let Some(ref key) = self.llm_config.api_key {
            request = request.set("Authorization", &format!("Bearer {}", key));
        }
        let response = request
            .send_string(&body_str)
            .map_err(|e| format!("LLM API call failed: {}", e))?;

        let response_body = response
            .into_string()
            .map_err(|e| format!("Failed to read Ollama response: {}", e))?;
        let json: serde_json::Value = serde_json::from_str(&response_body)
            .map_err(|e| format!("Failed to parse Ollama response: {}", e))?;

        let answer = json["response"]
            .as_str()
            .unwrap_or("(no response from model)")
            .to_string();

        Ok(AskResult { answer, sources })
    }

    /// Run connector ingestion: fetch documents from external sources and store as entities.
    pub fn ingest(
        &self,
        params: &IngestParams,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        if self.connectors.is_empty() {
            return Err("No connectors configured. Add connectors to enable ingestion.".into());
        }

        let mut ingested = 0u64;
        let mut errors = Vec::new();
        let now = crate::db::now_ms();

        for i in 0..self.connectors.len() {
            let name = self.connectors[i].name().to_string();

            if let Some(ref requested) = params.connector {
                if name != *requested {
                    continue;
                }
            }

            let fetch_result = self.connectors[i].fetch();
            match fetch_result {
                Ok(docs) => {
                    if params.dry_run {
                        ingested += docs.len() as u64;
                        continue;
                    }
                    for doc in docs {
                        let entity = Entity {
                            id: {
                                let raw = uuid::Uuid::new_v4().to_string().replace('-', "");
                                format!("ingest-{}", &raw[..12.min(raw.len())])
                            },
                            category: doc.category,
                            key: doc.key,
                            body_json: doc.body_json,
                            status: "active".to_string(),
                            entity_type: "insight".to_string(),
                            tags: doc.tags,
                            decay_score: 1.0,
                            retrieval_count: 0,
                            always_on: false,
                            certainty: 0.5,
                            layer: "buffer".to_string(),
                            topic_path: String::new(),
                            archived: false,
                            archive_reason: String::new(),
                            links: vec![],
                            verified: false,
                            source: format!("connector:{}", name),
                            created_at_unix_ms: now,
                            last_accessed_unix_ms: now,
                            embedding: None,
                        };
                        match self.remember(&entity) {
                            Ok(_) => ingested += 1,
                            Err(e) => errors.push(format!("{}/{}: {}", name, entity.key, e)),
                        }
                    }
                    self.connectors[i]
                        .last_sync()
                        .store(now, std::sync::atomic::Ordering::SeqCst);
                }
                Err(e) => errors.push(format!("{}: {}", name, e)),
            }
        }

        let result = serde_json::json!({
            "ingested": ingested,
            "dry_run": params.dry_run,
            "errors": errors,
        });
        Ok(result)
    }

    /// Store a dense vector embedding for an entity.
    #[allow(dead_code)]
    pub fn store_embedding(
        &self,
        id: &str,
        embedding: &[f32],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.conn.execute(
            "UPDATE entities SET embedding = ?1 WHERE id = ?2",
            params![blob, id],
        )?;
        Ok(())
    }

    /// Generate and store embeddings for entities via Ollama /api/embed.
    pub fn embed_entity(
        &self,
        params: &EmbedParams,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        // Batch mode: embed all entities in a category that lack embeddings
        if let Some(ref cat) = params.batch_category {
            let mut stmt = self.conn.prepare(
                "SELECT id, body_json FROM entities WHERE category = ?1 AND archived = 0 AND embedding IS NULL LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![cat, params.batch_limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;

            let mut embedded = 0usize;
            let mut errors = Vec::new();
            for row in rows {
                let (id, body) = row?;
                match self.call_ollama_embed(&body) {
                    Ok(vec) => {
                        self.store_embedding(&id, &vec)?;
                        embedded += 1;
                    }
                    Err(e) => errors.push(format!("{}: {}", id, e)),
                }
            }
            return Ok(serde_json::json!({
                "embedded": embedded,
                "batch_category": cat,
                "errors": errors,
            }));
        }

        // Single entity mode: require category + key
        let category = params.category.as_ref().ok_or("category is required")?;
        let key = params.key.as_ref().ok_or("key is required")?;
        let entity = self
            .get_entity(category, key)?
            .ok_or_else(|| format!("entity not found: {}/{}", category, key))?;

        let text = params.text.as_ref().unwrap_or(&entity.body_json);
        let embedding = self.call_ollama_embed(text)?;
        self.store_embedding(&entity.id, &embedding)?;

        Ok(serde_json::json!({
            "embedded": 1,
            "id": entity.id,
            "dimensions": embedding.len(),
        }))
    }

    /// Call embed endpoint to get a dense vector for a text.
    /// Supports both Ollama /api/embed and OpenAI-compatible /v1/embeddings.
    fn call_ollama_embed(&self, text: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        if !self.llm_config.enabled {
            return Err("LLM is not enabled. Set --llm-endpoint to use embedding.".into());
        }
        // Determine embedding endpoint: explicit --embedding-endpoint wins,
        // otherwise derive from the LLM endpoint by swapping /api/generate → /api/embed.
        let endpoint = self
            .llm_config
            .embedding_endpoint
            .as_deref()
            .unwrap_or({
                // Default: replace Ollama generate endpoint with embed
                self.llm_config.endpoint.as_str()
            });
        let effective_endpoint = if self.llm_config.embedding_endpoint.is_some() {
            // Explicit endpoint: use as-is
            endpoint.to_string()
        } else {
            // Derive: swap /api/generate for /api/embed (Ollama convention)
            endpoint.replace("/api/generate", "/api/embed")
        };

        // Detect OpenAI-compatible format: endpoint contains /v1/
        let is_openai = effective_endpoint.contains("/v1/");

        let body = serde_json::json!({
            "model": self.llm_config.model,
            "input": text,
        });
        let body_str = serde_json::to_string(&body)?;

        let mut request = ureq::post(&effective_endpoint)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.llm_config.timeout_secs));
        if let Some(ref key) = self.llm_config.api_key {
            request = request.set("Authorization", &format!("Bearer {}", key));
        }
        let response = request
            .send_string(&body_str)
            .map_err(|e| format!("Embed API call failed at {}: {}", effective_endpoint, e))?;
        let response_body = response
            .into_string()
            .map_err(|e| format!("Failed to read embed response: {}", e))?;
        let json: serde_json::Value = serde_json::from_str(&response_body)
            .map_err(|e| format!("Invalid embed response: {}", e))?;

        if is_openai {
            // OpenAI format: {"data": [{"embedding": [0.1, 0.2, ...]}], ...}
            let vec: Vec<f32> = json["data"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v["embedding"].as_array())
                .ok_or("No embeddings in OpenAI response")?
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            if vec.is_empty() {
                return Err("OpenAI returned empty embedding vector".into());
            }
            Ok(vec)
        } else {
            // Ollama format: {"embeddings": [[...]]}
            let embeddings = json["embeddings"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_array())
                .ok_or("No embeddings in Ollama response")?;

            let vec: Vec<f32> = embeddings
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            Ok(vec)
        }
    }

    /// Bulk archive entities matching criteria (category, decay threshold, age).
    pub fn prune(&self, params: &PruneParams) -> Result<PruneReport, Box<dyn std::error::Error>> {
        let mut conditions = vec!["archived = 0".to_string()];
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref cat) = params.category {
            conditions.push(format!("category = ?{}", param_values.len() + 1));
            param_values.push(Box::new(cat.clone()));
        }
        if let Some(min_d) = params.min_decay {
            conditions.push(format!("decay_score < ?{}", param_values.len() + 1));
            param_values.push(Box::new(min_d));
        }
        if let Some(days) = params.older_than_days {
            let cutoff = crate::db::now_ms() - (days as i64 * 86400 * 1000);
            conditions.push(format!("created_at_unix_ms < ?{}", param_values.len() + 1));
            param_values.push(Box::new(cutoff));
        }

        let reason = format!(
            "prune: cat={:?} decay<{:?} age>{:?}d",
            params.category, params.min_decay, params.older_than_days
        );

        // Count matching
        let count_sql = format!(
            "SELECT COUNT(*) FROM entities WHERE {}",
            conditions.join(" AND ")
        );
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let examined: usize = self
            .conn
            .query_row(&count_sql, param_refs.as_slice(), |r| r.get::<_, i64>(0))?
            as usize;

        if params.dry_run {
            return Ok(PruneReport {
                archived: 0,
                examined,
                dry_run: true,
                reason,
            });
        }

        let limit = if params.limit == 0 {
            String::new()
        } else {
            format!(" LIMIT {}", params.limit)
        };

        // Collect the exact rowids we are about to archive *before* mutating, so the
        // FTS cleanup targets this batch precisely (not every row that happens to share
        // an archive_reason string). The condition placeholders are 1-indexed against
        // `param_values`; we reuse the same bindings for the select and the update.
        let select_rowids_sql = format!(
            "SELECT rowid FROM entities WHERE {}{}",
            conditions.join(" AND "),
            limit
        );
        let select_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let rowids: Vec<i64> = {
            let mut stmt = self.conn.prepare(&select_rowids_sql)?;
            let rows = stmt.query_map(select_refs.as_slice(), |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<i64>, _>>()?
        };

        if rowids.is_empty() {
            return Ok(PruneReport {
                archived: 0,
                examined,
                dry_run: false,
                reason,
            });
        }

        // Wrap the entity UPDATE and the FTS5 DELETE in a single transaction so the
        // index can never drift out of sync if one statement fails (matches forget()).
        let tx = self.conn.unchecked_transaction()?;
        let placeholders = rowids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let mut update_params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(reason.clone())];
        for id in &rowids {
            update_params.push(Box::new(*id));
        }
        let update_refs: Vec<&dyn rusqlite::types::ToSql> =
            update_params.iter().map(|p| p.as_ref()).collect();
        let update_sql = format!(
            "UPDATE entities SET archived = 1, archive_reason = ?1 WHERE rowid IN ({})",
            placeholders
        );
        let archived = tx.execute(&update_sql, update_refs.as_slice())?;

        let rowid_refs: Vec<&dyn rusqlite::types::ToSql> =
            rowids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
        let delete_sql = format!(
            "DELETE FROM entities_fts WHERE rowid IN ({})",
            placeholders
        );
        tx.execute(&delete_sql, rowid_refs.as_slice())?;
        tx.commit()?;

        Ok(PruneReport {
            archived,
            examined,
            dry_run: false,
            reason,
        })
    }

    /// Rebuild the FTS5 index from the `entities` table.
    ///
    /// Recovers from index drift — e.g. after a direct SQLite write, an interrupted
    /// archive, or a legacy database written before the atomic prune/forget fixes.
    /// Clears `entities_fts` and repopulates it from every non-archived entity, so
    /// archived rows stop surfacing in recall/search. Returns the number of rows
    /// indexed.
    pub fn reindex_fts(&self) -> Result<usize, Box<dyn std::error::Error>> {
        let tx = self.conn.unchecked_transaction()?;
        // Drop everything currently in the FTS index.
        tx.execute("DELETE FROM entities_fts", [])?;
        // Repopulate from live (non-archived) entities only.
        let indexed = tx.execute(
            "INSERT INTO entities_fts (rowid, body_json)
             SELECT rowid, body_json FROM entities WHERE archived = 0",
            [],
        )?;
        tx.commit()?;
        Ok(indexed)
    }

    /// Dense vector search: brute-force cosine similarity over all entities with embeddings.
    /// Returns top-k entities sorted by similarity (highest first).
    pub fn dense_search(
        &self,
        query_vec: &[f32],
        limit: usize,
    ) -> Result<Vec<(Entity, f64)>, Box<dyn std::error::Error>> {
        let max_scan = 50_000; // safety ceiling — databases beyond this should use HNSW
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, embedding
             FROM entities WHERE archived = 0 AND embedding IS NOT NULL LIMIT {}",
            max_scan
        ))?;

        let enc = self.encryption.as_ref();
        let rows = stmt.query_map([], |row| {
            let entity = entity_from_row(row, enc)?;
            let emb_blob: Vec<u8> = row.get(18)?;
            let emb: Vec<f32> = emb_blob
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            Ok((entity, emb))
        })?;

        let mut scored: Vec<(Entity, f64)> = Vec::new();
        for row in rows {
            let (entity, emb) = row?;
            let sim = cosine_similarity(query_vec, &emb);
            scored.push((entity, sim));
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    // ─── Decay & Layer Progression ──────────────────────────────────

    /// Ebbinghaus decay half-life in milliseconds (default: 7 days).
    const DECAY_HALF_LIFE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

    /// Retrieval boost: how much decay_score increases on recall.
    const DECAY_BOOST: f64 = 0.25;

    /// Layer promotion thresholds (retrieval_count).
    const CORE_THRESHOLD: i64 = 20; // ≥20 retrievals → core
    const WORKING_THRESHOLD: i64 = 5; // ≥5 retrievals → working

    /// Compute Ebbinghaus decay score based on time since last access.
    /// decay = e^(-elapsed_ms / half_life_ms)
    /// Returns value in [0.0, 1.0] where 1.0 = just accessed.
    fn compute_decay(last_accessed_ms: i64, now_ms: i64) -> f64 {
        let elapsed = (now_ms - last_accessed_ms).max(0) as f64;
        let half_life = Self::DECAY_HALF_LIFE_MS as f64;
        if half_life <= 0.0 || elapsed <= 0.0 {
            return 1.0;
        }
        (-elapsed / half_life).exp().clamp(0.0, 1.0)
    }

    /// Boost decay score on retrieval (capped at 1.0).
    fn boost_decay(current: f64) -> f64 {
        (current + Self::DECAY_BOOST).min(1.0)
    }

    /// Determine layer based on retrieval_count.
    fn compute_layer(retrieval_count: i64) -> &'static str {
        if retrieval_count >= Self::CORE_THRESHOLD {
            "core"
        } else if retrieval_count >= Self::WORKING_THRESHOLD {
            "working"
        } else {
            "buffer"
        }
    }

    /// Recalculate decay scores for all non-archived entities.
    /// Called periodically or via mimir_decay tool.
    pub fn decay_tick(&self) -> Result<DecayReport, Box<dyn std::error::Error>> {
        self.decay_tick_with_limit(None)
    }

    /// Like decay_tick but with an optional max entities to process per call.
    fn decay_tick_with_limit(
        &self,
        max_entities: Option<i64>,
    ) -> Result<DecayReport, Box<dyn std::error::Error>> {
        let now = now_ms();
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;

        // Update decay_score for non-archived entities, optionally capped
        let sql = if let Some(max) = max_entities {
            format!(
                "SELECT id, last_accessed_unix_ms FROM entities WHERE archived = 0 LIMIT {}",
                max
            )
        } else {
            "SELECT id, last_accessed_unix_ms FROM entities WHERE archived = 0".to_string()
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;

        let mut updated = 0i64;
        let mut auto_archived = 0i64;
        let now_val = now;

        // M-2: wrap in RAII transaction so error paths roll back automatically
        let tx = self.conn.unchecked_transaction()?;

        for row in rows {
            let (id, last_access) = row?;
            let new_decay = Self::compute_decay(last_access, now_val);
            tx.execute(
                "UPDATE entities SET decay_score = ?1 WHERE id = ?2",
                params![new_decay, id],
            )?;
            updated += 1;

            // Auto-archive entities that have fully decayed (decay < 0.05)
            if new_decay < 0.05 {
                tx.execute(
                    "UPDATE entities SET archived = 1, archive_reason = 'decay threshold' WHERE id = ?1 AND archived = 0",
                    params![id],
                )?;
                auto_archived += 1;
                // Clean FTS5 index for auto-archived entity
                let _ = tx.execute(
                    "DELETE FROM entities_fts WHERE rowid = (SELECT rowid FROM entities WHERE id = ?1)",
                    params![id],
                );
            }
        }

        tx.commit()?;

        Ok(DecayReport {
            entities_checked: total,
            entities_updated: updated,
            auto_archived,
            completed_at_unix_ms: now,
        })
    }

    // ─── Entities ────────────────────────────────────────────────

    /// Compute trigram overlap similarity between two strings (0.0–1.0).
    /// Uses character trigrams for fast, language-agnostic comparison.
    fn trigram_similarity(a: &str, b: &str) -> f64 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }
        if a == b {
            return 1.0;
        }

        fn trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
            let chars: Vec<char> = s.chars().collect();
            if chars.len() < 3 {
                return std::collections::HashSet::new();
            }
            chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
        }

        let ta = trigrams(a);
        let tb = trigrams(b);

        if ta.is_empty() || tb.is_empty() {
            return 0.0;
        }

        let intersection = ta.intersection(&tb).count();
        let union = ta.len() + tb.len() - intersection;

        if union == 0 {
            return 0.0;
        }

        intersection as f64 / union as f64
    }

    /// Check for near-duplicate entities in the same category.
    /// Returns Some(existing_entity_id) if similarity > threshold.
    fn find_near_duplicate(
        &self,
        category: &str,
        body_json: &str,
        threshold: f64,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, body_json FROM entities WHERE category = ?1 AND archived = 0")?;
        let rows = stmt.query_map(params![category], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (id, existing_body) = row?;
            let sim = Self::trigram_similarity(body_json, &existing_body);
            if sim >= threshold {
                return Ok(Some(id));
            }
        }

        Ok(None)
    }

    /// Store or update an entity. Idempotent by (category, key).
    /// Returns the entity id and whether this was a create or update.
    pub fn remember(
        &self,
        entity: &Entity,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let tags_json = serde_json::to_string(&entity.tags)?;
        let links_json = serde_json::to_string(&entity.links)?;
        let archived_int = if entity.archived { 1 } else { 0 };
        let verified_int = if entity.verified { 1 } else { 0 };

        // Encrypt body_json with category+key as AAD to bind ciphertext to entity identity
        let body_encrypted = if let Some(ref enc) = self.encryption {
            let aad = format!("{}:{}", entity.category, entity.key);
            enc.encrypt(&entity.body_json, aad.as_bytes())
                .map_err(|e| format!("Encryption error in remember: {}", e))?
        } else {
            entity.body_json.clone()
        };

        let existing_id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM entities WHERE category = ?1 AND key = ?2",
                params![entity.category, entity.key],
                |r| r.get(0),
            )
            .ok();

        let action;
        let id;

        if let Some(ex_id) = existing_id {
            // Update existing entity — compute decay + boost (it's being remembered)
            id = ex_id;
            let now = now_ms();
            let old_decay: f64 = self
                .conn
                .query_row(
                    "SELECT decay_score FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(1.0);
            let old_count: i64 = self
                .conn
                .query_row(
                    "SELECT retrieval_count FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let boosted = Self::boost_decay(old_decay);
            let new_layer = Self::compute_layer(old_count + 1);

            // M-1: wrap entity UPDATE + FTS UPDATE in a transaction
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE entities SET
                    body_json = ?1, status = ?2, type = ?3, tags = ?4,
                    decay_score = ?5, layer = ?6, topic_path = ?7,
                    archived = ?8, archive_reason = ?9, links = ?10,
                    verified = ?11, source = ?12, last_accessed_unix_ms = ?13,
                    always_on = ?14, certainty = ?15,
                    retrieval_count = retrieval_count + 1
                 WHERE id = ?16",
                params![
                    body_encrypted,
                    entity.status,
                    entity.entity_type,
                    tags_json,
                    boosted,
                    new_layer,
                    entity.topic_path,
                    archived_int,
                    entity.archive_reason,
                    links_json,
                    verified_int,
                    entity.source,
                    now,
                    entity.always_on as i32,
                    entity.certainty,
                    id,
                ],
            )?;

            // Update FTS5 index
            tx.execute(
                "UPDATE entities_fts SET body_json = ?1 WHERE rowid = (SELECT rowid FROM entities WHERE id = ?2)",
                params![entity.body_json, id],
            )?;
            tx.commit()?;

            action = "updated".to_string();
        } else {
            // Check for near-duplicates before inserting
            let dup_threshold = 0.7; // 70% trigram similarity
            if let Ok(Some(dup_id)) =
                self.find_near_duplicate(&entity.category, &entity.body_json, dup_threshold)
            {
                // Near-duplicate found — bump its importance instead of creating new
                let _ = self.conn.execute(
                    "UPDATE entities SET decay_score = MIN(1.0, decay_score + 0.15),
                     retrieval_count = retrieval_count + 1,
                     last_accessed_unix_ms = ?1 WHERE id = ?2",
                    params![now_ms(), dup_id],
                );
                return Ok((dup_id, "deduped (new key not created)".to_string()));
            }

            // Insert new entity
            id = entity.id.clone();

            // M-1: wrap entity row + FTS index write in a transaction
            // so a failure in one doesn't leave the other orphaned.
            let tx = self.conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO entities
                 (id, category, key, body_json, status, type, tags,
                  decay_score, retrieval_count, layer, topic_path,
                  archived, archive_reason, links, verified, source,
                  always_on, certainty, created_at_unix_ms, last_accessed_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                         ?8, ?9, ?10, ?11,
                         ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20)",
                params![
                    id,
                    entity.category,
                    entity.key,
                    body_encrypted,
                    entity.status,
                    entity.entity_type,
                    tags_json,
                    entity.decay_score,
                    entity.retrieval_count,
                    entity.layer,
                    entity.topic_path,
                    archived_int,
                    entity.archive_reason,
                    links_json,
                    verified_int,
                    entity.source,
                    entity.always_on as i32,
                    entity.certainty,
                    entity.created_at_unix_ms,
                    entity.last_accessed_unix_ms,
                ],
            )?;

            // Add to FTS5 index
            tx.execute(
                "INSERT INTO entities_fts (rowid, body_json) VALUES (last_insert_rowid(), ?1)",
                params![entity.body_json],
            )?;
            tx.commit()?;

            action = "created".to_string();
        }

        Ok((id, action))
    }

    /// Search entities with FTS5 + LIKE fallback and optional filters.
    pub fn recall(&self, params: &RecallParams) -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
        // Dense vector search path
        if params.mode == crate::models::SearchMode::Dense
            || params.mode == crate::models::SearchMode::Hybrid
        {
            if let Some(ref query_vec) = params.embedding {
                let dense_results = self.dense_search(query_vec, params.limit as usize)?;

                if params.mode == crate::models::SearchMode::Dense {
                    return Ok(dense_results.into_iter().map(|(e, _)| e).collect());
                }

                // Hybrid: run FTS5 sparse search too, then fuse via RRF
                let sparse = self.fts5_search(params)?;
                let dense_scored: Vec<(Entity, f64)> = dense_results
                    .into_iter()
                    .collect();
                let sparse_scored: Vec<(Entity, f64)> = sparse
                    .into_iter()
                    .map(|e| {
                        let score = e.decay_score;
                        (e, score)
                    })
                    .collect();
                let fused = crate::db::reciprocal_rank_fusion(
                    &dense_scored,
                    &sparse_scored,
                    60.0,
                    params.limit as usize,
                );
                return Ok(fused.into_iter().map(|(e, _)| e).collect());
            }
            // Fall through to FTS5 if no embedding vector provided
        }

        self.fts5_search(params)
    }

    /// Core FTS5 + LIKE keyword search (extracted for reuse by recall and hybrid).
    fn fts5_search(
        &self,
        params: &RecallParams,
    ) -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        // Keyword search: FTS5 OR match + LIKE fallback
        if !params.query.is_empty() {
            let words: Vec<&str> = params
                .query
                .split_whitespace()
                .filter(|w| !w.is_empty())
                .collect();

            if !words.is_empty() {
                // FTS5 query: wrap each term in double-quotes to treat special chars literally
                let escape_fts = |s: &str| -> String {
                    // Double any double-quotes within the term (FTS5 escaping)
                    s.replace('"', "\"\"")
                };
                let fts_query = words
                    .iter()
                    .map(|w| {
                        let escaped = escape_fts(w);
                        if escaped.is_empty() {
                            "\"\"".to_string()
                        } else {
                            format!("\"{}\"", escaped)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" OR ");
                param_values.push(Box::new(fts_query));

                // LIKE fallback: match each word as substring
                let mut like_clauses = Vec::new();
                for _ in &words {
                    let idx = param_values.len() + 1;
                    like_clauses.push(format!("body_json LIKE ?{}", idx));
                }
                for word in &words {
                    param_values.push(Box::new(format!("%{}%", word.replace('\'', "''"))));
                }

                // When include_archived, skip FTS5 — archived entities have no FTS5 entries
                if params.include_archived {
                    conditions.push(like_clauses.join(" OR "));
                } else {
                    conditions.push(format!(
                        "((rowid IN (SELECT rowid FROM entities_fts WHERE entities_fts MATCH ?1)) OR {})",
                        like_clauses.join(" OR ")
                    ));
                }
            }
        }

        // Filter by category
        if let Some(ref cat) = params.category {
            if !cat.is_empty() {
                conditions.push(format!("category = ?{}", param_values.len() + 1));
                param_values.push(Box::new(cat.clone()));
            }
        }

        // Filter by type
        if let Some(ref t) = params.entity_type {
            if !t.is_empty() {
                conditions.push(format!("type = ?{}", param_values.len() + 1));
                param_values.push(Box::new(t.clone()));
            }
        }

        // Filter by decay score
        if params.min_decay > 0.0 {
            conditions.push(format!("decay_score >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(params.min_decay));
        }

        // Filter by topic path prefix
        if let Some(ref tp) = params.topic_path {
            if !tp.is_empty() {
                conditions.push(format!(
                    "topic_path LIKE ?{} ESCAPE '\\'",
                    param_values.len() + 1
                ));
                let escaped = tp
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                param_values.push(Box::new(format!("{}%", escaped)));
            }
        }

        // Filter by always_on flag
        if let Some(ao) = params.always_on {
            conditions.push(format!("always_on = ?{}", param_values.len() + 1));
            param_values.push(Box::new(ao as i32));
        }

        // Exclude archived unless explicitly requested
        if !params.include_archived {
            conditions.push("archived = 0".to_string());
        }

        // Build query
        let mut sql = String::from(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities",
        );

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        // Rank by retrieval count + recency
        sql.push_str(" ORDER BY retrieval_count DESC, last_accessed_unix_ms DESC");

        let safe_limit = params.limit.clamp(0, 1000);
        sql.push_str(&format!(" LIMIT ?{}", param_values.len() + 1));
        param_values.push(Box::new(safe_limit));

        if params.offset > 0 {
            let safe_offset = params.offset.clamp(0, 10000);
            sql.push_str(&format!(" OFFSET ?{}", param_values.len() + 1));
            param_values.push(Box::new(safe_offset));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let enc = self.encryption.as_ref();
        let rows = stmt.query_map(param_refs.as_slice(), |row| entity_from_row(row, enc))?;

        let mut items = Vec::new();
        for row in rows {
            let mut entity = row?;
            // Update retrieval count, recency, decay boost, and layer
            if !params.skip_side_effects {
                let new_count = entity.retrieval_count + 1;
                let boosted_decay = Self::boost_decay(entity.decay_score);
                let new_layer = Self::compute_layer(new_count);
                let _ = self.conn.execute(
                    "UPDATE entities SET retrieval_count = ?1,
                     last_accessed_unix_ms = ?2, decay_score = ?3, layer = ?4
                     WHERE id = ?5",
                    params![new_count, now_ms(), boosted_decay, new_layer, entity.id],
                );
            }

            // #103: Apply preview cap with drill-down footer (BrainDB-inspired)
            if let Some(cap) = params.preview_cap {
                let cap = cap as usize;
                if entity.body_json.len() > cap {
                    let extra = entity.body_json.len() - cap;
                    let truncated = entity.body_json[..cap].to_string();
                    let footer = format!(
                        "\n--truncated ({} more chars)-- full body: get_entity(\"{}\"). If large, delegate_to_subagent to read/extract it without polluting this context.",
                        extra, entity.id
                    );
                    entity.body_json = format!("{}{}", truncated, footer);
                    items.push(entity);
                    continue;
                }
            }
            items.push(entity);
        }

        // #106: Content witness signal (additive boost, never penalizes)
        if params.content_weight > 0.0 && !params.query.is_empty() {
            let query_lower = params.query.to_lowercase();
            let size_pivot: f64 = 5000.0;
            for entity in &mut items {
                let body_lower = entity.body_json.to_lowercase();
                if body_lower.contains(&query_lower) {
                    let content_len = entity.body_json.len() as f64;
                    let damper = 1.0 / (1.0 + (1.0 + content_len / size_pivot.max(1.0)).log10());
                    // Boost decay_score as a proxy for ranking (additive, never penalizes)
                    entity.decay_score =
                        (entity.decay_score + params.content_weight * damper).min(1.0);
                }
            }
            // Re-sort after content witness boost
            items.sort_by(|a, b| {
                b.decay_score
                    .partial_cmp(&a.decay_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        // #105: Two-level diversity quota (BrainDB-inspired)
        // Per-keyword halving: each distinct keyword gets ceil(max_results x halving^n) slots
        if params.diversity_halving < 1.0 && params.diversity_halving > 0.0 && !items.is_empty() {
            items = Self::apply_diversity_quota(
                items,
                params.limit as usize,
                params.diversity_halving,
                &params.query,
            );
        }

        Ok(items)
    }

    /// #105: Apply per-keyword halving diversity quota.
    /// Each distinct matched keyword gets ceil(max_results x halving^n) slots,
    /// preventing a single popular keyword from monopolizing results.
    fn apply_diversity_quota(
        mut items: Vec<Entity>,
        max_results: usize,
        halving: f64,
        query: &str,
    ) -> Vec<Entity> {
        // Extract the dominant matched keyword for each entity
        // (the first query word that appears in the entity body)
        let query_words: Vec<&str> = query.split_whitespace().filter(|w| w.len() >= 3).collect();
        let mut kw_slots: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        let mut kw_order: Vec<String> = Vec::new();
        let mut out: Vec<Entity> = Vec::new();
        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();

        for entity in items.drain(..) {
            if out.len() >= max_results {
                break;
            }
            if taken.contains(&entity.id) {
                continue;
            }

            // Find dominant keyword: first query word found in body_json
            let body_lower = entity.body_json.to_lowercase();
            let dom_kw = query_words
                .iter()
                .find(|w| body_lower.contains(&w.to_lowercase()))
                .map(|w| w.to_string());

            if let Some(ref kw) = dom_kw {
                if !kw_slots.contains_key(kw) {
                    let n = kw_slots.len();
                    kw_slots.insert(
                        kw.clone(),
                        (max_results as f64 * halving.powi(n as i32)).ceil() as i64,
                    );
                    kw_order.push(kw.clone());
                }
                let remaining = match kw_slots.get_mut(kw) {
                    Some(r) => r,
                    None => continue, // Should not happen: key was just inserted
                };
                if *remaining <= 0 {
                    continue; // This keyword's quota exhausted
                }
                *remaining -= 1;
            }

            taken.insert(entity.id.clone());
            out.push(entity);
        }

        out
    }

    /// Get a single entity by category and key.
    pub fn get_entity(
        &self,
        category: &str,
        key: &str,
    ) -> Result<Option<Entity>, Box<dyn std::error::Error>> {
        // Find the entity by category + key
        let mut stmt = self.conn.prepare(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities WHERE category = ?1 AND key = ?2 LIMIT 1",
        )?;

        let mut rows = stmt.query_map(params![category, key], |row| {
            entity_from_row(row, self.encryption.as_ref())
        })?;

        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// Soft-delete an entity (set archived = 1).
    pub fn forget(
        &self,
        category: &str,
        key: &str,
        reason: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        // M-1 extended: wrap forget's entity UPDATE + FTS DELETE in a transaction
        let tx = self.conn.unchecked_transaction()?;
        let affected = tx.execute(
            "UPDATE entities SET archived = 1, archive_reason = ?1,
             last_accessed_unix_ms = ?2
             WHERE category = ?3 AND key = ?4 AND archived = 0",
            params![reason, now_ms(), category, key],
        )?;
        // Clean FTS5 index for archived entity
        if affected > 0 {
            let _ = tx.execute(
                "DELETE FROM entities_fts WHERE rowid = (SELECT rowid FROM entities WHERE category = ?1 AND key = ?2)",
                params![category, key],
            );
        }
        tx.commit()?;
        Ok(affected > 0)
    }

    /// Create a link from one entity to another.
    pub fn link(
        &self,
        from_category: &str,
        from_key: &str,
        to_id: &str,
        relationship: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Verify both entities exist
        let from = self
            .get_entity(from_category, from_key)?
            .ok_or("Source entity not found")?;
        let _to: String = self
            .conn
            .query_row(
                "SELECT id FROM entities WHERE id = ?1",
                params![to_id],
                |r| r.get(0),
            )
            .map_err(|_| "Target entity not found")?;

        // Get existing links (default to empty array if missing)
        let links_str: String = self
            .conn
            .query_row(
                "SELECT links FROM entities WHERE id = ?1",
                params![from.id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());

        let mut links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
        // Avoid duplicates
        if !links.iter().any(|l| l.target_id == to_id) {
            links.push(MemoryLink {
                target_id: to_id.to_string(),
                relationship: relationship.to_string(),
                weight: 0.5,
            });
        }
        let new_links = serde_json::to_string(&links)?;
        self.conn.execute(
            "UPDATE entities SET links = ?1, last_accessed_unix_ms = ?2 WHERE id = ?3",
            params![new_links, now_ms(), from.id],
        )?;

        Ok(())
    }

    /// Remove a link from one entity to another.
    pub fn unlink(
        &self,
        from_category: &str,
        from_key: &str,
        to_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let from = self
            .get_entity(from_category, from_key)?
            .ok_or("Source entity not found")?;

        let links_str: String = self.conn.query_row(
            "SELECT links FROM entities WHERE id = ?1",
            params![from.id],
            |r| r.get(0),
        )?;

        let mut links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
        let before = links.len();
        links.retain(|l| l.target_id != to_id);

        if links.len() == before {
            return Ok(()); // Link wasn't there, nothing to do
        }

        let new_links = serde_json::to_string(&links)?;
        self.conn.execute(
            "UPDATE entities SET links = ?1, last_accessed_unix_ms = ?2 WHERE id = ?3",
            params![new_links, now_ms(), from.id],
        )?;

        Ok(())
    }

    // ─── Journal ─────────────────────────────────────────────────

    /// Append a journal event.
    pub fn journal(&self, event: &JournalEvent) -> Result<(), Box<dyn std::error::Error>> {
        self.conn.execute(
            "INSERT INTO journal
             (id, event_type, evaluated_json, acted_json, forward_json,
              category, key, entity_id, created_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.id,
                event.event_type,
                event.evaluated_json,
                event.acted_json,
                event.forward_json,
                event.category,
                event.key,
                event.entity_id,
                event.created_at_unix_ms,
            ],
        )?;
        Ok(())
    }

    /// Query journal events with time-range and filter parameters.
    pub fn timeline(
        &self,
        params: &TimelineParams,
    ) -> Result<Vec<JournalEvent>, Box<dyn std::error::Error>> {
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(from) = params.from_ms {
            conditions.push(format!("created_at_unix_ms >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(from));
        }

        if let Some(to) = params.to_ms {
            conditions.push(format!("created_at_unix_ms <= ?{}", param_values.len() + 1));
            param_values.push(Box::new(to));
        }

        if let Some(ref et) = params.event_type {
            if !et.is_empty() {
                conditions.push(format!("event_type = ?{}", param_values.len() + 1));
                param_values.push(Box::new(et.clone()));
            }
        }

        if let Some(ref cat) = params.category {
            if !cat.is_empty() {
                conditions.push(format!("category = ?{}", param_values.len() + 1));
                param_values.push(Box::new(cat.clone()));
            }
        }

        if let Some(ref eid) = params.entity_id {
            if !eid.is_empty() {
                conditions.push(format!("entity_id = ?{}", param_values.len() + 1));
                param_values.push(Box::new(eid.clone()));
            }
        }

        let mut sql = String::from(
            "SELECT id, event_type, evaluated_json, acted_json, forward_json,
                    category, key, entity_id, created_at_unix_ms
             FROM journal",
        );

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        sql.push_str(" ORDER BY created_at_unix_ms DESC");

        let safe_limit = params.limit.clamp(0, 1000);
        sql.push_str(&format!(" LIMIT ?{}", param_values.len() + 1));
        param_values.push(Box::new(safe_limit));

        if params.offset > 0 {
            let safe_offset = params.offset.clamp(0, 10000);
            sql.push_str(&format!(" OFFSET ?{}", param_values.len() + 1));
            param_values.push(Box::new(safe_offset));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(JournalEvent {
                id: row.get(0)?,
                event_type: row.get(1)?,
                evaluated_json: row.get(2)?,
                acted_json: row.get(3)?,
                forward_json: row.get(4)?,
                category: row.get(5)?,
                key: row.get(6)?,
                entity_id: row.get(7)?,
                created_at_unix_ms: row.get(8)?,
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    // ─── State ───────────────────────────────────────────────────

    /// Set a state key-value pair with optional TTL.
    pub fn state_set(&self, entry: &StateEntry) -> Result<(), Box<dyn std::error::Error>> {
        // Clean expired entries first (opportunistic)
        let _ = self.conn.execute(
            "DELETE FROM state WHERE expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms < ?1",
            params![now_ms()],
        );

        self.conn.execute(
            "INSERT OR REPLACE INTO state (key, value_json, expires_at_unix_ms, created_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                entry.key,
                entry.value_json,
                entry.expires_at_unix_ms,
                entry.created_at_unix_ms,
            ],
        )?;
        Ok(())
    }

    /// Get a state value. Returns None if the key doesn't exist or has expired.
    pub fn state_get(&self, key: &str) -> Result<Option<StateEntry>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, value_json, expires_at_unix_ms, created_at_unix_ms
             FROM state WHERE key = ?1",
        )?;

        let mut rows = stmt.query_map(params![key], |row| {
            Ok(StateEntry {
                key: row.get(0)?,
                value_json: row.get(1)?,
                expires_at_unix_ms: row.get(2)?,
                created_at_unix_ms: row.get(3)?,
            })
        })?;

        if let Some(row) = rows.next() {
            let entry = row?;
            // Check expiration
            if let Some(expires) = entry.expires_at_unix_ms {
                if expires < now_ms() {
                    // Expired — delete and return None
                    let _ = self
                        .conn
                        .execute("DELETE FROM state WHERE key = ?1", params![key]);
                    return Ok(None);
                }
            }
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    /// Delete a state entry.
    pub fn state_delete(&self, key: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let affected = self
            .conn
            .execute("DELETE FROM state WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    /// List state keys matching an optional prefix.
    pub fn state_list(&self, prefix: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        // Delete expired entries first
        let _ = self.conn.execute(
            "DELETE FROM state WHERE expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms < ?1",
            params![now_ms()],
        );

        let keys: Vec<String> = if prefix.is_empty() {
            let mut stmt = self.conn.prepare("SELECT key FROM state ORDER BY key")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT key FROM state WHERE key LIKE ?1 ORDER BY key")?;
            let pattern = format!("{}%", prefix);
            let rows = stmt.query_map(params![pattern], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        };

        Ok(keys)
    }

    // ─── Management ──────────────────────────────────────────────

    /// Database statistics.
    pub fn stats(&self) -> Result<Stats, Box<dyn std::error::Error>> {
        schema::gather_stats(&self.conn, &self.db_path)
    }

    /// Migrate from v0.1.x database.
    pub fn migrate_from_v0_1(
        &self,
        old_path: &str,
    ) -> Result<crate::models::MigrationReport, Box<dyn std::error::Error>> {
        schema::migrate_from_v0_1(old_path, &self.conn)
    }

    // ─── Memory Synthesis ───────────────────────────────────────────

    /// Traverse entity links starting from a given entity.
    /// Returns the entity and all linked entities up to max_depth.
    pub fn traverse_chain(
        &self,
        category: &str,
        key: &str,
        max_depth: i64,
        max_nodes: i64,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let root = self
            .get_entity(category, key)?
            .ok_or_else(|| format!("entity not found: {}/{}", category, key))?;

        // Get root links
        let links_json: String = self
            .conn
            .query_row(
                "SELECT links FROM entities WHERE id = ?1",
                params![root.id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());
        let root_links: Vec<MemoryLink> = serde_json::from_str(&links_json).unwrap_or_default();
        let root_links_json: Vec<serde_json::Value> = root_links
            .iter()
            .map(|l| serde_json::json!({"target_id": l.target_id, "relationship": l.relationship}))
            .collect();

        let mut visited = std::collections::HashSet::new();
        let mut traversed = Vec::new();

        visited.insert(root.id.clone());
        self._traverse_links(
            &root.id,
            &mut traversed,
            &mut visited,
            max_depth,
            max_nodes,
            0,
        );

        let chain = serde_json::json!({
            "entity": {
                "id": root.id,
                "category": root.category,
                "key": root.key,
                "body_json": root.body_json,
                "links": root_links_json
            },
            "traversed": traversed
        });

        Ok(chain)
    }

    fn _traverse_links(
        &self,
        entity_id: &str,
        traversed: &mut Vec<serde_json::Value>,
        visited: &mut std::collections::HashSet<String>,
        max_depth: i64,
        max_nodes: i64,
        current_depth: i64,
    ) {
        if current_depth >= max_depth || traversed.len() as i64 >= max_nodes {
            return;
        }

        let links_json: String = self
            .conn
            .query_row(
                "SELECT links FROM entities WHERE id = ?1",
                params![entity_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());

        let links: Vec<MemoryLink> = serde_json::from_str(&links_json).unwrap_or_default();

        for link in &links {
            if visited.contains(&link.target_id) {
                continue;
            }

            match self.get_entity_by_id(&link.target_id) {
                Ok(Some(entity)) => {
                    visited.insert(link.target_id.clone());

                    // Get this entity's outgoing links
                    let child_links_json: String = self
                        .conn
                        .query_row(
                            "SELECT links FROM entities WHERE id = ?1",
                            params![entity.id],
                            |r| r.get(0),
                        )
                        .unwrap_or_else(|_| "[]".to_string());
                    let child_links: Vec<MemoryLink> =
                        serde_json::from_str(&child_links_json).unwrap_or_default();
                    let child_links_json: Vec<serde_json::Value> = child_links.iter().map(|l| {
                    serde_json::json!({"target_id": l.target_id, "relationship": l.relationship})
                }).collect();

                    let node = serde_json::json!({
                        "id": entity.id,
                        "category": entity.category,
                        "key": entity.key,
                        "body_json": entity.body_json,
                        "relationship": link.relationship,
                        "depth": current_depth + 1,
                        "links": child_links_json
                    });

                    traversed.push(node.clone());

                    self._traverse_links(
                        &entity.id,
                        traversed,
                        visited,
                        max_depth,
                        max_nodes,
                        current_depth + 1,
                    );
                }
                Ok(None) => {
                    // Dangling link — target entity no longer exists
                }
                Err(e) => {
                    eprintln!(
                        "mimir: traverse error reading entity {}: {}",
                        link.target_id, e
                    );
                }
            }
        }
    }

    /// Get entity by ID (internal helper).
    fn get_entity_by_id(&self, id: &str) -> Result<Option<Entity>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            entity_from_row(row, self.encryption.as_ref())
        })?;
        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// Public alias for get_entity_by_id used by the web API.
    pub fn get_entity_by_id_public(
        &self,
        id: &str,
    ) -> Result<Option<Entity>, Box<dyn std::error::Error>> {
        self.get_entity_by_id(id)
    }

    /// List entities with pagination and optional filters.
    pub fn list_entities(
        &self,
        offset: i64,
        limit: i64,
        category: Option<&str>,
        layer: Option<&str>,
    ) -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
        let mut sql = String::from(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities WHERE archived = 0",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(cat) = category {
            if !cat.is_empty() {
                sql.push_str(&format!(" AND category = ?{}", params.len() + 1));
                params.push(Box::new(cat.to_string()));
            }
        }
        if let Some(l) = layer {
            if !l.is_empty() {
                sql.push_str(&format!(" AND layer = ?{}", params.len() + 1));
                params.push(Box::new(l.to_string()));
            }
        }

        sql.push_str(" ORDER BY last_accessed_unix_ms DESC");
        sql.push_str(&format!(
            " LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2
        ));
        params.push(Box::new(limit));
        params.push(Box::new(offset));

        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let enc = self.encryption.as_ref();
        let rows = stmt.query_map(param_refs.as_slice(), |row| entity_from_row(row, enc))?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Get recent journal events.
    pub fn get_recent_journal(
        &self,
        limit: i64,
    ) -> Result<Vec<JournalEvent>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, event_type, evaluated_json, acted_json, forward_json,
                    category, key, entity_id, created_at_unix_ms
             FROM journal ORDER BY created_at_unix_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(JournalEvent {
                id: row.get(0)?,
                event_type: row.get(1)?,
                evaluated_json: row.get::<_, String>(2).unwrap_or_default(),
                acted_json: row.get::<_, String>(3).unwrap_or_default(),
                forward_json: row.get::<_, String>(4).unwrap_or_default(),
                category: row.get::<_, String>(5).unwrap_or_default(),
                key: row.get::<_, String>(6).unwrap_or_default(),
                entity_id: row.get::<_, String>(7).unwrap_or_default(),
                created_at_unix_ms: row.get(8)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Build an entity link graph: nodes + edges for visualization.
    pub fn get_entity_graph(
        &self,
    ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), Box<dyn std::error::Error>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, category, key, links FROM entities WHERE archived = 0")?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let category: String = row.get(1)?;
            let key: String = row.get(2)?;
            let links_str: String = row.get::<_, String>(3).unwrap_or_else(|_| "[]".to_string());
            let links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
            Ok((id, category, key, links))
        })?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for row in rows {
            let (id, category, key, links) = row?;
            if seen_ids.insert(id.clone()) {
                nodes.push(GraphNode {
                    id: id.clone(),
                    label: key,
                    category,
                });
            }
            for link in &links {
                edges.push(GraphEdge {
                    from: id.clone(),
                    to: link.target_id.clone(),
                    relationship: link.relationship.clone(),
                });
            }
        }
        Ok((nodes, edges))
    }

    /// Score an entity's quality (0.0–1.0). Agents rate memories as useful/wrong.
    pub fn score_entity(
        &self,
        category: &str,
        key: &str,
        score: f64,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let score = score.clamp(0.0, 1.0);
        let affected = self.conn.execute(
            "UPDATE entities SET verified = ?1, decay_score = ?2,
             last_accessed_unix_ms = ?3 WHERE category = ?4 AND key = ?5",
            params![(score >= 0.7) as i32, score, now_ms(), category, key],
        )?;
        Ok(affected > 0)
    }

    /// Detect conflicting entities — entities in the same category with very different body_json.
    /// Returns pairs of entities with low trigram similarity (potential conflicts).
    /// #107: Also factors in certainty — low-certainty entities on the same topic
    /// amplify the conflict signal. Two entities with certainty < 0.4 on similar
    /// topics are flagged even at higher similarity thresholds.
    pub fn detect_conflicts(
        &self,
        category: &str,
        threshold: f64,
        limit: i64,
        offset: i64,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key, body_json, certainty FROM entities WHERE category = ?1 AND archived = 0
             ORDER BY last_accessed_unix_ms DESC LIMIT 200 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![category, offset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3).unwrap_or(0.5),
            ))
        })?;

        let entities: Vec<(String, String, String, f64)> = rows.filter_map(|r| r.ok()).collect();
        let mut conflicts = Vec::new();

        for i in 0..entities.len() {
            for j in (i + 1)..entities.len() {
                let (ref id1, ref key1, ref body1, cert1) = entities[i];
                let (ref id2, ref key2, ref body2, cert2) = entities[j];
                let sim = Self::trigram_similarity(body1, body2);
                // #107: Certainty-adjusted threshold — low-certainty pairs need less trigram overlap to flag
                let min_cert = cert1.min(cert2);
                let adj_threshold = if min_cert < 0.4 {
                    threshold * 1.5 // Relaxed threshold: catch more potential conflicts when certainty is low
                } else {
                    threshold
                };
                if sim < adj_threshold {
                    conflicts.push(serde_json::json!({
                        "entity_a": {"id": id1, "key": key1, "certainty": cert1},
                        "entity_b": {"id": id2, "key": key2, "certainty": cert2},
                        "similarity": sim,
                        "conflict_likely": sim < 0.3 || min_cert < 0.3,
                        "certainty_boosted": min_cert < 0.4
                    }));
                    if conflicts.len() as i64 >= limit {
                        break;
                    }
                }
            }
            if conflicts.len() as i64 >= limit {
                break;
            }
        }

        Ok(serde_json::json!({
            "category": category,
            "entities_compared": entities.len(),
            "conflicts_found": conflicts.len(),
            "threshold": threshold,
            "conflicts": conflicts
        }))
    }

    /// Compact: archive entities below a decay threshold.
    pub fn compact(
        &self,
        min_decay: f64,
        dry_run: bool,
    ) -> Result<CompactReport, Box<dyn std::error::Error>> {
        let examined: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;

        let archived = if dry_run {
            self.conn.query_row(
                "SELECT COUNT(*) FROM entities WHERE archived = 0 AND decay_score < ?1",
                params![min_decay],
                |r| r.get(0),
            )?
        } else {
            // M-1 extended: wrap compact UPDATE + FTS DELETE in a transaction
            let tx = self.conn.unchecked_transaction()?;
            let count = tx.execute(
                "UPDATE entities SET archived = 1, archive_reason = 'decay threshold',
                 last_accessed_unix_ms = ?1
                 WHERE archived = 0 AND decay_score < ?2",
                params![now_ms(), min_decay],
            )? as i64;
            // Clean FTS5 index for compacted entities
            let _ = tx.execute(
                "DELETE FROM entities_fts WHERE rowid IN (SELECT rowid FROM entities WHERE archived = 1 AND archive_reason = 'decay threshold')",
                [],
            );
            tx.commit()?;
            count
        };

        Ok(CompactReport {
            entities_archived: archived,
            entities_examined: examined,
            dry_run,
            completed_at_unix_ms: now_ms(),
        })
    }

    // ─── Embedding Search (v0.3 — requires embedding feature) ───────
    // Hybrid search with cosine similarity re-ranking.
    // Enable with: cargo build --features embedding
    // Requires OPENAI_API_KEY or compatible endpoint.
    // See ROADMAP.md for full spec.

    // ─── Vault Export / Import ──────────────────────────────────────

    /// Export all non-archived entities to .md files in a vault directory.
    /// Each entity becomes a .md file with YAML frontmatter.
    /// Idempotent — updates changed files, creates new ones, never deletes.
    pub fn vault_export(&self, vault_dir: &str) -> Result<VaultReport, Box<dyn std::error::Error>> {
        use std::fs;
        use std::path::Path;

        fs::create_dir_all(vault_dir)?;
        let vault = Path::new(vault_dir);

        let mut stmt = self.conn.prepare(
            "SELECT id, category, key, body_json, type, tags, decay_score,
                    retrieval_count, layer, created_at_unix_ms, last_accessed_unix_ms
             FROM entities WHERE archived = 0",
        )?;

        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?, // id
                r.get::<_, String>(1)?, // category
                r.get::<_, String>(2)?, // key
                r.get::<_, String>(3)?, // body_json
                r.get::<_, String>(4)?, // type
                r.get::<_, String>(5)?, // tags
                r.get::<_, f64>(6)?,    // decay_score
                r.get::<_, i64>(7)?,    // retrieval_count
                r.get::<_, String>(8)?, // layer
                r.get::<_, i64>(9)?,    // created_at
                r.get::<_, i64>(10)?,   // last_accessed
            ))
        })?;

        let mut files_created = 0i64;
        let mut files_updated = 0i64;
        let mut errors = Vec::new();

        for row in rows {
            let (
                id,
                category,
                key,
                body_json,
                etype,
                tags,
                decay,
                retrievals,
                layer,
                created,
                accessed,
            ) = row?;
            // Sanitize id for filesystem: only alphanumeric, hyphen, underscore
            let safe_id: String = id
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let filename = format!("{}.md", safe_id);
            let filepath = vault.join(&filename);

            let created_str = chrono_like(created);
            let accessed_str = chrono_like(accessed);

            let md_content = format!(
                "---
id: {}
category: {}
key: {}
type: {}
tags: {}
decay_score: {:.4}
retrieval_count: {}
layer: {}
created: {}
last_accessed: {}
---

{}
",
                id,
                category,
                key,
                etype,
                tags,
                decay,
                retrievals,
                layer,
                created_str,
                accessed_str,
                body_json
            );

            let _action = if filepath.exists() {
                // Only update if content changed
                let existing = fs::read_to_string(&filepath).unwrap_or_default();
                if existing == md_content {
                    continue; // unchanged
                }
                files_updated += 1;
                "updated"
            } else {
                files_created += 1;
                "created"
            };

            if let Err(e) = fs::write(&filepath, &md_content) {
                errors.push(format!("{}: {}", filename, e));
            }
        }

        Ok(VaultReport {
            files_created,
            files_updated,
            errors,
            vault_dir: vault_dir.to_string(),
            completed_at_unix_ms: now_ms(),
        })
    }

    /// Import .md files from a vault directory into the database.
    /// Reads YAML frontmatter + body, calls remember() for each.
    pub fn vault_import(&self, vault_dir: &str) -> Result<VaultReport, Box<dyn std::error::Error>> {
        use std::fs;
        use std::path::Path;

        let vault = Path::new(vault_dir);
        if !vault.is_dir() {
            return Err(format!("{} is not a directory", vault_dir).into());
        }

        let mut files_created = 0i64;
        let mut files_updated = 0i64;
        let mut errors = Vec::new();

        for entry in fs::read_dir(vault)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    errors.push(format!("{}: {}", path.display(), e));
                    continue;
                }
            };

            // Parse YAML frontmatter: find opening and closing --- on their own lines
            let mut lines = content.lines().peekable();
            // Skip leading blank lines
            while let Some(line) = lines.peek() {
                if line.trim().is_empty() {
                    lines.next();
                } else {
                    break;
                }
            }
            // Find opening ---
            match lines.next() {
                Some(line) if line.trim() == "---" => {}
                _ => {
                    errors.push(format!("{}: no frontmatter", path.display()));
                    continue;
                }
            }
            // Read frontmatter lines until closing ---
            let mut fm_lines = Vec::new();
            let mut found_close = false;
            for line in lines.by_ref() {
                if line.trim() == "---" {
                    found_close = true;
                    break;
                }
                fm_lines.push(line);
            }
            if !found_close {
                errors.push(format!("{}: unclosed frontmatter", path.display()));
                continue;
            }
            let fm = fm_lines.join("\n");
            // Remaining lines are the body
            let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();

            // Extract fields from frontmatter
            let get_fm = |key: &str| -> String {
                fm.lines()
                    .find(|l| l.starts_with(&format!("{}:", key)))
                    .and_then(|l| l.split_once(':').map(|x| x.1))
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };

            let raw_id = get_fm("id");
            // Validate id: no path separators, no parent dir references
            let id = if raw_id.contains('/')
                || raw_id.contains('\\')
                || raw_id == ".."
                || raw_id.starts_with("../")
                || raw_id.starts_with("..\\")
            {
                String::new() // Force UUID generation instead
            } else {
                raw_id
            };
            let category = get_fm("category");
            let key = get_fm("key");
            let etype = get_fm("type");
            let tags_str = get_fm("tags");
            let decay: f64 = get_fm("decay_score").parse().unwrap_or(1.0);
            let layer = get_fm("layer");

            let tags: Vec<String> = if tags_str.is_empty() || tags_str == "[]" {
                vec![]
            } else {
                tags_str
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };

            let entity = Entity {
                id: if id.is_empty() {
                    let raw = uuid::Uuid::new_v4().to_string().replace('-', "");
                    format!("mem-{}", &raw[..12.min(raw.len())])
                } else {
                    id
                },
                category: if category.is_empty() {
                    "general".to_string()
                } else {
                    category
                },
                key: if key.is_empty() {
                    "imported".to_string()
                } else {
                    key
                },
                body_json: body,
                status: "active".to_string(),
                entity_type: if etype.is_empty() {
                    "insight".to_string()
                } else {
                    etype
                },
                tags,
                decay_score: decay,
                retrieval_count: 0,
                layer: if layer.is_empty() {
                    "buffer".to_string()
                } else {
                    layer
                },
                topic_path: String::new(),
                archived: false,
                archive_reason: String::new(),
                links: vec![],
                verified: false,
                source: "vault-import".to_string(),
                always_on: false,
                certainty: 0.5,
                created_at_unix_ms: now_ms(),
                last_accessed_unix_ms: now_ms(),
                embedding: None,
            };

            match self.remember(&entity) {
                Ok((_, action)) => {
                    if action == "created" {
                        files_created += 1;
                    } else {
                        files_updated += 1;
                    }
                }
                Err(e) => {
                    errors.push(format!("{}: {}", path.display(), e));
                }
            }
        }

        Ok(VaultReport {
            files_created,
            files_updated,
            errors,
            vault_dir: vault_dir.to_string(),
            completed_at_unix_ms: now_ms(),
        })
    }

    /// Return a pre-formatted context block of top entities for session injection.
    pub fn context(
        &self,
        categories: &[String],
        limit: i64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut all_entities = Vec::new();

        // #104: Always-on entities — injected unconditionally, before query results
        let always_on_params = RecallParams {
            always_on: Some(true),
            limit: 50,
            skip_side_effects: true,
            ..RecallParams::default()
        };
        let always_on_entities = self.recall(&always_on_params)?;

        if categories.is_empty() {
            // No filter — get top entities overall (read-only, no side effects)
            let params = RecallParams {
                limit,
                skip_side_effects: true,
                ..RecallParams::default()
            };
            all_entities = self.recall(&params)?;
        } else {
            for cat in categories {
                let params = RecallParams {
                    category: Some(cat.clone()),
                    limit,
                    skip_side_effects: true,
                    ..RecallParams::default()
                };
                let mut batch = self.recall(&params)?;
                all_entities.append(&mut batch);
            }
        }

        // Format as markdown
        let mut ctx = String::from("## Mimir Context\n\n");

        // Always-on entities first, visually distinct
        if !always_on_entities.is_empty() {
            ctx.push_str("### Always On\n\n");
            for entity in &always_on_entities {
                ctx.push_str(&format!(
                    "- [always-on] [{}] **{}** — {} (retrievals: {}, decay: {:.2})\n",
                    entity.category,
                    entity.key,
                    truncate_str(&entity.body_json, 100),
                    entity.retrieval_count,
                    entity.decay_score,
                ));
            }
            ctx.push('\n');
        }

        for entity in &all_entities {
            ctx.push_str(&format!(
                "- [{}] **{}** — {} (retrievals: {}, decay: {:.2})\n",
                entity.category,
                entity.key,
                truncate_str(&entity.body_json, 100),
                entity.retrieval_count,
                entity.decay_score,
            ));
        }
        ctx.push_str(&format!(
            "\n> {} entities recalled\n",
            all_entities.len() + always_on_entities.len()
        ));

        Ok(ctx)
    }

    /// List all distinct categories in the entities table.
    pub fn workspace_list_categories(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT category FROM entities WHERE archived = 0 ORDER BY category",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut cats = Vec::new();
        for row in rows {
            cats.push(row?);
        }
        Ok(cats)
    }

    /// recall_when search: match a trigger context against entities' recall_when fields.
    /// Searches body_json for `"recall_when": ["...trigger..."]` patterns that contain
    /// any substring match against the given context text.
    pub fn recall_when(
        &self,
        context: &str,
        limit: i64,
    ) -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
        let words: Vec<&str> = context
            .split_whitespace()
            .filter(|w| w.len() >= 3)
            .collect();

        if words.is_empty() {
            return Ok(Vec::new());
        }

        // Build LIKE clauses for each significant word in the context
        // matching against body_json which should contain "recall_when" array entries
        let mut like_parts = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        for (i, word) in words.iter().enumerate() {
            let param_idx = i + 1;
            like_parts.push(format!("body_json LIKE ?{}", param_idx));
            params_vec.push(Box::new(format!(
                "%recall_when%{}%",
                word.replace('\'', "''")
            )));
        }

        let sql = format!(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities
             WHERE archived = 0 AND ({})
             ORDER BY decay_score DESC, retrieval_count DESC
             LIMIT ?{}",
            like_parts.join(" OR "),
            params_vec.len() + 1
        );

        let safe_limit = limit.clamp(0, 100);
        params_vec.push(Box::new(safe_limit));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let enc = self.encryption.as_ref();
        let rows = stmt.query_map(param_refs.as_slice(), |row| entity_from_row(row, enc))?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Coherence daemon: auto-groom the memory with promote, decay, link, archive.
    #[allow(unused_assignments)]
    pub fn cohere(
        &self,
        params: &crate::models::CohereParams,
    ) -> Result<crate::models::CohereReport, Box<dyn std::error::Error>> {
        let now = now_ms();
        let mut promoted: i64 = 0;
        let mut decayed: i64 = 0;
        let mut linked: i64 = 0;
        let mut archived: i64 = 0;

        // Count total examined
        let examined: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;

        if params.dry_run {
            return Ok(crate::models::CohereReport {
                promoted: 0,
                decayed: 0,
                linked: 0,
                archived: 0,
                entities_examined: examined,
                dry_run: true,
                completed_at_unix_ms: now,
            });
        }

        // Wrap all mutations in a transaction so partial writes are not left
        // behind if any step fails (cohere runs multiple statements on self.conn).
        self.conn.execute("BEGIN IMMEDIATE", [])?;
        let promote_threshold = if params.promote_threshold > 0 {
            params.promote_threshold
        } else {
            3
        };
        promoted = self.conn.execute(
            "UPDATE entities SET layer = 'working' WHERE layer = 'buffer' AND retrieval_count >= ?1",
            params![promote_threshold],
        )? as i64;

        // 2. Decay: apply Ebbinghaus decay to all non-archived entities
        // Formula: new_score = current_score * 0.95 (gentle decay)
        let decayed_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0 AND decay_score > 0.01",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "UPDATE entities SET decay_score = decay_score * 0.95 WHERE archived = 0 AND decay_score > 0.01",
            [],
        )?;
        decayed = decayed_count;

        // 3. Link: auto-link entities with shared tags within same category
        // Find entities with overlapping tags who aren't already linked
        let mut stmt = self.conn.prepare(
            "SELECT e1.id, e1.category, e1.key, e1.tags, e2.id as e2_id
             FROM entities e1
             JOIN entities e2 ON e1.category = e2.category AND e1.id < e2.id
             WHERE e1.archived = 0 AND e2.archived = 0
             AND e1.tags != '[]' AND e2.tags != '[]'
             LIMIT ?1",
        )?;

        let max_links = params.max_links.clamp(0, 100) as i64;
        let rows = stmt.query_map(params![max_links], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        for row in rows {
            let (_e1_id, cat, key, _tags1_str, e2_id) = row?;

            // Create a simple "related" link from e1 to e2
            if let Err(e) = self.link(&cat, &key, &e2_id, "auto-related") {
                eprintln!("mimir: cohere link error: {}", e);
                continue;
            }
            linked += 1;
        }

        // 4. Archive: entities below decay threshold
        let archive_threshold = if params.archive_threshold > 0.0 {
            params.archive_threshold
        } else {
            0.05
        };
        archived = self.conn.execute(
            "UPDATE entities SET archived = 1, archive_reason = 'auto-archived by coherence daemon (decay < threshold)'
             WHERE archived = 0 AND decay_score < ?1",
            params![archive_threshold],
        )? as i64;

        // Clean FTS5 entries for archived entities
        if archived > 0 {
            self.conn.execute(
                "DELETE FROM entities_fts WHERE rowid IN (SELECT rowid FROM entities WHERE archived = 1)",
                [],
            )?;
        }

        self.conn.execute("COMMIT", [])?;

        Ok(crate::models::CohereReport {
            promoted,
            decayed,
            linked,
            archived,
            entities_examined: examined,
            dry_run: false,
            completed_at_unix_ms: now,
        })
    }
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for i in 0..a.len() {
        let va = a[i] as f64;
        let vb = b[i] as f64;
        dot += va * vb;
        norm_a += va * va;
        norm_b += vb * vb;
    }
    let denom = (norm_a * norm_b).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Reciprocal Rank Fusion: combine dense and sparse result sets.
/// k controls the rank penalty (higher k = less penalty for lower ranks).
pub fn reciprocal_rank_fusion(
    dense_results: &[(crate::models::Entity, f64)],
    sparse_results: &[(crate::models::Entity, f64)],
    k: f64,
    limit: usize,
) -> Vec<(crate::models::Entity, f64)> {
    use std::collections::HashMap;

    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut entities: HashMap<String, crate::models::Entity> = HashMap::new();

    for (rank, (entity, _)) in dense_results.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f64);
        *scores.entry(entity.id.clone()).or_insert(0.0) += rrf;
        entities
            .entry(entity.id.clone())
            .or_insert_with(|| entity.clone());
    }

    for (rank, (entity, _)) in sparse_results.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f64);
        *scores.entry(entity.id.clone()).or_insert(0.0) += rrf;
        entities
            .entry(entity.id.clone())
            .or_insert_with(|| entity.clone());
    }

    let mut fused: Vec<_> = scores
        .into_iter()
        .filter_map(|(id, score)| entities.remove(&id).map(|entity| (entity, score)))
        .collect();

    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(limit);
    fused
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

/// Extract an Entity from a SQLite row, decrypting body_json if encryption is enabled.
fn entity_from_row(
    row: &rusqlite::Row,
    encryption: Option<&EncryptionManager>,
) -> rusqlite::Result<crate::models::Entity> {
    use crate::models::{Entity, MemoryLink};
    let tags_str: String = row.get::<_, String>(6).unwrap_or_else(|_| "[]".to_string());
    let links_str: String = row
        .get::<_, String>(13)
        .unwrap_or_else(|_| "[]".to_string());
    let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
    let links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
    let archived: i32 = row.get(11).unwrap_or(0);
    let verified: i32 = row.get(14).unwrap_or(0);
    let raw_body_json: String = row.get(3)?;

    let body_json = if let Some(enc) = encryption {
        let cat: String = row.get(1)?;
        let k: String = row.get(2)?;
        let aad = format!("{}:{}", cat, k);
        enc.decrypt(&raw_body_json, aad.as_bytes())
            .unwrap_or(raw_body_json) // Fall back to raw if decryption fails (unencrypted DB)
    } else {
        raw_body_json
    };

    Ok(Entity {
        id: row.get(0)?,
        category: row.get(1)?,
        key: row.get(2)?,
        body_json,
        status: row.get(4)?,
        entity_type: row.get(5)?,
        tags,
        decay_score: row.get(7)?,
        retrieval_count: row.get(8)?,
        layer: row.get(9)?,
        topic_path: row.get(10)?,
        archived: archived != 0,
        archive_reason: row.get(12)?,
        links,
        verified: verified != 0,
        source: row.get(15)?,
        always_on: row.get::<_, i32>(19).unwrap_or(0) != 0,
        certainty: row.get::<_, f64>(20).unwrap_or(0.5),
        created_at_unix_ms: row.get(16)?,
        last_accessed_unix_ms: row.get(17)?,
        embedding: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_db() -> (Database, String) {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mimir-test-db-{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap().to_string();
        let db = Database::open(&path_str).expect("open test db");
        (db, path_str)
    }

    fn make_entity(id: &str, category: &str, key: &str, body: &str) -> Entity {
        Entity {
            id: id.to_string(),
            category: category.to_string(),
            key: key.to_string(),
            body_json: body.to_string(),
            status: "active".to_string(),
            entity_type: "insight".to_string(),
            tags: vec![],
            decay_score: 1.0,
            retrieval_count: 0,
            layer: "working".to_string(),
            topic_path: String::new(),
            archived: false,
            archive_reason: String::new(),
            links: vec![],
            verified: false,
            source: "test".to_string(),
            always_on: false,
            certainty: 0.5,
            created_at_unix_ms: now_ms(),
            last_accessed_unix_ms: now_ms(),
            embedding: None,
        }
    }

    #[test]
    fn health_check_on_new_db() {
        let (db, path) = temp_db();
        assert!(db.health_check());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn remember_creates_and_updates_entity() {
        let (db, path) = temp_db();

        let entity = make_entity(
            "mem-test-1",
            "decision",
            "use-postgres",
            r#"{"decision": "Use PostgreSQL"}"#,
        );
        let (id, action) = db.remember(&entity).unwrap();
        assert_eq!(action, "created");
        assert_eq!(id, "mem-test-1");

        // Verify retrieval
        let found = db.get_entity("decision", "use-postgres").unwrap();
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().body_json,
            r#"{"decision": "Use PostgreSQL"}"#
        );

        // Update the same entity (idempotent)
        let mut updated = entity.clone();
        updated.body_json = r#"{"decision": "Use PostgreSQL 16"}"#.to_string();
        let (id2, action2) = db.remember(&updated).unwrap();
        assert_eq!(action2, "updated");
        assert_eq!(id2, "mem-test-1");

        let found2 = db.get_entity("decision", "use-postgres").unwrap();
        assert_eq!(
            found2.unwrap().body_json,
            r#"{"decision": "Use PostgreSQL 16"}"#
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn recall_with_category_filter() {
        let (db, path) = temp_db();

        let e1 = make_entity("e1", "decision", "use-pg", r#"{"d": "pg"}"#);
        let e2 = make_entity("e2", "architecture", "app-stack", r#"{"a": "stack"}"#);
        db.remember(&e1).unwrap();
        db.remember(&e2).unwrap();

        // Recall with category filter
        let params = RecallParams {
            category: Some("decision".to_string()),
            limit: 10,
            ..RecallParams::default()
        };
        let results = db.recall(&params).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, "decision");

        // Recall without filter
        let params2 = RecallParams::default();
        let results2 = db.recall(&params2).unwrap();
        assert_eq!(results2.len(), 2);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn forget_and_archive() {
        let (db, path) = temp_db();

        let e = make_entity("e-f", "decision", "forget-me", "{}");
        db.remember(&e).unwrap();

        let ok = db
            .forget("decision", "forget-me", "no longer relevant")
            .unwrap();
        assert!(ok);

        // Archived entity not in default recall
        let params = RecallParams::default();
        let results = db.recall(&params).unwrap();
        assert!(results.is_empty());

        // But retrievable with include_archived
        let params2 = RecallParams {
            include_archived: true,
            ..RecallParams::default()
        };
        let results2 = db.recall(&params2).unwrap();
        assert_eq!(results2.len(), 1);
        assert!(results2[0].archived);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn prune_cleans_fts_for_only_matching_rows() {
        let (db, path) = temp_db();

        // Two categories. We'll prune only "junk" and assert "keep" stays in FTS.
        let junk = make_entity("j1", "junk", "throwaway", "{\"body\":\"prunable widget\"}");
        let keep = make_entity("k1", "keep", "important", "{\"body\":\"prunable widget\"}");
        db.remember(&junk).unwrap();
        db.remember(&keep).unwrap();

        // helper: is a given entity id present in the FTS index?
        let in_fts = |id: &str| -> bool {
            db.conn
                .query_row(
                    "SELECT COUNT(*) FROM entities_fts
                     WHERE rowid = (SELECT rowid FROM entities WHERE id = ?1)",
                    params![id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                > 0
        };

        assert!(in_fts("j1"));
        assert!(in_fts("k1"));

        let report = db
            .prune(&PruneParams {
                category: Some("junk".to_string()),
                min_decay: None,
                older_than_days: None,
                limit: 0,
                dry_run: false,
            })
            .unwrap();
        assert_eq!(report.archived, 1);

        // The pruned row must be gone from FTS; the unrelated row must remain.
        assert!(!in_fts("j1"), "archived entity should be removed from FTS index");
        assert!(in_fts("k1"), "non-matching entity must NOT be evicted from FTS");

        // Entity rows still exist, just archived.
        let junk_row = db.get_entity("junk", "throwaway").unwrap().unwrap();
        assert!(junk_row.archived);
        let keep_row = db.get_entity("keep", "important").unwrap().unwrap();
        assert!(!keep_row.archived);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn reindex_fts_rebuilds_from_entities() {
        let (db, path) = temp_db();

        let e = make_entity("r1", "decision", "reindex-me", "{\"body\":\"searchable text\"}");
        db.remember(&e).unwrap();

        // Corrupt the index by deleting the FTS row directly (simulating drift).
        db.conn
            .execute("DELETE FROM entities_fts", [])
            .unwrap();
        let count_before: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM entities_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 0);

        // Reindex repairs it.
        let n = db.reindex_fts().unwrap();
        assert_eq!(n, 1);
        let count_after: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM entities_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, 1);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn link_and_unlink() {
        let (db, path) = temp_db();

        let e1 = make_entity("e1", "decision", "use-pg", "{}");
        let e2 = make_entity("e2", "architecture", "db-layer", "{}");
        db.remember(&e1).unwrap();
        db.remember(&e2).unwrap();

        db.link("decision", "use-pg", "e2", "depends_on").unwrap();

        let entity = db.get_entity("decision", "use-pg").unwrap().unwrap();
        assert_eq!(entity.links.len(), 1);
        assert_eq!(entity.links[0].target_id, "e2");

        // Unlink
        db.unlink("decision", "use-pg", "e2").unwrap();
        let entity2 = db.get_entity("decision", "use-pg").unwrap().unwrap();
        assert_eq!(entity2.links.len(), 0);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn journal_and_timeline() {
        let (db, path) = temp_db();

        let event = JournalEvent {
            id: "jrn-1".to_string(),
            event_type: "decision".to_string(),
            evaluated_json: r#"{"options": ["pg", "mysql"]}"#.to_string(),
            acted_json: r#"{"chosen": "pg"}"#.to_string(),
            forward_json: r#"{"next": "migrate"}"#.to_string(),
            category: "decision".to_string(),
            key: "use-pg".to_string(),
            entity_id: "e1".to_string(),
            created_at_unix_ms: now_ms(),
        };
        db.journal(&event).unwrap();

        let timeline = TimelineParams::default();
        let events = db.timeline(&timeline).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "decision");
        assert!(events[0].acted_json.contains("pg"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn state_set_get_expire() {
        let (db, path) = temp_db();

        let entry = StateEntry {
            key: "test-key".to_string(),
            value_json: r#"{"value": 42}"#.to_string(),
            expires_at_unix_ms: None,
            created_at_unix_ms: now_ms(),
        };
        db.state_set(&entry).unwrap();

        let got = db.state_get("test-key").unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().value_json, r#"{"value": 42}"#);

        // Set with TTL in the past
        let expired = StateEntry {
            key: "expired-key".to_string(),
            value_json: r#"{"value": 1}"#.to_string(),
            expires_at_unix_ms: Some(now_ms() - 1000),
            created_at_unix_ms: now_ms(),
        };
        db.state_set(&expired).unwrap();

        let got2 = db.state_get("expired-key").unwrap();
        assert!(got2.is_none()); // Should be auto-deleted

        // Delete
        assert!(db.state_delete("test-key").unwrap());
        assert!(db.state_get("test-key").unwrap().is_none());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn compact_archives_below_threshold() {
        let (db, path) = temp_db();

        let mut e1 = make_entity("e-a", "test", "keep", r#"{"name":"entity A"}"#);
        e1.decay_score = 0.9;
        let mut e2 = make_entity(
            "e-b",
            "test",
            "archive",
            r#"{"name":"entity B is different"}"#,
        );
        e2.decay_score = 0.1;
        db.remember(&e1).unwrap();
        db.remember(&e2).unwrap();

        let report = db.compact(0.3, false).unwrap();
        assert_eq!(report.entities_examined, 2);
        assert_eq!(report.entities_archived, 1);

        let params = RecallParams::default();
        let results = db.recall(&params).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "keep");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn query_expansion_with_stemming() {
        let (db, path) = temp_db();

        // Create entities with "configuration" and "configured" in body.
        // Neither contains "configure" as a substring, so LIKE won't help.
        // This is exactly why stemming-based expansion is needed.
        let e1 = make_entity(
            "e1",
            "insight",
            "config-file",
            r#"{"content": "The server configuration lives in /etc/config"}"#,
        );
        let e2 = make_entity(
            "e2",
            "insight",
            "configured-host",
            r#"{"content": "The host was configured via Ansible playbook"}"#,
        );
        let e3 = make_entity(
            "e3",
            "insight",
            "unrelated",
            r#"{"content": "Coffee is best at 93\u00b0C"}"#,
        );
        db.remember(&e1).unwrap();
        db.remember(&e2).unwrap();
        db.remember(&e3).unwrap();

        // Baseline: search for "configure" — LIKE won't find "configuration"
        let params = RecallParams {
            query: "configure".to_string(),
            limit: 10,
            ..RecallParams::default()
        };
        let results = db.recall(&params).unwrap();
        assert!(
            results.iter().any(|e| e.key == "configured-host"),
            "configured-host should match: 'configure' is a substring of 'configured'"
        );
        assert!(
            !results.iter().any(|e| e.key == "config-file"),
            "config-file should NOT match: 'configuration' does not contain 'configure'"
        );
        assert!(
            !results.iter().any(|e| e.key == "unrelated"),
            "unrelated should not match"
        );

        // With expansion: stemming reduces both "configure" and "configuration"
        // to "configur", so searching for the stem should find both.
        let params2 = RecallParams {
            query: "configur".to_string(),
            limit: 10,
            ..RecallParams::default()
        };
        let results2 = db.recall(&params2).unwrap();
        assert!(
            results2.iter().any(|e| e.key == "config-file"),
            "stemmed search should find 'configuration'"
        );
        assert!(
            results2.iter().any(|e| e.key == "configured-host"),
            "stemmed search should find 'configured'"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn encryption_roundtrip() {
        use crate::encryption::EncryptionManager;
        use std::io::Write;

        let (mut db, path) = temp_db();

        // Create a temp key file with a generated key
        let key = EncryptionManager::generate_key();
        let key_dir = std::env::temp_dir();
        let key_path = key_dir.join(format!("mimir-test-key-{}.key", uuid::Uuid::new_v4()));
        let key_path_str = key_path.to_str().unwrap().to_string();

        let mut f = std::fs::File::create(&key_path).unwrap();
        f.write_all(key.as_bytes()).unwrap();
        drop(f);

        // Enable encryption
        db.set_encryption(&key_path_str).unwrap();
        assert!(db.encryption_enabled());

        // Store an entity — body should be encrypted at rest
        let entity = make_entity(
            "e-enc",
            "insight",
            "secret-note",
            r#"{"content": "top secret data"}"#,
        );
        db.remember(&entity).unwrap();

        // Retrieve and verify round-trip
        let found = db.get_entity("insight", "secret-note").unwrap().unwrap();
        assert_eq!(found.body_json, r#"{"content": "top secret data"}"#);

        // Verify the raw DB column is encrypted (not plaintext)
        let raw_body: String = db
            .conn
            .query_row(
                "SELECT body_json FROM entities WHERE category = ?1 AND key = ?2",
                rusqlite::params!["insight", "secret-note"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !raw_body.contains("top secret"),
            "Raw DB column should be encrypted, got: {}",
            &raw_body[..raw_body.len().min(60)]
        );

        // Test that a Database without encryption sees the garbled text
        let (db2, path2) = temp_db();
        // Copy the raw encrypted row into db2 (without setting encryption)
        db2.conn.execute(
            "INSERT INTO entities (id, category, key, body_json, status, type, tags, decay_score, retrieval_count, layer, topic_path, archived, archive_reason, links, verified, source, created_at_unix_ms, last_accessed_unix_ms) VALUES (?1, ?2, ?3, ?4, 'active', 'insight', '[]', 1.0, 0, 'working', '', 0, '', '[]', 0, 'agent', 0, 0)",
            rusqlite::params!["e-enc", "insight", "secret-note", raw_body],
        ).unwrap();

        // Without encryption, the body_json should be the raw encrypted blob
        let found2 = db2.get_entity("insight", "secret-note").unwrap().unwrap();
        assert_ne!(found2.body_json, r#"{"content": "top secret data"}"#);

        // Cleanup
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&path2);
        let _ = fs::remove_file(&key_path);
    }

    #[test]
    fn rrf_hybrid_preserves_dense_only_results() {
        // Regression test for #125: dense-only (pure semantic) hits
        // should survive RRF fusion even when the sparse result set
        // has no overlapping documents.
        let dense_only = make_entity(
            "dense-1",
            "insight",
            "only-in-dense",
            r#"{"note": "semantic match"}"#,
        );
        let sparse_only = make_entity(
            "sparse-1",
            "insight",
            "only-in-sparse",
            r#"{"note": "keyword match"}"#,
        );
        let both = make_entity("both-1", "insight", "in-both", r#"{"note": "both"}"#);

        let dense_results = vec![(dense_only, 0.95), (both.clone(), 0.80)];
        let sparse_results = vec![(sparse_only, 1.0), (both, 0.9)];

        let fused = crate::db::reciprocal_rank_fusion(&dense_results, &sparse_results, 60.0, 10);
        let fused_ids: Vec<&str> = fused.iter().map(|(e, _)| e.id.as_str()).collect();

        assert!(
            fused_ids.contains(&"dense-1"),
            "dense-only entity was dropped from hybrid results (regression #125)"
        );
        assert!(
            fused_ids.contains(&"sparse-1"),
            "sparse-only entity was dropped from hybrid results"
        );
        assert!(
            fused_ids.contains(&"both-1"),
            "overlapping entity was dropped from hybrid results"
        );
    }
}

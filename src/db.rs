use rusqlite::params;
use r2d2_sqlite::SqliteConnectionManager;
use std::time::{SystemTime, UNIX_EPOCH};

/// A connection checked out from the pool. Derefs to `rusqlite::Connection`, so
/// every existing `conn.prepare/execute/query_row/...` call works unchanged once
/// a method binds `let conn = self.conn()?;`. (#210)
type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

use crate::connectors::Connector;
use crate::encryption::EncryptionManager;
use crate::models::{
    AskParams, AskResult, AskSource, CompactReport, DecayReport, EmbedParams, Entity, GraphEdge,
    GraphNode, IngestParams, JournalEvent, MemoryLink, PruneParams, PruneReport, PurgeReport, RecallParams,
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

/// Simple LRU cache for embedding vectors. Stores up to `capacity` entries;
/// when full, the oldest entry is evicted. Avoids re-computing embeddings for
/// recently seen text.
struct EmbeddingCache {
    entries: Vec<(String, Vec<f32>)>,
    capacity: usize,
}

impl EmbeddingCache {
    fn new(capacity: usize) -> Self {
        EmbeddingCache { entries: Vec::with_capacity(capacity), capacity }
    }
    fn get(&mut self, text: &str) -> Option<&Vec<f32>> {
        if let Some(pos) = self.entries.iter().position(|(t, _)| t.as_str() == text) {
            // Move to front (MRU)
            let entry = self.entries.remove(pos);
            self.entries.insert(0, entry);
            Some(&self.entries[0].1)
        } else {
            None
        }
    }
    fn put(&mut self, text: String, vec: Vec<f32>) {
        if let Some(pos) = self.entries.iter().position(|(t, _)| t.as_str() == text) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity {
            self.entries.pop();
        }
        self.entries.insert(0, (text, vec));
    }
}

pub struct Database {
    pool: r2d2::Pool<SqliteConnectionManager>,
    db_path: String,
    encryption: Option<EncryptionManager>,
    llm_config: LlmConfig,
    #[allow(dead_code)]
    embedding_config: crate::embedding::EmbeddingConfig,
    embedding_cache: std::sync::Mutex<EmbeddingCache>,
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
        // #210: a connection pool lets concurrent HTTP/SSE requests read in
        // parallel under WAL instead of serializing on one Mutex<Connection>
        // (rusqlite Connection is !Sync). The PRAGMAs must be applied to EVERY
        // pooled connection (not once), so set them on checkout via with_init.
        // synchronous=NORMAL is durable under WAL (only risks the last txn on an
        // OS crash) and avoids an fsync per commit; cache/mmap/temp_store reduce
        // cold-scan cost. (#208)
        //
        // Pool size and busy_timeout are tunable via env so operators can match
        // the pool to their workload and so the concurrent-client load test can
        // sweep them (#223). Defaults preserve the prior hard-coded values
        // (max_size=16, busy_timeout=5000ms).
        let max_size: u32 = std::env::var("MIMIR_POOL_MAX_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(16);
        let busy_timeout_ms: u64 = std::env::var("MIMIR_BUSY_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);
        let manager = SqliteConnectionManager::file(path).with_init(move |c| {
            c.execute_batch(&format!(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=1000; \
                 PRAGMA foreign_keys=ON; PRAGMA busy_timeout={busy_timeout_ms}; \
                 PRAGMA synchronous=NORMAL; PRAGMA cache_size=-8000; \
                 PRAGMA mmap_size=268435456; PRAGMA temp_store=MEMORY;",
            ))
        });
        let pool = r2d2::Pool::builder().max_size(max_size).build(manager)?;

        // Initialize schema once if this is a new database.
        let setup_conn = pool.get()?;
        schema::initialize_schema(&setup_conn)?;
        drop(setup_conn);

        Ok(Database {
            pool,
            db_path: path.to_string(),
            encryption: None,
            llm_config: LlmConfig::default(),
            embedding_config: crate::embedding::EmbeddingConfig::default(),
            embedding_cache: std::sync::Mutex::new(EmbeddingCache::new(256)),
            connectors: Vec::new(),
        })
    }

    /// Check out a pooled connection. Each DB method binds one of these and uses
    /// it for the duration of the call (so a method's statements — including any
    /// transaction — share a single connection). (#210)
    fn conn(&self) -> Result<PooledConn, Box<dyn std::error::Error>> {
        Ok(self.pool.get()?)
    }

    /// Simple health check — verify the DB responds.
    pub fn health_check(&self) -> bool {
        match self.conn() {
            Ok(conn) => conn.query_row("SELECT 1", [], |_| Ok(())).is_ok(),
            Err(_) => false,
        }
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
    /// Configure local embedding backend with path to ONNX model.
    /// Enables local embeddings via the bundled `ort` + `tokenizers` backend.
    pub fn set_embedding_model(&mut self, model_path: &str) {
        self.embedding_config = crate::embedding::EmbeddingConfig::with_model_path(
            std::path::PathBuf::from(model_path),
        );
        self.llm_config.embedding_endpoint = None; // prefer local ONNX over remote
    }

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
                            workspace_hash: String::new(),
                            agent_id: String::new(),
                                        visibility: "workspace".to_string(),
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
        let conn = self.conn()?;
        let blob: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
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
        let conn = self.conn()?;
        // Batch mode: embed all entities in a category that lack embeddings
        if let Some(ref cat) = params.batch_category {
            let mut stmt = conn.prepare(
                "SELECT id, body_json FROM entities WHERE category = ?1 AND archived = 0 AND embedding IS NULL LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![cat, params.batch_limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;

            let mut embedded = 0usize;
            let mut errors = Vec::new();
            for row in rows {
                let (id, body) = row?;
                match self.generate_embedding_with_fallback(&body) {
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
        let embedding = self.generate_embedding_with_fallback(text)?;
        self.store_embedding(&entity.id, &embedding)?;

        Ok(serde_json::json!({
            "embedded": 1,
            "id": entity.id,
            "dimensions": embedding.len(),
        }))
    }

    /// Generate an embedding vector, falling back through available backends:
    /// 1. Local ONNX model (if embedding_config.enabled)
    /// 2. Python onnxruntime subprocess (if available)
    /// 3. Ollama /api/embed or OpenAI /v1/embeddings (if llm_config set)
    fn generate_embedding_with_fallback(
        &self,
        text: &str,
    ) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        // 0. Hit the in-memory cache
        if let Ok(mut cache) = self.embedding_cache.lock() {
            if let Some(vec) = cache.get(text) {
                return Ok(vec.clone());
            }
        }

        // 1. Local ONNX — if enabled and either the model is compiled in (#237)
        //    or a model file exists on disk.
        if self.embedding_config.enabled
            && (self.embedding_config.bundled || self.embedding_config.model_path.exists())
        {
            match crate::embedding::generate_embedding(&self.embedding_config, text) {
                Ok(vec) => {
                    // Cache successful embedding
                    if let Ok(mut cache) = self.embedding_cache.lock() {
                        cache.put(text.to_string(), vec.clone());
                    }
                    return Ok(vec);
                }
                Err(e) => eprintln!(
                    "mimir: local ONNX embedding failed ({}), falling back...",
                    e
                ),
            }
        }

        // 2. Remote endpoint (Ollama or OpenAI-compatible)
        if self.llm_config.enabled {
            return self.call_ollama_embed(text);
        }

        Err("No embedding backend available. Set --embedding-model to a local ONNX model, or --llm-endpoint for remote.".into())
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
        let conn = self.conn()?;
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
        let examined: usize = conn
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
            let mut stmt = conn.prepare(&select_rowids_sql)?;
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
        let tx = conn.unchecked_transaction()?;
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
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
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
        let conn = self.conn()?;
        let max_scan = 50_000; // safety ceiling — databases beyond this should use HNSW
        let dim = query_vec.len();

        // Phase 1 (#209): lightweight scan — read only id + embedding for scoring.
        // The old query hydrated EVERY candidate (decrypt body, parse tags/links)
        // up to max_scan just to score and then keep top-k. Defer full hydration
        // to the surviving top-k in phase 3.
        let mut stmt = conn.prepare(&format!(
            "SELECT id, embedding FROM entities \
             WHERE archived = 0 AND embedding IS NOT NULL LIMIT {}",
            max_scan
        ))?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let emb_blob: Vec<u8> = row.get(1)?;
            let emb: Vec<f32> = emb_blob
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            Ok((id, emb))
        })?;
        let candidates: Vec<(String, Vec<f32>)> = rows
            .filter_map(|r| r.ok())
            .filter(|(_, emb)| emb.len() == dim)
            .collect();

        // Phase 2: score by cosine similarity, keep the top `limit` ids.
        let mut scored_ids: Vec<(String, f64)>;
        #[cfg(feature = "bundled-embeddings")]
        {
            // Batched cosine similarity using SIMD-accelerated ndarray ops.
            scored_ids = Vec::with_capacity(candidates.len());
            if !candidates.is_empty() {
                let n = candidates.len();
                let mut all_embs: Vec<f32> = Vec::with_capacity(n * dim);
                for (_, emb) in &candidates {
                    all_embs.extend_from_slice(emb);
                }
                let q = ndarray::Array1::from_vec(query_vec.to_vec());
                let embs = ndarray::Array2::from_shape_vec((n, dim), all_embs)
                    .unwrap_or_else(|_| ndarray::Array2::zeros((n, dim)));
                let q_norm = q.iter().map(|v| v * v).sum::<f32>().sqrt();
                let emb_norms = embs.mapv(|v| v * v).sum_axis(ndarray::Axis(1)).mapv(f32::sqrt);
                let dots = embs.dot(&q);
                for (i, (id, _)) in candidates.into_iter().enumerate() {
                    let denom = q_norm * emb_norms[i];
                    let sim = if denom > 0.0 { dots[i] as f64 / denom as f64 } else { 0.0 };
                    scored_ids.push((id, sim));
                }
            }
        }
        #[cfg(not(feature = "bundled-embeddings"))]
        {
            // Row-by-row fallback (no ndarray available).
            scored_ids = candidates
                .into_iter()
                .map(|(id, emb)| {
                    let sim = cosine_similarity(query_vec, &emb);
                    (id, sim)
                })
                .collect();
        }
        scored_ids.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored_ids.truncate(limit);
        if scored_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Phase 3 (#209): hydrate only the surviving top-k rows, then return them
        // in score order. This is the single place that pays the decrypt/parse
        // cost — for `limit` rows instead of up to `max_scan`.
        let placeholders = vec!["?"; scored_ids.len()].join(",");
        let hydrate_sql = format!(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
             FROM entities WHERE id IN ({})",
            placeholders
        );
        let enc = self.encryption.as_ref();
        let mut hstmt = conn.prepare(&hydrate_sql)?;
        let id_refs: Vec<&dyn rusqlite::types::ToSql> = scored_ids
            .iter()
            .map(|(id, _)| id as &dyn rusqlite::types::ToSql)
            .collect();
        let hydrated = hstmt.query_map(id_refs.as_slice(), |row| entity_from_row(row, enc))?;
        let mut by_id: std::collections::HashMap<String, Entity> = std::collections::HashMap::new();
        for e in hydrated {
            let e = e?;
            by_id.insert(e.id.clone(), e);
        }
        // Emit in the score order computed above; skip any id that vanished
        // between the scan and the hydrate (deleted concurrently).
        let result: Vec<(Entity, f64)> = scored_ids
            .into_iter()
            .filter_map(|(id, sim)| by_id.remove(&id).map(|e| (e, sim)))
            .collect();
        Ok(result)
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

    /// Apply recall side-effects (retrieval-count bump, recency, decay boost,
    /// layer promotion) to a set of entities in a single batched UPDATE.
    ///
    /// This is the SQL mirror of the per-entity `boost_decay` / `compute_layer`
    /// logic, hoisted out of the recall row loop so the hottest read path issues
    /// one write instead of one-per-row (#207). The `layer` CASE uses the
    /// incremented count so it matches `compute_layer(retrieval_count + 1)`.
    ///
    /// A single `execute` is atomic, so no explicit transaction is needed. The id
    /// count is bounded by the recall LIMIT clamp (≤1000), well under SQLite's
    /// bound-variable ceiling.
    pub fn apply_recall_side_effects(
        &self,
        ids: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "UPDATE entities SET \
                retrieval_count = retrieval_count + 1, \
                last_accessed_unix_ms = ?, \
                decay_score = MIN(1.0, decay_score + {boost}), \
                layer = CASE \
                    WHEN retrieval_count + 1 >= {core} THEN 'core' \
                    WHEN retrieval_count + 1 >= {working} THEN 'working' \
                    ELSE 'buffer' END \
             WHERE id IN ({placeholders})",
            boost = Self::DECAY_BOOST,
            core = Self::CORE_THRESHOLD,
            working = Self::WORKING_THRESHOLD,
        );

        let now = now_ms();
        let mut param_values: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(ids.len() + 1);
        param_values.push(&now);
        for id in ids {
            param_values.push(id);
        }
        conn.execute(&sql, param_values.as_slice())?;
        Ok(())
    }

    /// Recalculate decay scores for all non-archived entities.
    /// Called periodically or via mimir_decay tool.
    pub fn decay_tick(&self) -> Result<DecayReport, Box<dyn std::error::Error>> {
        self.decay_tick_with_limit(None)
    }

    /// Like decay_tick but with an optional max entities to process per call.
    /// Processes entities in batches of 1000, each in its own transaction,
    /// to avoid holding a single large transaction at 100K+ scale.
    fn decay_tick_with_limit(
        &self,
        max_entities: Option<i64>,
    ) -> Result<DecayReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let now = now_ms();
        let total: i64 = conn.query_row(
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
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;

        let mut updated = 0i64;
        let mut auto_archived = 0i64;
        let mut batch: Vec<(String, i64)> = Vec::with_capacity(1000);
        let now_val = now;

        // Helper: flush the current batch in a transaction.
        let flush_batch = |batch: &mut Vec<(String, i64)>,
                            updated: &mut i64,
                            auto_archived: &mut i64|
         -> Result<(), Box<dyn std::error::Error>> {
            if batch.is_empty() {
                return Ok(());
            }
            let tx = conn.unchecked_transaction()?;
            for (id, last_access) in batch.drain(..) {
                let new_decay = Self::compute_decay(last_access, now_val);
                tx.execute(
                    "UPDATE entities SET decay_score = ?1 WHERE id = ?2",
                    params![new_decay, &id],
                )?;
                *updated += 1;
                // Auto-archive entities that have fully decayed (decay < 0.05)
                if new_decay < 0.05 {
                    tx.execute(
                        "UPDATE entities SET archived = 1, archive_reason = 'decay threshold' WHERE id = ?1 AND archived = 0",
                        params![&id],
                    )?;
                    *auto_archived += 1;
                    // Clean FTS5 index for auto-archived entity
                    let _ = tx.execute(
                        "DELETE FROM entities_fts WHERE rowid = (SELECT rowid FROM entities WHERE id = ?1)",
                        params![&id],
                    );
                }
            }
            tx.commit()?;
            Ok(())
        };

        for row in rows {
            let (id, last_access) = row?;
            batch.push((id, last_access));
            if batch.len() >= 1000 {
                flush_batch(&mut batch, &mut updated, &mut auto_archived)?;
            }
        }
        // Flush final partial batch
        flush_batch(&mut batch, &mut updated, &mut auto_archived)?;

        Ok(DecayReport {
            entities_checked: total,
            entities_updated: updated,
            auto_archived,
            completed_at_unix_ms: now,
        })
    }

    // ─── Entities ────────────────────────────────────────────────

    /// Character-trigram set of a string (fast, language-agnostic).
    fn trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() < 3 {
            return std::collections::HashSet::new();
        }
        chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
    }

    /// Jaccard overlap of two precomputed trigram sets (0.0–1.0).
    fn trigram_overlap(
        ta: &std::collections::HashSet<[char; 3]>,
        tb: &std::collections::HashSet<[char; 3]>,
    ) -> f64 {
        if ta.is_empty() || tb.is_empty() {
            return 0.0;
        }
        let intersection = ta.intersection(tb).count();
        let union = ta.len() + tb.len() - intersection;
        if union == 0 {
            return 0.0;
        }
        intersection as f64 / union as f64
    }

    /// Compute trigram overlap similarity between two strings (0.0–1.0).
    /// Uses character trigrams for fast, language-agnostic comparison.
    fn trigram_similarity(a: &str, b: &str) -> f64 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }
        if a == b {
            return 1.0;
        }
        Self::trigram_overlap(&Self::trigrams(a), &Self::trigrams(b))
    }

    /// Check for near-duplicate entities in the same category.
    /// Returns Some(existing_entity_id) if similarity > threshold.
    fn find_near_duplicate(
        &self,
        category: &str,
        body_json: &str,
        threshold: f64,
        fts_prefilter: bool,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        // Precompute the new body's trigram set ONCE rather than rebuilding it
        // for every candidate inside trigram_similarity (it was reconstructed on
        // each comparison — #209). The exact-match / empty cases below preserve
        // trigram_similarity's prior semantics exactly.
        if body_json.is_empty() {
            return Ok(None);
        }
        let target = Self::trigrams(body_json);

        let conn = self.conn()?;

        // Opt-in FTS candidate prefilter (#228), gated by the caller. The exact
        // cost of dedup on bulk import is the exhaustive trigram comparison
        // against every non-archived row in the category (O(M*N) over an import of
        // M rows). When enabled, only same-category rows that share at least one
        // FTS token with the new body are compared, which collapses that cost for
        // categories with diverse bodies. This is a HEURISTIC, not lossless: a
        // near-duplicate that shares no FTS token with the new body (e.g. one
        // differing only in punctuation the tokenizer drops) can slip through and
        // be stored as a separate entity. The default full scan stays exact.
        // entities_fts holds the plaintext body even under encryption, so the
        // prefilter works on encrypted DBs too.
        let mut match_query = String::new();
        if fts_prefilter {
            // OR the body's distinct tokens into a single MATCH expression. Cap
            // the term count so a very large body can't build a pathological FTS
            // query; the cap only narrows candidates further (still a prefilter).
            let mut seen = std::collections::HashSet::new();
            let terms: Vec<String> = body_json
                .split_whitespace()
                .filter(|w| seen.insert(*w))
                .take(64)
                .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
                .collect();
            if !terms.is_empty() {
                match_query = terms.join(" OR ");
            }
        }

        // A non-capturing row mapper, shared so both query branches have the same
        // closure type and can be assigned to one `rows` binding.
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String)> {
            Ok((r.get(0)?, r.get(1)?))
        };
        let mut stmt = if match_query.is_empty() {
            conn.prepare("SELECT id, body_json FROM entities WHERE category = ?1 AND archived = 0")?
        } else {
            conn.prepare(
                "SELECT id, body_json FROM entities \
                 WHERE category = ?1 AND archived = 0 \
                   AND rowid IN (SELECT rowid FROM entities_fts WHERE entities_fts MATCH ?2)",
            )?
        };
        let rows = if match_query.is_empty() {
            stmt.query_map(params![category], map_row)?
        } else {
            stmt.query_map(params![category, match_query], map_row)?
        };

        let target_len = target.len() as f64;
        for row in rows {
            let (id, existing_body) = row?;
            let sim = if existing_body.is_empty() {
                0.0
            } else if existing_body == body_json {
                1.0
            } else {
                // Lossless length prefilter (#228). A candidate body of N chars
                // yields at most N-2 trigrams, and Jaccard similarity is bounded
                // by min(a,b)/max(a,b) <= (N-2)/a. If that ceiling is below the
                // threshold the candidate can never qualify, so skip building its
                // trigram set (the costly part). This prunes only candidates whose
                // best possible score is sub-threshold, so it never changes which
                // entities are deduped — exact matches share the target's length
                // and so always clear the filter.
                let cand_max_trigrams = existing_body.chars().count().saturating_sub(2);
                if (cand_max_trigrams as f64) < threshold * target_len {
                    continue;
                }
                Self::trigram_overlap(&target, &Self::trigrams(&existing_body))
            };
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
        let conn = self.conn()?;
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

        let existing_id: Option<String> = conn
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
            let old_decay: f64 = conn
                .query_row(
                    "SELECT decay_score FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(1.0);
            let old_count: i64 = conn
                .query_row(
                    "SELECT retrieval_count FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let boosted = Self::boost_decay(old_decay);
            let new_layer = Self::compute_layer(old_count + 1);

            // Bi-temporal supersession (v2.4.0): if this remember() changes the
            // stored content, snapshot the prior version into entity_history
            // before overwriting, so history is kept for as-of queries. An
            // identical re-assertion is NOT a new version (no spurious history).
            // Compare on plaintext — GCM ciphertext differs every call.
            let old_raw_body: String = conn
                .query_row(
                    "SELECT body_json FROM entities WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let old_plain_body = if let Some(ref enc) = self.encryption {
                let aad = format!("{}:{}", entity.category, entity.key);
                enc.decrypt(&old_raw_body, aad.as_bytes())
                    .unwrap_or_else(|_| old_raw_body.clone())
            } else {
                old_raw_body.clone()
            };
            let content_changed = old_plain_body != entity.body_json;
            let history_id = format!(
                "hist-{}",
                uuid::Uuid::new_v4().to_string().replace('-', "")[..16].to_string()
            );

            // M-1: wrap entity UPDATE + FTS UPDATE in a transaction
            let tx = conn.unchecked_transaction()?;

            // Snapshot the OLD row BEFORE the UPDATE overwrites it. invalidated_at
            // = now (transaction time it was retired); superseded_by = the live id
            // that replaces it. Other columns (incl. the prior recorded_at) copied
            // verbatim, so the version was live during [recorded_at, invalidated_at).
            if content_changed {
                tx.execute(
                    "INSERT INTO entity_history
                     (history_id, id, category, key, body_json, status, type, tags,
                      decay_score, retrieval_count, layer, topic_path, archived,
                      archive_reason, links, verified, source, always_on, certainty,
                      workspace_hash, agent_id, visibility, valid_from_unix_ms,
                      valid_to_unix_ms, recorded_at_unix_ms, invalidated_at_unix_ms,
                      supersedes, superseded_by, created_at_unix_ms, last_accessed_unix_ms)
                     SELECT ?1, id, category, key, body_json, status, type, tags,
                      decay_score, retrieval_count, layer, topic_path, archived,
                      archive_reason, links, verified, source, always_on, certainty,
                      workspace_hash, agent_id, visibility, valid_from_unix_ms,
                      valid_to_unix_ms, recorded_at_unix_ms, ?2,
                      supersedes, ?3, created_at_unix_ms, last_accessed_unix_ms
                     FROM entities WHERE id = ?3",
                    params![history_id, now, id],
                )?;
            }

            tx.execute(
                "UPDATE entities SET
                    body_json = ?1, status = ?2, type = ?3, tags = ?4,
                    decay_score = ?5, layer = ?6, topic_path = ?7,
                    archived = ?8, archive_reason = ?9, links = ?10,
                    verified = ?11, source = ?12, last_accessed_unix_ms = ?13,
                    always_on = ?14, certainty = ?15, workspace_hash = ?16, agent_id = ?17, visibility = ?18,
                    retrieval_count = retrieval_count + 1
                 WHERE id = ?19",
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
                    entity.workspace_hash,
                    entity.agent_id,
                    entity.visibility,
                    id,
                ],
            )?;

            // Stamp the now-current version's transaction time and link it back to
            // the snapshot it replaced. Only on a real content change, so an
            // identical re-assertion leaves recorded_at/supersedes untouched.
            if content_changed {
                tx.execute(
                    "UPDATE entities SET recorded_at_unix_ms = ?1, supersedes = ?2 WHERE id = ?3",
                    params![now, history_id, id],
                )?;
            }

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
            // MIMIR_DEDUP_FTS_PREFILTER (default off) trades exact dedup for an
            // FTS candidate prefilter that collapses the O(M*N) bulk-import cost.
            // See find_near_duplicate for the lossiness tradeoff. (#228)
            let fts_prefilter = std::env::var("MIMIR_DEDUP_FTS_PREFILTER")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if let Ok(Some(dup_id)) = self.find_near_duplicate(
                &entity.category,
                &entity.body_json,
                dup_threshold,
                fts_prefilter,
            ) {
                // Near-duplicate found — bump its importance instead of creating new
                let _ = conn.execute(
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
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO entities
                 (id, category, key, body_json, status, type, tags,
                  decay_score, retrieval_count, layer, topic_path,
                  archived, archive_reason, links, verified, source,
                  always_on, certainty, created_at_unix_ms, last_accessed_unix_ms,
                  workspace_hash, agent_id, visibility, recorded_at_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                         ?8, ?9, ?10, ?11,
                         ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
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
                    entity.workspace_hash,
                    entity.agent_id,
                    entity.visibility,
                    // Transaction time: a new fact's recorded_at is its creation time.
                    entity.created_at_unix_ms,
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
            // Use the caller-supplied query vector, or embed the query text. An
            // empty query has nothing to embed and falls through to FTS5; a
            // non-empty query with no embedding backend surfaces a clear error
            // rather than silently degrading to keyword search.
            let embedded;
            let query_vec: Option<&[f32]> = match params.embedding {
                Some(ref v) => Some(v.as_slice()),
                None if !params.query.trim().is_empty() => {
                    embedded = self.generate_embedding_with_fallback(&params.query)?;
                    Some(embedded.as_slice())
                }
                None => None,
            };

            if let Some(query_vec) = query_vec {
                let dense_results = self.dense_search(query_vec, params.limit as usize)?;

                if params.mode == crate::models::SearchMode::Dense {
                    return Ok(dense_results.into_iter().map(|(e, _)| e).collect());
                }

                // Hybrid: fuse the dense vectors with a read-only, BM25-ranked,
                // stopword-filtered keyword arm. The keyword arm is fused at a
                // reduced weight (and dropped entirely when it finds nothing) so
                // it cannot dilute a strong dense ranking (#247).
                //
                // Both arms and the fusion are read-only: like `Dense`, the
                // semantic recall path issues no access-state writes, so repeated
                // hybrid recalls are idempotent (#247). Nothing in dense/BM25
                // ordering or the id tie-break depends on access state, so the
                // result is byte-stable run-to-run.
                let dense_scored = dense_results;
                let sparse_scored = self.fts5_bm25_search(params)?;
                let sparse_weight = crate::db::sparse_arm_weight(sparse_scored.len());
                let fused = crate::db::reciprocal_rank_fusion(
                    &dense_scored,
                    &sparse_scored,
                    60.0,
                    params.limit as usize,
                    sparse_weight,
                    params.recency_half_life_secs,
                    now_ms(),
                );
                return Ok(fused.into_iter().map(|(e, _)| e).collect());
            }
            // Empty query: nothing to embed, fall through to FTS5
        }

        self.fts5_search(params)
    }

    /// Core FTS5 + LIKE keyword search (extracted for reuse by recall and hybrid).
    fn fts5_search(
        &self,
        params: &RecallParams,
    ) -> Result<Vec<Entity>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
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
                // FTS5 escaping: double any double-quotes within the term.
                let escape_fts = |s: &str| -> String { s.replace('"', "\"\"") };

                if params.include_archived {
                    // Archived entities are not in the FTS5 index, so this
                    // opt-in path still scans body_json with a LIKE substring
                    // match. It is the only path that can reach archived rows.
                    let mut like_clauses = Vec::new();
                    for word in &words {
                        let idx = param_values.len() + 1;
                        like_clauses.push(format!("body_json LIKE ?{}", idx));
                        param_values.push(Box::new(format!("%{}%", word.replace('\'', "''"))));
                    }
                    conditions.push(like_clauses.join(" OR "));
                } else {
                    // Prefix-match each term against the FTS5 index. The trailing
                    // `*` makes "auth" still find "authentication" while keeping
                    // the lookup on the index; the previous `OR body_json LIKE
                    // '%term%'` forced a full body_json scan on every recall,
                    // defeating FTS5. (Pure-infix matches like "oauth" for the
                    // query "auth" are no longer returned; prefix matching covers
                    // the common case without scanning the table.)
                    let fts_query = words
                        .iter()
                        .map(|w| {
                            let escaped = escape_fts(w);
                            if escaped.is_empty() {
                                "\"\"".to_string()
                            } else {
                                format!("\"{}\"*", escaped)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" OR ");
                    let idx = param_values.len() + 1;
                    param_values.push(Box::new(fts_query));
                    conditions.push(format!(
                        "rowid IN (SELECT rowid FROM entities_fts WHERE entities_fts MATCH ?{})",
                        idx
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

        // Filter by workspace_hash (v1.2.0 scoping). When set, only entities
        // in the matching workspace are visible.
        if let Some(ref ws) = params.workspace_hash {
            conditions.push(format!("workspace_hash = ?{}", param_values.len() + 1));
            param_values.push(Box::new(ws.clone()));
        }

        // Filter by agent_id (v1.2.0 attribution). When set, only entities
        // written by the specified agent are visible.
        if let Some(ref aid) = params.agent_id {
            conditions.push(format!("agent_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(aid.clone()));
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
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
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

        let mut stmt = conn.prepare(&sql)?;
        let enc = self.encryption.as_ref();
        let rows = stmt.query_map(param_refs.as_slice(), |row| entity_from_row(row, enc))?;

        let mut items = Vec::new();
        // #207: collect matched ids and apply retrieval-count/recency/decay/layer
        // side-effects in one batched UPDATE after the loop, instead of one
        // write per returned row on this read-mostly hot path.
        let mut hit_ids: Vec<String> = Vec::new();
        for row in rows {
            let mut entity = row?;
            if !params.skip_side_effects {
                hit_ids.push(entity.id.clone());
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

        // #207: one batched side-effect write for all matched rows. Errors are
        // ignored here exactly as the previous per-row write was — a failed bump
        // must never fail the read.
        if !hit_ids.is_empty() {
            let _ = self.apply_recall_side_effects(&hit_ids);
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

        // Provenance trust signal (additive boost, never penalizes).
        // The agent should not just read — it should know what to trust. A
        // verified source outranks an unverified AI draft on the same topic.
        // Verified entities get the full boost; unverified entities get it
        // scaled by their certainty (source="agent" drafts default to 0.5),
        // so reliable sources float above speculative ones.
        //
        // Unlike the content-witness boost above, we sort by a local
        // trust-adjusted key rather than mutating decay_score: decay_score is
        // already capped at 1.0, so adding to it would saturate for fresh
        // entities and fail to reorder — exactly when trust must reorder. We
        // also avoid returning a >1.0 or inflated decay_score to the caller.
        if params.trust_weight > 0.0 {
            let trust_score = |e: &Entity| -> f64 {
                let trust = if e.verified {
                    1.0
                } else {
                    e.certainty.clamp(0.0, 1.0)
                };
                e.decay_score + params.trust_weight * trust
            };
            items.sort_by(|a, b| {
                trust_score(b)
                    .partial_cmp(&trust_score(a))
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

    /// Read-only keyword search for the hybrid sparse arm, ranked by BM25
    /// relevance instead of popularity (#247).
    ///
    /// `fts5_search` orders by `retrieval_count`/`last_accessed` and mutates
    /// access state — both wrong for a hybrid sub-query:
    ///   * popularity ordering is not a relevance signal, so a query that only
    ///     matched stopwords returned the whole corpus in popularity order, which
    ///     rank-based RRF then treated as full-confidence keyword hits;
    ///   * the access-state mutation made each recall a write, so the sparse arm's
    ///     popularity ordering (and thus hybrid's output) drifted run-to-run.
    ///
    /// This returns `(entity, relevance)` ordered by relevance (best first),
    /// where `relevance = -bm25(...)` so higher is better (SQLite's `bm25()` is
    /// more-negative-is-better). It issues no writes. Archived rows are not in the
    /// FTS index, so this path never reaches them; hybrid over archived entities
    /// simply has no keyword arm (dense-only), which is acceptable.
    fn fts5_bm25_search(
        &self,
        params: &RecallParams,
    ) -> Result<Vec<(Entity, f64)>, Box<dyn std::error::Error>> {
        // Keep only content-bearing terms. A natural-language query ("what hot
        // beverage does the user have each day") is mostly function words that
        // match almost every memory; matching on them turns the keyword arm into
        // popularity noise that dilutes the dense ranking under RRF (#247). We
        // drop stopwords here (sparse arm only — the fts5 keyword mode is
        // untouched) so the arm matches on meaning-bearing terms or not at all.
        let words: Vec<&str> = params
            .query
            .split_whitespace()
            .filter(|w| !w.is_empty() && !is_stopword(w))
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn()?;
        let escape_fts = |s: &str| -> String { s.replace('"', "\"\"") };
        let fts_query = words
            .iter()
            .map(|w| {
                let escaped = escape_fts(w);
                if escaped.is_empty() {
                    "\"\"".to_string()
                } else {
                    format!("\"{}\"*", escaped)
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut conditions: Vec<String> = vec!["e.archived = 0".to_string()];
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        // ?1 is always the FTS MATCH expression.
        param_values.push(Box::new(fts_query));

        if let Some(ref cat) = params.category {
            if !cat.is_empty() {
                conditions.push(format!("e.category = ?{}", param_values.len() + 1));
                param_values.push(Box::new(cat.clone()));
            }
        }
        if let Some(ref t) = params.entity_type {
            if !t.is_empty() {
                conditions.push(format!("e.type = ?{}", param_values.len() + 1));
                param_values.push(Box::new(t.clone()));
            }
        }
        if params.min_decay > 0.0 {
            conditions.push(format!("e.decay_score >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(params.min_decay));
        }
        if let Some(ref tp) = params.topic_path {
            if !tp.is_empty() {
                conditions.push(format!(
                    "e.topic_path LIKE ?{} ESCAPE '\\'",
                    param_values.len() + 1
                ));
                let escaped = tp
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                param_values.push(Box::new(format!("{}%", escaped)));
            }
        }
        if let Some(ao) = params.always_on {
            conditions.push(format!("e.always_on = ?{}", param_values.len() + 1));
            param_values.push(Box::new(ao as i32));
        }
        if let Some(ref ws) = params.workspace_hash {
            conditions.push(format!("e.workspace_hash = ?{}", param_values.len() + 1));
            param_values.push(Box::new(ws.clone()));
        }
        if let Some(ref aid) = params.agent_id {
            conditions.push(format!("e.agent_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(aid.clone()));
        }

        let safe_limit = params.limit.clamp(0, 1000);
        let limit_idx = param_values.len() + 1;
        param_values.push(Box::new(safe_limit));

        // bm25(entities_fts) is the trailing column (index 24); the leading 24
        // columns match `entity_from_row`'s expected layout exactly.
        let sql = format!(
            "SELECT e.id, e.category, e.key, e.body_json, e.status, e.type, e.tags,
                    e.decay_score, e.retrieval_count, e.layer, e.topic_path,
                    e.archived, e.archive_reason, e.links, e.verified, e.source,
                    e.created_at_unix_ms, e.last_accessed_unix_ms, NULL as embedding,
                    e.always_on, e.certainty, e.workspace_hash, e.agent_id, e.visibility,
                    bm25(entities_fts) AS rank
             FROM entities_fts
             JOIN entities e ON e.rowid = entities_fts.rowid
             WHERE entities_fts MATCH ?1 AND {conditions}
             ORDER BY rank ASC
             LIMIT ?{limit_idx}",
            conditions = conditions.join(" AND "),
        );

        let enc = self.encryption.as_ref();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let entity = entity_from_row(row, enc)?;
            let bm25: f64 = row.get(24)?;
            // Flip sign so higher = more relevant (BM25 is more-negative-is-better).
            Ok((entity, -bm25))
        })?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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
        let conn = self.conn()?;
        // Find the entity by category + key
        let mut stmt = conn.prepare(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
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
        let conn = self.conn()?;
        // M-1 extended: wrap forget's entity UPDATE + FTS DELETE in a transaction
        let tx = conn.unchecked_transaction()?;
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
        let conn = self.conn()?;
        // Verify both entities exist
        let from = self
            .get_entity(from_category, from_key)?
            .ok_or("Source entity not found")?;
        let _to: String = conn
            .query_row(
                "SELECT id FROM entities WHERE id = ?1",
                params![to_id],
                |r| r.get(0),
            )
            .map_err(|_| "Target entity not found")?;

        // Get existing links (default to empty array if missing)
        let links_str: String = conn
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
        conn.execute(
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
        let conn = self.conn()?;
        let from = self
            .get_entity(from_category, from_key)?
            .ok_or("Source entity not found")?;

        let links_str: String = conn.query_row(
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
        conn.execute(
            "UPDATE entities SET links = ?1, last_accessed_unix_ms = ?2 WHERE id = ?3",
            params![new_links, now_ms(), from.id],
        )?;

        Ok(())
    }

    // ─── Journal ─────────────────────────────────────────────────

    /// Append a journal event.
    pub fn journal(&self, event: &JournalEvent) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        // Compute audit chain hash: SHA-256(prev_hash || event_id || created_at_ms)
        let prev_hash: Option<String> = conn.query_row(
            "SELECT audit_hash FROM journal ORDER BY created_at_unix_ms DESC LIMIT 1",
            [],
            |r| r.get::<_, Option<String>>(0),
        ).unwrap_or(None);

        let computed_hash = if let Some(ref prev) = prev_hash {
            crate::db::sha256_chain(prev, &event.id, event.created_at_unix_ms)
        } else {
            crate::db::sha256_genesis(&event.id, event.created_at_unix_ms)
        };

        conn.execute(
            "INSERT INTO journal
             (id, event_type, evaluated_json, acted_json, forward_json,
              category, key, entity_id, agent_id, audit_hash, created_at_unix_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.id,
                event.event_type,
                event.evaluated_json,
                event.acted_json,
                event.forward_json,
                event.category,
                event.key,
                event.entity_id,
                event.agent_id,
                computed_hash,
                event.created_at_unix_ms,
            ],
        )?;
        Ok(())
    }

    /// All superseded (historical) versions of a (category, key), newest first.
    /// Each was the live fact during [recorded_at_unix_ms, invalidated_at_unix_ms);
    /// the current live version lives in `entities`, not here. Bodies are decrypted
    /// like a normal recall. (v2.4.0 — bi-temporal facts)
    pub fn history_versions(
        &self,
        category: &str,
        key: &str,
    ) -> Result<Vec<crate::models::Entity>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        // Column order matches entity_from_row (incl. NULL embedding at index 18).
        let mut stmt = conn.prepare(
            "SELECT id, category, key, body_json, status, type, tags, decay_score,
                    retrieval_count, layer, topic_path, archived, archive_reason, links,
                    verified, source, created_at_unix_ms, last_accessed_unix_ms,
                    NULL as embedding, always_on, certainty, workspace_hash, agent_id,
                    visibility
             FROM entity_history
             WHERE category = ?1 AND key = ?2
             ORDER BY invalidated_at_unix_ms DESC, recorded_at_unix_ms DESC",
        )?;
        let enc = self.encryption.as_ref();
        let rows = stmt.query_map(params![category, key], |r| entity_from_row(r, enc))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// The version of (category, key) that was the live fact at transaction time
    /// `as_of_unix_ms` — recorded at or before T and not yet superseded at T.
    /// Bi-temporal time-travel: "what did Mimir believe about this at time T?".
    /// Returns None if the fact had not been recorded yet at T. (v2.4.0)
    ///
    /// Versions partition time contiguously: each historical version was live
    /// during [recorded_at, invalidated_at) and the current row is live during
    /// [recorded_at, ∞), so at any T exactly one version matches. A superseded
    /// version takes precedence when its interval contains T; otherwise the live
    /// row answers iff it had been recorded by T.
    pub fn as_of(
        &self,
        category: &str,
        key: &str,
        as_of_unix_ms: i64,
    ) -> Result<Option<crate::models::Entity>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let enc = self.encryption.as_ref();
        // Column order matches entity_from_row (NULL embedding at index 18).
        const COLS: &str = "id, category, key, body_json, status, type, tags, decay_score, \
             retrieval_count, layer, topic_path, archived, archive_reason, links, verified, \
             source, created_at_unix_ms, last_accessed_unix_ms, NULL as embedding, always_on, \
             certainty, workspace_hash, agent_id, visibility";

        // A superseded version answers iff it was live across T:
        // recorded_at <= T < invalidated_at. (recorded_at may be NULL on a row
        // created before it was populated — fall back to created_at.) Among
        // matches, the smallest invalidated_at is the interval containing T.
        let hist_sql = format!(
            "SELECT {COLS} FROM entity_history
             WHERE category = ?1 AND key = ?2
               AND COALESCE(recorded_at_unix_ms, created_at_unix_ms) <= ?3
               AND invalidated_at_unix_ms > ?3
             ORDER BY invalidated_at_unix_ms ASC LIMIT 1"
        );
        {
            let mut stmt = conn.prepare(&hist_sql)?;
            let mut rows = stmt.query_map(params![category, key, as_of_unix_ms], |r| {
                entity_from_row(r, enc)
            })?;
            if let Some(r) = rows.next() {
                return Ok(Some(r?));
            }
        }

        // Otherwise the current live row answers iff it had been recorded by T.
        let live_sql = format!(
            "SELECT {COLS} FROM entities
             WHERE category = ?1 AND key = ?2
               AND COALESCE(recorded_at_unix_ms, created_at_unix_ms) <= ?3
             LIMIT 1"
        );
        let mut stmt = conn.prepare(&live_sql)?;
        let mut rows = stmt.query_map(params![category, key, as_of_unix_ms], |r| {
            entity_from_row(r, enc)
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Query journal events with time-range and filter parameters.
    pub fn timeline(
        &self,
        params: &TimelineParams,
    ) -> Result<Vec<JournalEvent>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
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
                    category, key, entity_id, agent_id, created_at_unix_ms
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

        let mut stmt = conn.prepare(&sql)?;
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
                agent_id: row.get::<_, Option<String>>(8).unwrap_or(None).unwrap_or_default(),
                created_at_unix_ms: row.get(9)?,
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
        let conn = self.conn()?;
        // Clean expired entries first (opportunistic)
        let _ = conn.execute(
            "DELETE FROM state WHERE expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms < ?1",
            params![now_ms()],
        );

        conn.execute(
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
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
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
                    let _ = conn
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
        let conn = self.conn()?;
        let affected = conn
            .execute("DELETE FROM state WHERE key = ?1", params![key])?;
        Ok(affected > 0)
    }

    /// List state keys matching an optional prefix.
    pub fn state_list(&self, prefix: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        // Delete expired entries first
        let _ = conn.execute(
            "DELETE FROM state WHERE expires_at_unix_ms IS NOT NULL AND expires_at_unix_ms < ?1",
            params![now_ms()],
        );

        let keys: Vec<String> = if prefix.is_empty() {
            let mut stmt = conn.prepare("SELECT key FROM state ORDER BY key")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for row in rows {
                v.push(row?);
            }
            v
        } else {
            let mut stmt = conn
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
        let conn = self.conn()?;
        schema::gather_stats(&conn, &self.db_path)
    }

    /// Get database file size in bytes.
    pub fn file_size_bytes(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let path = std::path::Path::new(&self.db_path);
        let metadata = std::fs::metadata(path)?;
        Ok(metadata.len())
    }

    /// Migrate from v0.1.x database.
    pub fn migrate_from_v0_1(
        &self,
        old_path: &str,
    ) -> Result<crate::models::MigrationReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        schema::migrate_from_v0_1(old_path, &conn)
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
        let conn = self.conn()?;
        let root = self
            .get_entity(category, key)?
            .ok_or_else(|| format!("entity not found: {}/{}", category, key))?;

        // Get root links
        let links_json: String = conn
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
            &conn,
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
        // #210: thread one pooled connection through the recursion instead of
        // checking one out per level — otherwise a deep chain would hold a
        // connection at every frame and exhaust/deadlock the pool.
        conn: &rusqlite::Connection,
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

        let links_json: String = conn
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
                    let child_links_json: String = conn
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
                        conn,
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

    /// Update an entity's status (e.g., to "deprecated").
    pub fn update_entity_status(
        &self,
        id: &str,
        status: &str,
        reason: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE entities SET status = ?1, archive_reason = ?2, last_accessed_unix_ms = ?3 WHERE id = ?4",
            params![status, reason, now_ms(), id],
        )?;
        Ok(())
    }

    /// Find entities with identical (category, key) and merge/archive duplicates, keeping the newest.
    /// Returns the number of entities archived.
    pub fn deduplicate_entities(&self, dry_run: bool) -> Result<i64, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let mut archived_count = 0i64;

        // Find duplicate (category, key) pairs, keeping the newest `created_at_unix_ms`.
        let mut stmt = conn.prepare(
            "SELECT T1.id, T1.category, T1.key FROM entities AS T1 JOIN (
                SELECT category, key, MAX(created_at_unix_ms) as max_created_at
                FROM entities
                GROUP BY category, key
                HAVING COUNT(*) > 1
            ) AS T2 ON T1.category = T2.category AND T1.key = T2.key
            WHERE T1.created_at_unix_ms < T2.max_created_at AND T1.archived = 0"
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;

        let mut ids_to_archive = Vec::new();
        for row in rows {
            let (id, category, key) = row?;
            ids_to_archive.push(id);
            eprintln!(
                "mimir: deduplicate_entities: found duplicate {}/{} (will archive oldest)",
                category, key
            );
        }

        if !dry_run && !ids_to_archive.is_empty() {
            let placeholders = ids_to_archive
                .iter()
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(", ");

            let tx = conn.unchecked_transaction()?;
            let now = now_ms();

            // Archive duplicates
            let update_sql = format!(
                "UPDATE entities SET archived = 1, archive_reason = 'deduplicate', last_accessed_unix_ms = ?1 WHERE id IN ({})",
                placeholders
            );

            // Build the params list: first the timestamp, then all IDs
            let mut param_refs: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
            let now_box: Box<dyn rusqlite::types::ToSql> = Box::new(now);
            param_refs.push(now_box.as_ref());
            let id_boxes: Vec<Box<dyn rusqlite::types::ToSql>> = ids_to_archive.iter().map(|s| Box::new(s.clone()) as Box<dyn rusqlite::types::ToSql>).collect();
            for b in &id_boxes {
                param_refs.push(b.as_ref());
            }
            archived_count = tx.execute(&update_sql, param_refs.as_slice())? as i64;

            // Clean FTS5 index for archived entities
            let delete_sql = format!(
                "DELETE FROM entities_fts WHERE rowid IN (SELECT rowid FROM entities WHERE id IN ({}) )",
                placeholders
            );
            let id_param_refs: Vec<&dyn rusqlite::types::ToSql> = id_boxes.iter().map(|b| b.as_ref()).collect();
            tx.execute(&delete_sql, id_param_refs.as_slice())?;
            tx.commit()?;
        }

        Ok(archived_count)
    }

    /// Detect journal entries pointing to archived/deleted entities.
    /// Returns the number of orphan journal entries found.
    pub fn detect_orphan_journal_entries(&self) -> Result<i64, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM journal WHERE entity_id IS NOT NULL AND entity_id != '' AND entity_id NOT IN (SELECT id FROM entities)",
            [],
            |r| r.get(0),
        )?;
        Ok(count)
    }

    /// Detect links pointing to archived/deleted entities.
    /// Returns the number of orphan links found.
    pub fn detect_orphan_links(&self) -> Result<i64, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        // Load all entity ids once and check link targets against an in-memory
        // set, instead of a COUNT(*) point query per link (which was N+1 — one
        // query per edge in the whole graph). #209
        let all_ids: std::collections::HashSet<String> = {
            let mut id_stmt = conn.prepare("SELECT id FROM entities")?;
            let ids = id_stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            ids
        };

        let mut orphan_count = 0i64;
        let mut stmt = conn.prepare(
            "SELECT links FROM entities WHERE links != '[]'"
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for row in rows {
            let links_json = row?;
            let links: Vec<MemoryLink> = serde_json::from_str(&links_json).unwrap_or_default();
            let original_len = links.len();
            let live = links
                .iter()
                .filter(|link| all_ids.contains(&link.target_id))
                .count();
            // Orphans = links whose target no longer exists. (Read-only
            // detection: we count but don't rewrite the entity.)
            orphan_count += (original_len - live) as i64;
        }
        Ok(orphan_count)
    }

    /// Run SQLite VACUUM command to reclaim space.
    pub fn vacuum(&self) -> Result<(), Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Get a single entity by ID (internal helper).
    fn get_entity_by_id(&self, id: &str) -> Result<Option<Entity>, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
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
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
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

        let mut stmt = conn.prepare(&sql)?;
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
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, event_type, evaluated_json, acted_json, forward_json,
                    category, key, entity_id, agent_id, created_at_unix_ms
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
                agent_id: row.get::<_, Option<String>>(8).unwrap_or(None).unwrap_or_default(),
                created_at_unix_ms: row.get(9)?,
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
        let conn = self.conn()?;
        let mut stmt = conn
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
        let conn = self.conn()?;
        let score = score.clamp(0.0, 1.0);
        let affected = conn.execute(
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
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
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

    /// Permanently delete all archived entities and run VACUUM to reclaim disk space.
    /// This is the only way to actually remove entities; prune/forget only soft-archive.
    /// Deleted entities are NOT recoverable. Use dry_run=true to preview first.
    pub fn purge(&self, dry_run: bool) -> Result<PurgeReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let before_size = match std::fs::metadata(&self.db_path) {
            Ok(m) => m.len() as i64,
            Err(_) => 0i64,
        };

        // Count archived entities
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM entities WHERE archived = 1")?;
        let count: i64 = stmt.query_row([], |r| r.get(0))?;
        stmt.finalize()?;

        if dry_run {
            return Ok(PurgeReport {
                entities_deleted: count,
                bytes_freed: 0,
                dry_run: true,
                completed_at_unix_ms: now_ms(),
            });
        }

        if count == 0 {
            return Ok(PurgeReport {
                entities_deleted: 0,
                bytes_freed: 0,
                dry_run: false,
                completed_at_unix_ms: now_ms(),
            });
        }

        // Delete archived entities from FTS5 index first, then the entities table
        let tx = conn.unchecked_transaction()?;
        conn.execute_batch(
            "DELETE FROM entities_fts WHERE rowid IN (SELECT rowid FROM entities WHERE archived = 1);
             DELETE FROM entities WHERE archived = 1;"
        )?;
        tx.commit()?;

        // VACUUM to reclaim disk space
        conn.execute_batch("VACUUM;")?;

        let after_size = match std::fs::metadata(&self.db_path) {
            Ok(m) => m.len() as i64,
            Err(_) => 0i64,
        };
        let freed = if before_size > after_size { before_size - after_size } else { 0 };

        Ok(PurgeReport {
            entities_deleted: count,
            bytes_freed: freed,
            dry_run: false,
            completed_at_unix_ms: now_ms(),
        })
    }

    /// Compact: archive entities below a decay threshold.
    pub fn compact(
        &self,
        min_decay: f64,
        dry_run: bool,
    ) -> Result<CompactReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let examined: i64 = conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;

        let archived = if dry_run {
            conn.query_row(
                "SELECT COUNT(*) FROM entities WHERE archived = 0 AND decay_score < ?1",
                params![min_decay],
                |r| r.get(0),
            )?
        } else {
            // M-1 extended: wrap compact UPDATE + FTS DELETE in a transaction
            let tx = conn.unchecked_transaction()?;
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
    pub fn vault_export(
        &self,
        vault_dir: &str,
        workspace_hash: Option<&str>,
    ) -> Result<VaultReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        use std::fs;
        use std::path::Path;

        fs::create_dir_all(vault_dir)?;
        let vault = Path::new(vault_dir);

        let sql = if let Some(ws) = workspace_hash {
            format!(
                "SELECT id, category, key, body_json, type, tags, decay_score,
                        retrieval_count, layer, workspace_hash, agent_id,
                        created_at_unix_ms, last_accessed_unix_ms
                 FROM entities WHERE archived = 0 AND workspace_hash = '{}'",
                ws.replace('\'', "''")
            )
        } else {
            "SELECT id, category, key, body_json, type, tags, decay_score,
                    retrieval_count, layer, workspace_hash, agent_id,
                    created_at_unix_ms, last_accessed_unix_ms
             FROM entities WHERE archived = 0".to_string()
        };
        let mut stmt = conn.prepare(&sql)?;

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
                r.get::<_, String>(9)?, // workspace_hash
                r.get::<_, String>(10)?,// agent_id
                r.get::<_, i64>(11)?,   // created_at
                r.get::<_, i64>(12)?,   // last_accessed
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
                workspace_hash_val,
                agent_id_val,
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
workspace_hash: {}
agent_id: {}
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
                workspace_hash_val,
                agent_id_val,
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
            let workspace_hash_val = get_fm("workspace_hash");
            let agent_id_val = get_fm("agent_id");

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
                workspace_hash: workspace_hash_val,
                agent_id: agent_id_val,
                visibility: "workspace".to_string(),
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
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
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
        let conn = self.conn()?;
        let words: Vec<&str> = context
            .split_whitespace()
            .filter(|w| w.len() >= 3)
            .collect();

        if words.is_empty() {
            return Ok(Vec::new());
        }

        // Prefilter candidates with an FTS5 prefix-OR query, then confirm each
        // against the entity's recall_when triggers in Rust. This replaces a
        // leading-wildcard `body_json LIKE '%recall_when%word%'` full table scan
        // (#209). entities_fts holds the plaintext body even when encryption is
        // on, so this also works on encrypted DBs — where the old LIKE ran
        // against ciphertext and silently matched nothing.
        let lc_words: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
        let fts_query = lc_words
            .iter()
            .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
            .filter(|w| !w.is_empty())
            .map(|w| format!("{}*", w))
            .collect::<Vec<_>>()
            .join(" OR ");
        if fts_query.is_empty() {
            return Ok(Vec::new());
        }

        let safe_limit = limit.clamp(0, 100);
        // Scan a multiple of the requested limit since some FTS candidates won't
        // pass the recall_when confirmation; bounded so this stays cheap.
        let scan_cap = (safe_limit * 5).clamp(50, 500);

        let sql = "SELECT id, category, key, body_json, status, type, tags,
                    decay_score, retrieval_count, layer, topic_path,
                    archived, archive_reason, links, verified, source,
                    created_at_unix_ms, last_accessed_unix_ms, NULL as embedding,
                    always_on, certainty, workspace_hash, agent_id, visibility
             FROM entities
             WHERE archived = 0
               AND rowid IN (SELECT rowid FROM entities_fts WHERE entities_fts MATCH ?1)
             ORDER BY decay_score DESC, retrieval_count DESC
             LIMIT ?2";

        let mut stmt = conn.prepare(sql)?;
        let enc = self.encryption.as_ref();
        let rows =
            stmt.query_map(params![fts_query, scan_cap], |row| entity_from_row(row, enc))?;

        let mut items = Vec::new();
        for row in rows {
            let entity = row?;
            if Self::matches_recall_when(&entity.body_json, &lc_words) {
                items.push(entity);
                if items.len() as i64 >= safe_limit {
                    break;
                }
            }
        }
        Ok(items)
    }

    /// True if any of `lc_words` (already lowercased) is a substring of any
    /// string in the body's `recall_when` array. Used to confirm FTS candidates.
    fn matches_recall_when(body_json: &str, lc_words: &[String]) -> bool {
        let parsed: serde_json::Value = match serde_json::from_str(body_json) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let triggers = match parsed.get("recall_when").and_then(|v| v.as_array()) {
            Some(t) => t,
            None => return false,
        };
        for trig in triggers {
            if let Some(s) = trig.as_str() {
                let s_lc = s.to_lowercase();
                if lc_words.iter().any(|w| s_lc.contains(w.as_str())) {
                    return true;
                }
            }
        }
        false
    }

    /// Coherence daemon: auto-groom the memory with promote, decay, link, archive.
    #[allow(unused_assignments)]
    pub fn cohere(
        &self,
        params: &crate::models::CohereParams,
    ) -> Result<crate::models::CohereReport, Box<dyn std::error::Error>> {
        let conn = self.conn()?;
        let now = now_ms();
        let mut promoted: i64 = 0;
        let mut decayed: i64 = 0;
        let mut linked: i64 = 0;
        let mut archived: i64 = 0;

        // Count total examined
        let examined: i64 = conn.query_row(
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
        conn.execute("BEGIN IMMEDIATE", [])?;
        let promote_threshold = if params.promote_threshold > 0 {
            params.promote_threshold
        } else {
            3
        };
        promoted = conn.execute(
            "UPDATE entities SET layer = 'working' WHERE layer = 'buffer' AND retrieval_count >= ?1",
            params![promote_threshold],
        )? as i64;

        // 2. Decay: apply Ebbinghaus decay to all non-archived entities
        // Formula: new_score = current_score * 0.95 (gentle decay)
        let decayed_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0 AND decay_score > 0.01",
            [],
            |r| r.get(0),
        )?;
        conn.execute(
            "UPDATE entities SET decay_score = decay_score * 0.95 WHERE archived = 0 AND decay_score > 0.01",
            [],
        )?;
        decayed = decayed_count;

        // 3. Link: auto-link entities sharing a category. The JOIN already
        // proves both rows exist and carries e1.id + e1.links, so we build the
        // new link lists in memory and flush one UPDATE per source entity —
        // instead of calling link() (≈4 queries each) per pair inside this write
        // transaction (#209). Accumulating per e1 also keeps multiple links to
        // the same source correct (the old code re-read links fresh each call).
        let max_links = params.max_links.clamp(0, 100) as i64;
        let mut pending: std::collections::HashMap<String, Vec<MemoryLink>> =
            std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT e1.id, e1.links, e2.id as e2_id
                 FROM entities e1
                 JOIN entities e2 ON e1.category = e2.category AND e1.id < e2.id
                 WHERE e1.archived = 0 AND e2.archived = 0
                 AND e1.tags != '[]' AND e2.tags != '[]'
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![max_links], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (e1_id, e1_links_json, e2_id) = row?;
                let entry = pending
                    .entry(e1_id)
                    .or_insert_with(|| serde_json::from_str(&e1_links_json).unwrap_or_default());
                if !entry.iter().any(|l| l.target_id == e2_id) {
                    entry.push(MemoryLink {
                        target_id: e2_id,
                        relationship: "auto-related".to_string(),
                        weight: 0.5,
                    });
                }
                linked += 1;
            }
        }

        let link_ts = now_ms();
        for (id, links) in &pending {
            let new_links = serde_json::to_string(links)?;
            conn.execute(
                "UPDATE entities SET links = ?1, last_accessed_unix_ms = ?2 WHERE id = ?3",
                params![new_links, link_ts, id],
            )?;
        }

        // 4. Archive: entities below decay threshold
        let archive_threshold = if params.archive_threshold > 0.0 {
            params.archive_threshold
        } else {
            0.05
        };
        archived = conn.execute(
            "UPDATE entities SET archived = 1, archive_reason = 'auto-archived by coherence daemon (decay < threshold)'
             WHERE archived = 0 AND decay_score < ?1",
            params![archive_threshold],
        )? as i64;

        // Clean FTS5 entries for archived entities
        if archived > 0 {
            conn.execute(
                "DELETE FROM entities_fts WHERE rowid IN (SELECT rowid FROM entities WHERE archived = 1)",
                [],
            )?;
        }

        conn.execute("COMMIT", [])?;

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
    /// Structured correction capture — stores the wrong approach, user correction,
    /// and task context as both an entity and a journal entry.
    pub fn correct(&self, params: &crate::models::CorrectParams) -> Result<crate::models::CorrectResult, Box<dyn std::error::Error>> {
        let id = format!("cor-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
        let now = now_ms();
        let category = if params.category.is_empty() { "correction".to_string() } else { params.category.clone() };
        let key = format!("correction-{}", &id[4..16]);
        
        let body = serde_json::json!({
            "wrong_approach": params.wrong_approach,
            "user_correction": params.user_correction,
            "task_context": params.task_context,
            "session_id": params.session_id,
            "lesson": format!("When {}: do NOT {}. Instead: {}", 
                params.task_context, params.wrong_approach, params.user_correction),
        });
        
        let entity = crate::models::Entity {
            id: id.clone(),
            category: category.clone(),
            key: key.clone(),
            body_json: serde_json::to_string(&body)?,
            status: "active".to_string(),
            entity_type: "correction".to_string(),
            tags: params.tags.clone(),
            decay_score: 1.0,
            retrieval_count: 0,
            layer: "working".to_string(),
            topic_path: String::new(),
            archived: false,
            archive_reason: String::new(),
            links: Vec::new(),
            verified: false,
            source: "mimir_correct".to_string(),
            always_on: false,
            certainty: 1.0,
            workspace_hash: String::new(),
            agent_id: String::new(),
            visibility: params.visibility.clone(),
            embedding: None,
            created_at_unix_ms: now,
            last_accessed_unix_ms: now,
        };
        self.remember(&entity)?;
        
        // Also create a journal entry
        let journal_id = format!("jrn-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
        let event = crate::models::JournalEvent {
            id: journal_id.clone(),
            event_type: "correction".to_string(),
            evaluated_json: serde_json::to_string(&serde_json::json!({"wrong_approach": params.wrong_approach}))?,
            acted_json: serde_json::to_string(&serde_json::json!({"user_correction": params.user_correction}))?,
            forward_json: serde_json::to_string(&serde_json::json!({"lesson_learned": true, "task_context": params.task_context}))?,
            category: category.clone(),
            key: key.clone(),
            entity_id: id.clone(),
            agent_id: String::new(),
            created_at_unix_ms: now,
        };
        self.journal(&event)?;
        
        Ok(crate::models::CorrectResult {
            entity_id: id,
            journal_id,
            category,
            key,
            created_at_unix_ms: now,
        })
    }

    /// Session synthesis — uses LLM to extract structured lessons from session content.
    /// Creates entities for each lesson found.
    pub fn synthesize(&self, params: &crate::models::SynthesizeParams) -> Result<crate::models::SynthesizeResult, Box<dyn std::error::Error>> {
        if !self.llm_config.enabled {
            return Err("LLM is not enabled. Set --llm-endpoint to enable mimir_synthesize.".into());
        }
        
        let prompt = format!(
            r#"You are a learning extraction system for an AI agent. Given a session transcript between a user and an AI agent, extract structured lessons about what worked and what didn't.

CRITICAL INSTRUCTIONS:
- Only extract lessons that are clearly evidenced in the transcript.
- If the user explicitly corrected the agent, that's a high-confidence correction.
- If the agent tried an approach and it failed, that's a failure lesson.
- If the agent tried something and it worked well, that's a success lesson.
- If an approach was abandoned without resolution, that's a dead_end.
- If a key architectural or strategic decision was made, that's a decision.
- Return ONLY valid JSON. No markdown, no commentary.

Transcript:
{}

Return a JSON object with a "lessons" array. Each lesson has:
- "lesson_type": one of "success", "failure", "correction", "dead_end", "decision", "insight"
- "summary": one-line summary of the lesson (max 200 chars)
- "evidence": quote or description from the transcript that supports this lesson (max 300 chars)
- "confidence": number 0.0-1.0 indicating how confident you are in this lesson

Example:
{{"lessons": [{{"lesson_type": "correction", "summary": "Use absolute paths not relative paths for file operations", "evidence": "User said 'always use absolute paths' after agent used relative path", "confidence": 0.95}}]}}

If no clear lessons found, return: {{"lessons": []}}"#,
            params.session_content
        );
        
        let body = serde_json::json!({
            "model": self.llm_config.model,
            "prompt": prompt,
            "stream": false,
        });
        
        let body_str = serde_json::to_string(&body)?;
        let request = ureq::post(&self.llm_config.endpoint)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(self.llm_config.timeout_secs));
        
        let request = if let Some(ref key) = self.llm_config.api_key {
            request.set("Authorization", &format!("Bearer {}", key))
        } else {
            request
        };
        
        let response_body = request.send_string(&body_str)?.into_string()?;
        let resp: serde_json::Value = serde_json::from_str(&response_body)?;
        let response_text = resp["response"].as_str().unwrap_or_default();
        
        // Parse the LLM response as JSON
        let lessons: Vec<crate::models::SynthesizedLesson> = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response_text) {
            if let Some(arr) = parsed["lessons"].as_array() {
                arr.iter().filter_map(|l| {
                    Some(crate::models::SynthesizedLesson {
                        lesson_type: l["lesson_type"].as_str()?.to_string(),
                        summary: l["summary"].as_str()?.to_string(),
                        evidence: l["evidence"].as_str()?.to_string(),
                        confidence: l["confidence"].as_f64()?,
                    })
                }).collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        
        let now = now_ms();
        let mut entities_created: i64 = 0;
        
        for lesson in &lessons {
            let id = format!("syn-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
            let key = format!("{}-{}", lesson.lesson_type, &id[4..16]);
            let body = serde_json::json!({
                "lesson_type": lesson.lesson_type,
                "summary": lesson.summary,
                "evidence": lesson.evidence,
                "confidence": lesson.confidence,
                "session_id": params.session_id,
                "source": "mimir_synthesize",
            });
            
            let entity = crate::models::Entity {
                id: id.clone(),
                category: "synthesis".to_string(),
                key: key.clone(),
                body_json: serde_json::to_string(&body)?,
                status: "active".to_string(),
                entity_type: "lesson".to_string(),
                tags: params.tags.clone(),
                decay_score: lesson.confidence,
                retrieval_count: 0,
                layer: "working".to_string(),
                topic_path: String::new(),
                archived: false,
                archive_reason: String::new(),
                links: Vec::new(),
                verified: false,
                source: "mimir_synthesize".to_string(),
                always_on: false,
                certainty: lesson.confidence,
                workspace_hash: String::new(),
                agent_id: String::new(),
                visibility: if params.visibility.is_empty() { "workspace".to_string() } else { params.visibility.clone() },
                embedding: None,
                created_at_unix_ms: now,
                last_accessed_unix_ms: now,
            };
            let _ = self.remember(&entity);
            entities_created += 1;
        }
        
        // Journal the synthesis run
        let journal_id = format!("jrn-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
        let event = crate::models::JournalEvent {
            id: journal_id.clone(),
            event_type: "synthesis".to_string(),
            evaluated_json: serde_json::to_string(&serde_json::json!({"session_id": params.session_id, "content_length": params.session_content.len()}))?,
            acted_json: serde_json::to_string(&serde_json::json!({"lessons_found": lessons.len(), "entities_created": entities_created}))?,
            forward_json: serde_json::to_string(&serde_json::json!({"lesson_types": lessons.iter().map(|l| &l.lesson_type).collect::<Vec<_>>()}))?,
            category: "synthesis".to_string(),
            key: format!("session-{}", params.session_id),
            entity_id: String::new(),
            agent_id: String::new(),
            created_at_unix_ms: now,
        };
        self.journal(&event)?;
        
        Ok(crate::models::SynthesizeResult {
            lessons,
            entities_created,
            journal_id,
            dry_run: false,
            completed_at_unix_ms: now,
        })
    }

    /// Performance benchmark tracking — records task metrics linked to memory recall usage.
    pub fn bench(&self, params: &crate::models::BenchParams) -> Result<crate::models::BenchResult, Box<dyn std::error::Error>> {
        let id = format!("bch-{}", uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string());
        let now = now_ms();
        let key = format!("bench-{}", &id[4..16]);
        
        let body = serde_json::json!({
            "task_description": params.task_description,
            "turns_taken": params.turns_taken,
            "tokens_used": params.tokens_used,
            "memory_recall_used": params.memory_recall_used,
            "recall_count": params.recall_count,
            "task_success": params.task_success,
            "session_id": params.session_id,
            "tokens_per_turn": if params.turns_taken > 0 { params.tokens_used / params.turns_taken } else { 0 },
        });
        
        let entity = crate::models::Entity {
            id: id.clone(),
            category: "benchmark".to_string(),
            key: key.clone(),
            body_json: serde_json::to_string(&body)?,
            status: "active".to_string(),
            entity_type: "benchmark".to_string(),
            tags: params.tags.clone(),
            decay_score: 0.5,
            retrieval_count: 0,
            layer: "working".to_string(),
            topic_path: String::new(),
            archived: false,
            archive_reason: String::new(),
            links: Vec::new(),
            verified: false,
            source: "mimir_bench".to_string(),
            always_on: false,
            certainty: 0.5,
            workspace_hash: String::new(),
            agent_id: String::new(),
            visibility: "workspace".to_string(),
            embedding: None, created_at_unix_ms: now,
            last_accessed_unix_ms: now,
        };
        self.remember(&entity)?;
        
        Ok(crate::models::BenchResult {
            entity_id: id,
            created_at_unix_ms: now,
        })
    }

}

/// Compute cosine similarity between two vectors.
/// Compute SHA-256 chain hash for the next journal entry.
/// chain = SHA-256(prev_hash || event_id || created_at_ms)
/// Simple deterministic hash for audit chain (SHA-256 substitute).
/// Uses Rust's stdlib SipHash — not cryptographic but fast and deterministic.
/// For production audit logs, upgrade to a proper crypto crate.
fn audit_hash(prev_hash: &str, event_id: &str, created_at_ms: i64) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    prev_hash.hash(&mut hasher);
    event_id.hash(&mut hasher);
    created_at_ms.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sha256_chain(prev_hash: &str, event_id: &str, created_at_ms: i64) -> String {
    audit_hash(prev_hash, event_id, created_at_ms)
}

fn sha256_genesis(event_id: &str, created_at_ms: i64) -> String {
    audit_hash("genesis", event_id, created_at_ms)
}

/// Verify the audit chain by checking that each hash was correctly computed
/// from the previous entry. Returns the number of entries verified, or an error
/// describing the first invalid entry.
// Retained as a callable integrity check (audit chain is written by the journal
// path) but not yet wired to a CLI/MCP command, so it has no in-crate caller.
#[allow(dead_code)]
pub fn verify_audit_chain(db: &Database) -> Result<i64, String> {
    let conn = db.conn().map_err(|e| format!("connection: {}", e))?;
    let mut stmt = conn.prepare(
        "SELECT id, audit_hash, created_at_unix_ms FROM journal WHERE audit_hash != '' ORDER BY created_at_unix_ms ASC",
    ).map_err(|e| format!("prepare: {}", e))?;

    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    }).map_err(|e| format!("query: {}", e))?;

    let mut count = 0i64;
    let mut prev_hash: Option<String> = None;
    for row in rows {
        let (id, stored_hash, ts) = row.map_err(|e| format!("row: {}", e))?;
        let expected = if let Some(ref prev) = prev_hash {
            sha256_chain(prev, &id, ts)
        } else {
            sha256_genesis(&id, ts)
        };
        if expected != stored_hash {
            return Err(format!(
                "audit chain broken at journal entry '{}': expected {} but stored {}",
                id, expected, stored_hash
            ));
        }
        prev_hash = Some(stored_hash);
        count += 1;
    }
    Ok(count)
}

// Only the non-bundled-embeddings build uses this scalar fallback; the feature
// build scores with the vectorized ndarray path above, so gate it to match its
// sole caller and avoid a dead-code warning under the feature. (#212)
#[cfg(not(feature = "bundled-embeddings"))]
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

/// Fusion weight for the sparse (keyword) arm of hybrid recall (#247).
///
/// The dense arm is the trusted primary semantic signal; the keyword arm is
/// fused at a reduced weight (`< 1.0`) so it *augments* dense recall rather than
/// overriding it. Plain equal-weight RRF let a keyword rank-1 tie or beat a
/// confident dense rank-1, so a keyword arm that matched only common terms could
/// bury the correct semantic hit.
///
/// Relevance-awareness lives in how the arm is *built*, not in a post-hoc
/// scalar: `fts5_bm25_search` drops stopwords and ranks by BM25 relevance
/// instead of popularity, so a paraphrase query with no meaning-bearing overlap
/// produces an empty arm (weight 0 here) instead of the whole corpus as noise.
/// An arm that fires has matched real content terms and is fused at
/// `SPARSE_ARM_WEIGHT`.
pub(crate) fn sparse_arm_weight(n_hits: usize) -> f64 {
    /// Keyword-arm weight relative to the dense arm (1.0). Kept below 1 so dense
    /// stays the primary ranking and the keyword arm corroborates / adds lexical
    /// recall rather than overriding a confident semantic hit.
    const SPARSE_ARM_WEIGHT: f64 = 0.5;
    if n_hits == 0 {
        0.0
    } else {
        SPARSE_ARM_WEIGHT
    }
}

/// Reciprocal Rank Fusion: combine dense and sparse result sets.
/// k controls the rank penalty (higher k = less penalty for lower ranks).
///
/// The dense arm carries full weight; the sparse arm is scaled by `sparse_weight`
/// (see `sparse_arm_weight`) so a weak/empty keyword arm cannot dilute a strong
/// dense ranking (#247).
///
/// When `recency_half_life_secs` is `Some(hl)` with `hl > 0` (#235), each fused
/// score is multiplied by a time-decay factor `0.5^(age / hl)` based on the
/// entity's `created_at_unix_ms` relative to `now_ms`, so recent memories outrank
/// older but lexically-similar hits. `None` (or `hl <= 0`) leaves the pure
/// relevance ranking untouched. Entities with an unset (`<= 0`)
/// `created_at_unix_ms` are never penalized (factor 1.0).

pub fn reciprocal_rank_fusion(
    dense_results: &[(crate::models::Entity, f64)],
    sparse_results: &[(crate::models::Entity, f64)],
    k: f64,
    limit: usize,
    sparse_weight: f64,
    recency_half_life_secs: Option<f64>,
    now_ms: i64,
) -> Vec<(crate::models::Entity, f64)> {
    use std::collections::HashMap;

    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut entities: HashMap<String, crate::models::Entity> = HashMap::new();

    // The dense arm always carries full weight; the sparse (keyword) arm is
    // weighted by its relevance confidence so a weak/noisy arm cannot dilute a
    // strong dense ranking (#247).
    for (rank, (entity, _)) in dense_results.iter().enumerate() {
        let rrf = 1.0 / (k + (rank + 1) as f64);
        *scores.entry(entity.id.clone()).or_insert(0.0) += rrf;
        entities
            .entry(entity.id.clone())
            .or_insert_with(|| entity.clone());
    }

    for (rank, (entity, _)) in sparse_results.iter().enumerate() {
        let rrf = sparse_weight / (k + (rank + 1) as f64);
        *scores.entry(entity.id.clone()).or_insert(0.0) += rrf;
        entities
            .entry(entity.id.clone())
            .or_insert_with(|| entity.clone());
    }

    // Optional recency re-weighting (#235): multiply each fused score by an
    // exponential decay on the entity's age. half_life seconds → factor 0.5.
    let recency = recency_half_life_secs.filter(|hl| *hl > 0.0);

    let mut fused: Vec<_> = scores
        .into_iter()
        .filter_map(|(id, score)| {
            entities.remove(&id).map(|entity| {
                let score = match recency {
                    Some(hl) if entity.created_at_unix_ms > 0 => {
                        let age_secs =
                            ((now_ms - entity.created_at_unix_ms).max(0) as f64) / 1000.0;
                        score * 0.5_f64.powf(age_secs / hl)
                    }
                    _ => score,
                };
                (entity, score)
            })
        })
        .collect();

    // Sort by fused score (desc), breaking ties by entity id (asc) so the
    // ordering is fully deterministic run-to-run. Without an explicit tie-break,
    // equal-score entities fell back to the (randomly-seeded) HashMap iteration
    // order, making hybrid recall drift ~1-2 queries between identical runs
    // (#247).
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    fused.truncate(limit);
    fused
}

/// Common English function/question words stripped from the hybrid keyword arm
/// before FTS matching (#247).
///
/// These appear in nearly every memory, so matching on them makes the keyword
/// arm return the whole corpus as low-relevance noise. Removing them lets the
/// sparse arm match on meaning-bearing terms (or match nothing, in which case it
/// is dropped). This list intentionally covers only high-frequency English
/// stopwords and interrogatives; it is used solely for the hybrid sparse arm and
/// never alters what the `fts5` keyword mode matches.
fn is_stopword(word: &str) -> bool {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "any", "are", "as", "at", "be", "been", "by", "can", "could", "did",
        "do", "does", "each", "for", "from", "had", "has", "have", "he", "her", "his", "how", "i",
        "in", "is", "it", "its", "many", "me", "much", "my", "of", "on", "or", "our", "she",
        "some", "that", "the", "their", "them", "they", "this", "to", "user", "users",
        "was", "we", "were", "what", "when", "where", "which", "who", "whom", "whose", "why",
        "will", "with", "would", "you", "your",
    ];
    let lower = word.to_ascii_lowercase();
    STOPWORDS.contains(&lower.as_str())
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
        workspace_hash: row.get::<_, Option<String>>(21).unwrap_or(None).unwrap_or_default(),
        agent_id: row.get::<_, Option<String>>(22).unwrap_or(None).unwrap_or_default(),
        visibility: row.get::<_, Option<String>>(23).unwrap_or(None).unwrap_or_else(|| "workspace".to_string()),
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
            workspace_hash: String::new(),
            agent_id: String::new(),
            visibility: "workspace".to_string(),
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
    fn remember_supersedes_changed_content_into_history() {
        let (db, path) = temp_db();
        let e1 = make_entity("e-sup", "facts", "fav-color", r#"{"note":"blue"}"#);
        db.remember(&e1).unwrap();
        assert!(
            db.history_versions("facts", "fav-color").unwrap().is_empty(),
            "no history after the first remember"
        );

        // Changed content under the same (category,key) -> supersession.
        let e2 = make_entity("ignored-id", "facts", "fav-color", r#"{"note":"green"}"#);
        db.remember(&e2).unwrap();

        let hist = db.history_versions("facts", "fav-color").unwrap();
        assert_eq!(hist.len(), 1, "prior version snapshotted into history");
        assert!(hist[0].body_json.contains("blue"), "history keeps the OLD content");

        // The live row carries the NEW content, links back to a snapshot, stays live.
        let conn = db.conn().unwrap();
        let (live_body, supersedes, invalidated): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT body_json, supersedes, invalidated_at_unix_ms FROM entities \
                 WHERE category='facts' AND key='fav-color'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(live_body.contains("green"), "live row has the new content");
        assert!(supersedes.starts_with("hist-"), "live row links to its snapshot");
        assert_eq!(invalidated, None, "live row must not be invalidated");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn remember_identical_content_creates_no_history() {
        let (db, path) = temp_db();
        let e = make_entity("e-idem", "facts", "k", r#"{"note":"same"}"#);
        db.remember(&e).unwrap();
        let again = make_entity("e-idem-2", "facts", "k", r#"{"note":"same"}"#);
        db.remember(&again).unwrap();
        assert!(
            db.history_versions("facts", "k").unwrap().is_empty(),
            "an identical re-assertion must not create a spurious version"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn remember_sets_recorded_at_to_created_at_on_insert() {
        let (db, path) = temp_db();
        let e = make_entity("e-rec", "facts", "k2", r#"{"n":1}"#);
        db.remember(&e).unwrap();
        let conn = db.conn().unwrap();
        let (created, recorded): (i64, Option<i64>) = conn
            .query_row(
                "SELECT created_at_unix_ms, recorded_at_unix_ms FROM entities WHERE id='e-rec'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(recorded, Some(created), "recorded_at must equal created_at on insert");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn as_of_returns_the_version_live_at_a_past_instant() {
        use std::thread::sleep;
        use std::time::Duration;
        let (db, path) = temp_db();

        // v1 recorded "now".
        let v1 = make_entity("e-asof", "facts", "capital", r#"{"note":"Bonn"}"#);
        db.remember(&v1).unwrap();
        let t_created: i64 = {
            let conn = db.conn().unwrap();
            conn.query_row(
                "SELECT created_at_unix_ms FROM entities WHERE id='e-asof'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };

        // Separate the two transaction times so an instant strictly between exists.
        sleep(Duration::from_millis(5));
        let v2 = make_entity("ignored", "facts", "capital", r#"{"note":"Berlin"}"#);
        db.remember(&v2).unwrap();
        let t_super: i64 = {
            let conn = db.conn().unwrap();
            conn.query_row(
                "SELECT recorded_at_unix_ms FROM entities WHERE category='facts' AND key='capital'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(t_super > t_created, "supersession must advance recorded_at");

        // Before the fact existed → None.
        assert!(db.as_of("facts", "capital", t_created - 1).unwrap().is_none());
        // Strictly between v1 and v2 → the OLD version (from history).
        let mid = db
            .as_of("facts", "capital", t_super - 1)
            .unwrap()
            .expect("v1 was live just before the supersede");
        assert!(mid.body_json.contains("Bonn"), "as_of mid must return the old version");
        // At/after the supersede → the CURRENT version (live row).
        let now = db
            .as_of("facts", "capital", t_super)
            .unwrap()
            .expect("v2 is live from the supersede onward");
        assert!(now.body_json.contains("Berlin"), "as_of now must return the current version");

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

    // #228: the lossless length prefilter must never change which entities are
    // treated as near-duplicates. We assert find_near_duplicate (prefilter on)
    // agrees with an exhaustive trigram scan (the un-prefiltered reference) for a
    // spread of probe bodies, including ones that exercise the short-candidate
    // prune path.
    #[test]
    fn find_near_duplicate_length_prefilter_matches_exhaustive_scan() {
        let (db, path) = temp_db();
        let threshold = 0.7;

        // Same-category corpus with deliberately varied body lengths.
        let bodies = [
            ("c1", r#"{"note":"the quick brown fox jumps over the lazy dog"}"#),
            ("c2", r#"{"note":"the quick brown fox jumps over the lazy cat"}"#),
            ("c3", r#"{"x":"tiny"}"#),
            ("c4", r#"{"note":"completely unrelated content about databases"}"#),
            ("c5", r#"{"note":"the quick brown fox jumps over the lazy dog!!"}"#),
        ];
        for (key, body) in bodies {
            db.remember(&make_entity(key, "insight", key, body)).unwrap();
        }

        // Some corpus bodies are near-duplicates of each other and so dedup on
        // insert; read back what actually landed rather than assuming all five.
        let stored: Vec<String> = {
            let conn = db.conn().unwrap();
            let mut stmt = conn
                .prepare("SELECT body_json FROM entities WHERE category = 'insight' AND archived = 0")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };

        // Exhaustive reference: any stored same-category body whose trigram
        // similarity to the probe meets the threshold. This is exactly what
        // find_near_duplicate computes without the length prefilter.
        let reference_has_dup = |probe: &str| -> bool {
            stored
                .iter()
                .any(|b| Database::trigram_similarity(b, probe) >= threshold)
        };

        let probes = [
            r#"{"note":"the quick brown fox jumps over the lazy dog"}"#, // exact match
            r#"{"note":"the quick brown fox jumps over the lazy dog."}"#, // near match
            r#"{"x":"tiny"}"#,                                            // exact short match
            r#"{"y":"no"}"#,                                              // short, no match
            r#"{"note":"a totally different sentence with no overlap xyz"}"#, // long, no match
        ];
        for probe in probes {
            let got = db
                .find_near_duplicate("insight", probe, threshold, false)
                .unwrap()
                .is_some();
            assert_eq!(
                got,
                reference_has_dup(probe),
                "length prefilter changed dedup outcome for probe: {}",
                probe
            );
        }

        let _ = fs::remove_file(&path);
    }

    // #228: the opt-in FTS prefilter finds near-duplicates that share an FTS
    // token (the common case) but, by design, can miss a near-duplicate that
    // shares none. Both halves are asserted so the documented tradeoff is pinned.
    #[test]
    fn find_near_duplicate_fts_prefilter_tradeoff() {
        let (db, path) = temp_db();
        let threshold = 0.7;

        // Token-sharing near-duplicate: caught by the FTS prefilter.
        db.remember(&make_entity(
            "f1",
            "insight",
            "f1",
            r#"{"content":"hello world foo bar"}"#,
        ))
        .unwrap();
        let token_sharing_probe = r#"{"content":"hello world foo baz"}"#;
        assert!(
            Database::trigram_similarity(
                r#"{"content":"hello world foo bar"}"#,
                token_sharing_probe
            ) >= threshold,
            "probe must be a genuine near-duplicate"
        );
        assert!(
            db.find_near_duplicate("insight", token_sharing_probe, threshold, true)
                .unwrap()
                .is_some(),
            "FTS prefilter should find a token-sharing near-duplicate"
        );

        // No-shared-token near-duplicate: the whole body is a single token, so a
        // one-character difference yields disjoint FTS tokens despite >=0.7
        // trigram overlap. The exact scan finds it; the FTS prefilter cannot.
        db.remember(&make_entity("f2", "insight", "f2", "abcabcabcabc"))
            .unwrap();
        let no_shared_token_probe = "abcabcabcabd";
        assert!(
            Database::trigram_similarity("abcabcabcabc", no_shared_token_probe) >= threshold,
            "probe must be a genuine near-duplicate"
        );
        assert!(
            db.find_near_duplicate("insight", no_shared_token_probe, threshold, false)
                .unwrap()
                .is_some(),
            "exact scan should find the no-shared-token near-duplicate"
        );
        assert!(
            db.find_near_duplicate("insight", no_shared_token_probe, threshold, true)
                .unwrap()
                .is_none(),
            "FTS prefilter is expected to miss a near-duplicate sharing no token"
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

    // #227: keyword recall matches via the FTS5 index (with prefix matching)
    // rather than an unconditional body_json LIKE scan. A prefix query must
    // still find longer tokens, and an exact token must still match.
    #[test]
    fn recall_keyword_prefix_matches_via_fts() {
        let (db, path) = temp_db();
        db.remember(&make_entity(
            "e1",
            "insight",
            "auth-note",
            r#"{"content": "authentication flow uses tokens"}"#,
        ))
        .unwrap();

        // Prefix: "auth" must still find the "authentication" token.
        let prefix = db
            .recall(&RecallParams {
                query: "auth".to_string(),
                limit: 10,
                ..RecallParams::default()
            })
            .unwrap();
        assert!(
            prefix.iter().any(|e| e.id == "e1"),
            "prefix query 'auth' should match 'authentication' via FTS5"
        );

        // Exact token still matches.
        let exact = db
            .recall(&RecallParams {
                query: "tokens".to_string(),
                limit: 10,
                ..RecallParams::default()
            })
            .unwrap();
        assert!(
            exact.iter().any(|e| e.id == "e1"),
            "exact token 'tokens' should match"
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn trust_weight_ranks_verified_above_drafts() {
        let (db, path) = temp_db();

        // Two entities matching the same query. The draft is inserted first and
        // would otherwise tie on decay/recency; trust_weight must float the
        // verified source to the top.
        let mut draft = make_entity(
            "draft-1",
            "decision",
            "db-choice-draft",
            r#"{"note": "maybe use sqlite for the database"}"#,
        );
        draft.verified = false;
        draft.source = "agent".to_string();
        draft.certainty = 0.5;

        let mut verified = make_entity(
            "verified-1",
            "decision",
            "db-choice-final",
            r#"{"note": "decided: use sqlite for the database"}"#,
        );
        verified.verified = true;
        verified.source = "user".to_string();
        verified.certainty = 0.9;

        db.remember(&draft).unwrap();
        db.remember(&verified).unwrap();

        // Baseline: without trust_weight, no provenance ordering is guaranteed.
        // With trust_weight, the verified source must rank first.
        let params = RecallParams {
            query: "sqlite database".to_string(),
            trust_weight: 0.5,
            limit: 10,
            skip_side_effects: true,
            ..RecallParams::default()
        };
        let results = db.recall(&params).unwrap();
        assert_eq!(results.len(), 2, "both entities should match the query");
        assert_eq!(
            results[0].id, "verified-1",
            "verified source must outrank the unverified draft when trust_weight > 0"
        );

        // decay_score must not be mutated/inflated by the trust boost.
        assert!(
            results.iter().all(|e| e.decay_score <= 1.0),
            "trust ranking must not push decay_score above 1.0"
        );

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
            db.conn().unwrap()
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
                purge_all: false,
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
        db.conn().unwrap()
            .execute("DELETE FROM entities_fts", [])
            .unwrap();
        let count_before: i64 = db.conn().unwrap()
            .query_row("SELECT COUNT(*) FROM entities_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 0);

        // Reindex repairs it.
        let n = db.reindex_fts().unwrap();
        assert_eq!(n, 1);
        let count_after: i64 = db.conn().unwrap()
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
    fn recall_when_matches_triggers_via_fts() {
        let (db, path) = temp_db();

        let e1 = make_entity(
            "rw1",
            "skill",
            "deploy",
            r#"{"recall_when": ["deploying to production", "kubernetes rollout"], "note": "steps"}"#,
        );
        // Has the word "about" but no recall_when field — must be filtered out
        // even though it becomes an FTS candidate.
        let e2 = make_entity("rw2", "skill", "other", r#"{"note": "thinking about cats"}"#);
        // Has recall_when but unrelated triggers.
        let e3 = make_entity("rw3", "skill", "billing", r#"{"recall_when": ["invoice generation"]}"#);
        db.remember(&e1).unwrap();
        db.remember(&e2).unwrap();
        db.remember(&e3).unwrap();

        let hits = db.recall_when("about to start deploying the service", 10).unwrap();
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        assert!(ids.contains(&"rw1".to_string()), "should match deploy trigger, got {ids:?}");
        assert!(!ids.contains(&"rw2".to_string()), "no recall_when field -> excluded by confirmation");
        assert!(!ids.contains(&"rw3".to_string()), "unrelated triggers -> excluded");

        // No overlapping triggers at all -> rw1 not returned.
        let none = db.recall_when("completely unrelated banana topic", 10).unwrap();
        assert!(none.iter().all(|h| h.id != "rw1"), "no spurious match");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cohere_auto_links_batched_same_source() {
        let (db, path) = temp_db();

        // All same category with non-empty tags; ids order ca < cb < cc, so the
        // self-join yields pairs (ca,cb), (ca,cc), (cb,cc). ca appears in two
        // pairs — the batched accumulation must keep BOTH links on ca.
        let mut a = make_entity("ca", "project", "alpha", r#"{"n":1}"#);
        a.tags = vec!["x".to_string()];
        let mut b = make_entity("cb", "project", "beta", r#"{"n":2}"#);
        b.tags = vec!["x".to_string()];
        let mut c = make_entity("cc", "project", "gamma", r#"{"n":3}"#);
        c.tags = vec!["y".to_string()];
        db.remember(&a).unwrap();
        db.remember(&b).unwrap();
        db.remember(&c).unwrap();

        let params = crate::models::CohereParams {
            dry_run: false,
            max_links: 100,
            promote_threshold: 0,
            archive_threshold: 0.0,
        };
        let report = db.cohere(&params).unwrap();
        assert!(report.linked >= 1, "should link, got {}", report.linked);

        let ca = db.get_entity("project", "alpha").unwrap().unwrap();
        let targets: Vec<String> = ca.links.iter().map(|l| l.target_id.clone()).collect();
        assert!(targets.contains(&"cb".to_string()), "ca->cb, got {targets:?}");
        assert!(
            targets.contains(&"cc".to_string()),
            "ca->cc must survive batched same-source accumulation, got {targets:?}"
        );

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
            agent_id: "agent-1".to_string(),
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

    // #207: recall must apply retrieval-count/recency/decay/layer side-effects to
    // every returned row, in one batched write, and bump each row exactly once.
    #[test]
    fn recall_batches_side_effects_and_bumps_once() {
        let (db, path) = temp_db();

        // Insert rows with controlled counts/decay so we can pin the CASE
        // boundaries of compute_layer (buffer < 5 ≤ working < 20 ≤ core) and the
        // decay cap. Side-effect-free raw inserts mirror the stress test.
        let rows = [
            // (id, retrieval_count, decay_score, archived)
            ("se-buffer", 0i64, 0.5f64, 0i64),  // → count 1  → buffer, decay 0.75
            ("se-working", 4, 0.5, 0),          // → count 5  → working, decay 0.75
            ("se-core", 19, 0.9, 0),            // → count 20 → core, decay 1.0 (capped)
            ("se-archived", 7, 0.5, 1),         // filtered out → never bumped
        ];
        for (id, count, decay, archived) in rows {
            db.conn().unwrap()
                .execute(
                    "INSERT INTO entities (id, category, key, body_json, type, status,
                        retrieval_count, last_accessed_unix_ms, created_at_unix_ms,
                        decay_score, layer, archived)
                     VALUES (?1, 'insight', ?1, '{\"content\":\"x\"}', 'insight', 'active',
                        ?2, 0, 0, ?3, 'buffer', ?4)",
                    params![id, count, decay, archived],
                )
                .unwrap();
        }

        // Live recall (side effects ON, the MCP default).
        let live = db.recall(&RecallParams::default()).unwrap();
        // Returned entities must reflect PRE-bump state (the loop never mutates
        // the in-memory entity; only the DB is updated).
        let live_working = live.iter().find(|e| e.id == "se-working").unwrap();
        assert_eq!(
            live_working.retrieval_count, 4,
            "returned entity must show pre-bump count"
        );
        assert!(
            !live.iter().any(|e| e.id == "se-archived"),
            "archived entity must not be returned"
        );

        // Re-read without side effects to observe the persisted batched bump.
        let after = db
            .recall(&RecallParams {
                include_archived: true,
                skip_side_effects: true,
                limit: 50,
                ..RecallParams::default()
            })
            .unwrap();
        let get = |id: &str| after.iter().find(|e| e.id == id).unwrap();

        // Count bumped by exactly 1 — one batched write, not N, and not per-variant.
        assert_eq!(get("se-buffer").retrieval_count, 1);
        assert_eq!(get("se-working").retrieval_count, 5);
        assert_eq!(get("se-core").retrieval_count, 20);
        // Archived row was filtered from recall → untouched.
        assert_eq!(get("se-archived").retrieval_count, 7);

        // Layer recomputed on the new count, matching compute_layer exactly.
        assert_eq!(get("se-buffer").layer, "buffer");
        assert_eq!(get("se-working").layer, "working");
        assert_eq!(get("se-core").layer, "core");

        // Decay boosted by DECAY_BOOST (0.25), capped at 1.0.
        assert!((get("se-buffer").decay_score - 0.75).abs() < 1e-9);
        assert!((get("se-working").decay_score - 0.75).abs() < 1e-9);
        assert!((get("se-core").decay_score - 1.0).abs() < 1e-9);

        let _ = fs::remove_file(&path);
    }

    // #209: dense_search must score a lightweight id+embedding scan, then hydrate
    // only the top-k, returned in score order (archived rows excluded). Runs on
    // the default (scalar) build; the feature build shares the scan + hydrate and
    // differs only in the scoring math.
    #[test]
    fn dense_search_returns_top_k_hydrated_in_score_order() {
        let (db, path) = temp_db();
        let blob = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
        let insert = |id: &str, key: &str, emb: &[f32], archived: i64| {
            db.conn().unwrap()
                .execute(
                    "INSERT INTO entities (id, category, key, body_json, type, status,
                        retrieval_count, last_accessed_unix_ms, created_at_unix_ms,
                        decay_score, layer, embedding, archived)
                     VALUES (?1, 'insight', ?2, ?3, 'insight', 'active', 0, 0, 0, 1.0, 'working', ?4, ?5)",
                    params![id, key, format!("{{\"k\":\"{}\"}}", key), blob(emb), archived],
                )
                .unwrap();
        };
        insert("d-best", "best", &[1.0, 0.0, 0.0], 0);
        insert("d-mid", "mid", &[0.7, 0.7, 0.0], 0);
        insert("d-far", "far", &[0.0, 1.0, 0.0], 0);
        insert("d-arch", "arch", &[1.0, 0.0, 0.0], 1); // archived → must be excluded

        let results = db.dense_search(&[1.0, 0.0, 0.0], 2).unwrap();

        // Top-2 by cosine: best (1.0) then mid (~0.707); far (0) truncated, archived filtered.
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0.key, "best");
        assert_eq!(results[1].0.key, "mid");
        assert!(results[0].1 > results[1].1, "must be in descending score order");
        // Full entity hydrated (body present), not just id/embedding.
        assert!(results[0].0.body_json.contains("best"));
        assert!(
            !results.iter().any(|(e, _)| e.key == "arch"),
            "archived entity must not be returned"
        );

        let _ = fs::remove_file(&path);
    }

    // #226: dense/hybrid recall must embed the query, not silently fall back to
    // FTS5. With no embedding backend configured, a dense recall over a
    // non-empty query surfaces the backend error instead of returning keyword
    // results — the silent fallback was what masked the missing embedding.
    #[test]
    fn dense_recall_without_backend_errors_instead_of_silent_fts5() {
        let (mut db, path) = temp_db();
        // Explicitly disable the embedding backend. With bundled-embeddings now on
        // by default (#237) the default Db *has* a backend, so to exercise the
        // "no backend" path (#226) we turn it off here — keeping this test valid in
        // both the default and --no-default-features builds.
        db.embedding_config.enabled = false;
        // Seed a row a keyword search WOULD match, so the pre-fix silent FTS5
        // fallback would have wrongly returned it as an Ok result.
        db.conn()
            .unwrap()
            .execute(
                "INSERT INTO entities (id, category, key, body_json, type, status,
                    retrieval_count, last_accessed_unix_ms, created_at_unix_ms, decay_score, layer)
                 VALUES ('e1', 'insight', 'alpha', '{\"content\":\"alpha beta\"}', 'insight', 'active', 0, 0, 0, 1.0, 'working')",
                params![],
            )
            .unwrap();

        let res = db.recall(&RecallParams {
            query: "alpha".to_string(),
            mode: crate::models::SearchMode::Dense,
            limit: 5,
            skip_side_effects: true,
            ..RecallParams::default()
        });

        assert!(
            res.is_err(),
            "dense recall with no embedding backend must error, not silently return FTS5 results"
        );
        let msg = format!("{}", res.unwrap_err());
        assert!(
            msg.contains("embedding backend"),
            "error should name the missing embedding backend, got: {msg}"
        );

        let _ = fs::remove_file(&path);
    }

    // #210: a single Database (pooled internally) shared as Arc<Database> across
    // threads must serve concurrent reads + writes without panicking, locking up,
    // or losing writes — the property the transport now relies on (no Mutex).
    #[test]
    fn pooled_database_shared_across_threads() {
        use std::sync::Arc;
        use std::thread;

        let (db, path) = temp_db();
        let db = Arc::new(db);

        // Raw inserts through the pool (each thread checks out its own pooled
        // connection) — this tests concurrent pooled writes directly, without
        // remember's near-duplicate dedup confounding the row count.
        let insert = |conn: &rusqlite::Connection, id: &str| {
            conn.execute(
                "INSERT INTO entities (id, category, key, body_json, type, status,
                    retrieval_count, last_accessed_unix_ms, created_at_unix_ms, decay_score, layer)
                 VALUES (?1, 'insight', ?1, '{\"content\":\"x\"}', 'insight', 'active', 0, 0, 0, 0.5, 'working')",
                params![id],
            )
        };

        let mut handles = Vec::new();
        // 4 writer threads, each inserting 50 rows through the shared Arc<Database>.
        for w in 0..4 {
            let d = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                for i in 0..50 {
                    let conn = d.conn().expect("pooled connection");
                    insert(&conn, &format!("w{}-{:03}", w, i)).expect("concurrent insert");
                }
            }));
        }
        // 4 reader threads recalling concurrently (pure reads through the pool).
        for _ in 0..4 {
            let d = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    d.recall(&RecallParams {
                        limit: 5,
                        skip_side_effects: true,
                        ..RecallParams::default()
                    })
                    .expect("concurrent recall should not fail");
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread panicked");
        }

        // All 200 writer rows landed (no lost writes under concurrency).
        let count: i64 = db
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 200, "expected 200 rows after concurrent writes, got {}", count);

        let _ = fs::remove_file(&path);
    }

    // #207: duplicate ids in one batch bump only once (the property the
    // query-expansion path relies on after merging variant results).
    #[test]
    fn apply_recall_side_effects_dedupes_ids() {
        let (db, path) = temp_db();
        db.conn().unwrap()
            .execute(
                "INSERT INTO entities (id, category, key, body_json, type, status,
                    retrieval_count, last_accessed_unix_ms, created_at_unix_ms,
                    decay_score, layer)
                 VALUES ('dup-1', 'insight', 'dup', '{\"content\":\"x\"}', 'insight',
                    'active', 0, 0, 0, 0.5, 'buffer')",
                params![],
            )
            .unwrap();

        db.apply_recall_side_effects(&["dup-1".to_string(), "dup-1".to_string()])
            .unwrap();

        let n: i64 = db.conn().unwrap()
            .query_row(
                "SELECT retrieval_count FROM entities WHERE id = 'dup-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "duplicate ids in IN (...) must bump only once");

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
        let raw_body: String = db.conn().unwrap()
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
        db2.conn().unwrap().execute(
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

        let fused =
            crate::db::reciprocal_rank_fusion(&dense_results, &sparse_results, 60.0, 10, 1.0, None, 0);
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

    #[test]
    fn rrf_recency_boost_promotes_newer_entity() {
        // #235: an optional recency half-life folds a time-decay factor into the
        // fused score so a recent memory can outrank an older, more lexically /
        // semantically relevant hit. Default (None) must preserve the existing
        // relevance-only ranking exactly.
        let now = 1_000_000_000_000_i64;
        let day_ms = 24 * 60 * 60 * 1000;

        let mut old = make_entity("old", "insight", "old-key", r#"{"n":"old"}"#);
        old.created_at_unix_ms = now - 100 * day_ms; // 100 days old
        let mut fresh = make_entity("fresh", "insight", "fresh-key", r#"{"n":"fresh"}"#);
        fresh.created_at_unix_ms = now; // brand new

        // `old` is the more-relevant hit (rank 0); `fresh` trails it at rank 1.
        let dense = vec![(old, 0.99), (fresh, 0.80)];
        let sparse: Vec<(Entity, f64)> = vec![];

        // Relevance-only (default): the older, more-relevant entity ranks first.
        let baseline = crate::db::reciprocal_rank_fusion(&dense, &sparse, 60.0, 10, 1.0, None, now);
        assert_eq!(
            baseline[0].0.id, "old",
            "without recency, the top-relevance entity must win"
        );

        // With a 1-day half-life, the 100-day-old hit is decayed to ~0 and the
        // brand-new entity overtakes it.
        let hl = day_ms as f64 / 1000.0;
        let boosted =
            crate::db::reciprocal_rank_fusion(&dense, &sparse, 60.0, 10, 1.0, Some(hl), now);
        assert_eq!(
            boosted[0].0.id, "fresh",
            "recency boost must promote the newer entity"
        );

        // A non-positive half-life is treated as disabled (no-op) — same as None.
        let disabled =
            crate::db::reciprocal_rank_fusion(&dense, &sparse, 60.0, 10, 1.0, Some(0.0), now);
        assert_eq!(
            disabled[0].0.id, "old",
            "hl <= 0 must disable recency weighting"
        );
    }

    #[test]
    fn rrf_recency_never_penalizes_unset_created_at() {
        // Guard: an entity with an unset (<= 0) created_at_unix_ms has no age
        // signal, so the recency factor must be 1.0 (plain RRF), never ~0 — which
        // would silently drop it from results.
        let now = 1_000_000_000_000_i64;
        let mut unset = make_entity("unset", "insight", "k", r#"{"n":"x"}"#);
        unset.created_at_unix_ms = 0;
        let dense = vec![(unset, 0.5)];
        let sparse: Vec<(Entity, f64)> = vec![];

        let out = crate::db::reciprocal_rank_fusion(&dense, &sparse, 60.0, 10, 1.0, Some(1.0), now);
        let expected = 1.0 / (60.0 + 1.0); // rank-0 RRF, unscaled
        assert!(
            (out[0].1 - expected).abs() < 1e-12,
            "entity with unset created_at must not be recency-penalized"
        );
    }

    // ─── #247: relevance-aware, deterministic hybrid fusion ──────────────

    #[test]
    fn sparse_arm_weight_drops_empty_arm_and_subweights_a_firing_arm() {
        // An empty keyword arm (e.g. a paraphrase query whose content terms
        // matched nothing after stopword filtering) contributes nothing.
        assert_eq!(crate::db::sparse_arm_weight(0), 0.0);
        // A firing arm is fused below the dense arm's full weight (dense-primary),
        // so the keyword arm augments rather than overrides the semantic ranking.
        let w = crate::db::sparse_arm_weight(3);
        assert!(w > 0.0 && w < 1.0, "a firing keyword arm must be sub-unity, got {w}");
        // Weight depends only on whether the arm fired, not on how many hits.
        assert_eq!(crate::db::sparse_arm_weight(1), crate::db::sparse_arm_weight(9));
    }

    #[test]
    fn rrf_weak_sparse_arm_does_not_dilute_dense_ranking() {
        // Regression for #247 issue 1: a confident dense rank-1 must survive
        // fusion even when the keyword arm ranks a different entity first. With
        // the sparse arm dropped (weight 0), fusion reduces to the dense order.
        let want = make_entity("dense-top", "insight", "k1", r#"{"n":"a"}"#);
        let other = make_entity("dense-2", "insight", "k2", r#"{"n":"b"}"#);
        let noise = make_entity("kw-noise", "insight", "k3", r#"{"n":"c"}"#);

        let dense = vec![(want, 0.91), (other, 0.40)];
        // Keyword arm ranks an irrelevant entity first.
        let sparse = vec![(noise, 5.0)];

        let fused =
            crate::db::reciprocal_rank_fusion(&dense, &sparse, 60.0, 10, 0.0, None, 0);
        assert_eq!(
            fused[0].0.id, "dense-top",
            "a weight-0 keyword arm must not displace the dense rank-1 hit"
        );

        // With full weight, the unweighted-RRF behavior is preserved: a tie at
        // rank 0 between the two arms is broken deterministically by entity id.
        let want2 = make_entity("dense-top", "insight", "k1", r#"{"n":"a"}"#);
        let noise2 = make_entity("kw-noise", "insight", "k3", r#"{"n":"c"}"#);
        let dense2 = vec![(want2, 0.91)];
        let sparse2 = vec![(noise2, 5.0)];
        let tied = crate::db::reciprocal_rank_fusion(&dense2, &sparse2, 60.0, 10, 1.0, None, 0);
        // Both rank-0 in their arm → equal fused score → id tie-break (asc).
        assert_eq!(tied[0].0.id, "dense-top");
        assert_eq!(tied[1].0.id, "kw-noise");
    }

    #[test]
    fn rrf_tie_break_is_deterministic_by_id() {
        // Regression for #247 issue 2: equal fused scores must order by entity id,
        // not by the (randomly-seeded) HashMap iteration order. Run a fused query
        // many times over the same all-tied inputs; the order must never change.
        let mut dense = Vec::new();
        for i in 0..8 {
            dense.push((
                make_entity(&format!("e{i}"), "insight", &format!("k{i}"), r#"{"n":"x"}"#),
                0.5, // identical scores → identical RRF ranks → all tied
            ));
        }
        let first = crate::db::reciprocal_rank_fusion(&dense, &[], 60.0, 10, 0.0, None, 0);
        let order: Vec<String> = first.iter().map(|(e, _)| e.id.clone()).collect();
        // All scores equal, so id order must be ascending and stable.
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "all-tied results must be ordered by id ascending");
        for _ in 0..50 {
            let again = crate::db::reciprocal_rank_fusion(&dense, &[], 60.0, 10, 0.0, None, 0);
            let again_order: Vec<String> = again.iter().map(|(e, _)| e.id.clone()).collect();
            assert_eq!(again_order, order, "fused tie order must be deterministic");
        }
    }

    #[test]
    fn fts5_bm25_search_filters_stopwords_and_is_read_only() {
        // Regression for #247: the hybrid keyword arm drops stopwords (so a
        // natural-language paraphrase query doesn't match the whole corpus on
        // function words) and never mutates access state.
        let (db, path) = temp_db();
        db.remember(&make_entity(
            "espresso",
            "habit",
            "coffee",
            r#"{"note": "I drink a strong espresso every morning"}"#,
        ))
        .unwrap();
        db.remember(&make_entity(
            "bike",
            "habit",
            "commute",
            r#"{"note": "I usually bike to the office on weekdays"}"#,
        ))
        .unwrap();

        // All-stopword query ("does the user...") → no content terms → empty arm.
        let stop = db
            .fts5_bm25_search(&RecallParams {
                query: "does the user have any".to_string(),
                limit: 10,
                ..RecallParams::default()
            })
            .unwrap();
        assert!(
            stop.is_empty(),
            "a query of only stopwords must yield no keyword matches, got {:?}",
            stop.iter().map(|(e, _)| &e.id).collect::<Vec<_>>()
        );

        // A content term still matches, and scores are relevance (higher better).
        let hit = db
            .fts5_bm25_search(&RecallParams {
                query: "the espresso".to_string(),
                limit: 10,
                ..RecallParams::default()
            })
            .unwrap();
        assert!(
            hit.iter().any(|(e, _)| e.id == "espresso"),
            "content term 'espresso' must match its memory"
        );

        // Read-only: retrieval_count must be unchanged after the keyword search.
        let count: i64 = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT retrieval_count FROM entities WHERE id = 'espresso'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "fts5_bm25_search must not mutate retrieval_count");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn hybrid_recall_is_read_only_and_idempotent() {
        // Regression for #247: like dense mode, hybrid recall issues no
        // access-state writes — repeated recalls return identical results and
        // never bump retrieval_count/last_accessed/decay. A caller-supplied query
        // embedding drives the dense arm so the test needs no ONNX backend.
        let (db, path) = temp_db();
        let blob = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
        let insert = |id: &str, key: &str, body: &str, emb: &[f32]| {
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO entities (id, category, key, body_json, type, status,
                        retrieval_count, last_accessed_unix_ms, created_at_unix_ms,
                        decay_score, layer, embedding, archived)
                     VALUES (?1, 'insight', ?2, ?3, 'insight', 'active', 0, 0, 0, 1.0, 'working', ?4, 0)",
                    params![id, key, body, blob(emb)],
                )
                .unwrap();
            // Keep the FTS index in sync so the keyword arm can match.
            db.conn()
                .unwrap()
                .execute(
                    "INSERT INTO entities_fts (rowid, body_json)
                     VALUES ((SELECT rowid FROM entities WHERE id = ?1), ?2)",
                    params![id, body],
                )
                .unwrap();
        };
        insert("e-coffee", "coffee", r#"{"note":"espresso every morning"}"#, &[1.0, 0.0, 0.0]);
        insert("e-bike", "commute", r#"{"note":"bike to the office"}"#, &[0.0, 1.0, 0.0]);

        let params = RecallParams {
            query: "espresso".to_string(), // exercises the keyword arm too
            mode: crate::models::SearchMode::Hybrid,
            embedding: Some(vec![1.0, 0.0, 0.0]),
            limit: 10,
            ..RecallParams::default()
        };

        let first = db.recall(&params).unwrap();
        assert!(
            first.iter().any(|e| e.id == "e-coffee"),
            "hybrid recall should surface the matching entity"
        );

        // No access-state writes: counts/timestamps stay at their seeded zero.
        let (rc, la): (i64, i64) = db
            .conn()
            .unwrap()
            .query_row(
                "SELECT retrieval_count, last_accessed_unix_ms FROM entities WHERE id = 'e-coffee'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rc, 0, "hybrid recall must not bump retrieval_count");
        assert_eq!(la, 0, "hybrid recall must not touch last_accessed_unix_ms");

        // Idempotent: a second identical recall returns the same ordering.
        let second = db.recall(&params).unwrap();
        let ids1: Vec<&str> = first.iter().map(|e| e.id.as_str()).collect();
        let ids2: Vec<&str> = second.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids1, ids2, "repeated hybrid recall must be idempotent");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn handle_extract_from_text_returns_structured_items() {
        // #234: the mimir_extract tool runs the local rule-based extractor over
        // provided text and returns structured items without touching the store.
        let (db, _path) = temp_db();
        let args = serde_json::json!({
            "text": "The database is PostgreSQL. I prefer dark mode."
        });
        let out = crate::tools::handle_extract(&db, args).expect("extract should succeed");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["strategy"], "rule_based");
        let kinds: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"fact"));
        assert!(kinds.contains(&"preference"));
    }

    #[test]
    fn handle_extract_strategy_none_is_noop() {
        let (db, _path) = temp_db();
        let args = serde_json::json!({"text": "The database is PostgreSQL.", "strategy": "none"});
        let out = crate::tools::handle_extract(&db, args).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["total"], 0);
    }

    #[test]
    fn handle_extract_from_stored_entity_and_missing_errors() {
        let (db, _path) = temp_db();
        // Store an entity, then extract from it by category/key.
        crate::tools::handle_remember(
            &db,
            serde_json::json!({
                "category": "notes",
                "key": "stack",
                "body_json": "{\"content\": \"The service uses OAuth 2.0. We shipped on 2026-06-20.\"}"
            }),
        )
        .expect("remember should succeed");

        let out = crate::tools::handle_extract(
            &db,
            serde_json::json!({"category": "notes", "key": "stack"}),
        )
        .expect("extract from entity should succeed");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["total"].as_i64().unwrap() >= 1, "should extract at least one item");

        // Missing entity surfaces a clear error rather than empty success.
        let err = crate::tools::handle_extract(
            &db,
            serde_json::json!({"category": "notes", "key": "nope"}),
        );
        assert!(err.is_err(), "missing entity must error");

        // Neither text nor category+key is a usage error.
        assert!(crate::tools::handle_extract(&db, serde_json::json!({})).is_err());
    }

    #[test]
    fn handle_ingest_file_stores_and_recalls_plaintext_document() {
        // #236: mimir_ingest_file extracts a document's text locally and stores it
        // as a normal, recallable entity. Plaintext works without the multimodal
        // feature.
        let (db, _path) = temp_db();
        let p = std::env::temp_dir().join(format!("mimir-ingest-{}.md", uuid::Uuid::new_v4()));
        std::fs::write(&p, "# Notes\n\nThe widget API uses cursor pagination.").unwrap();

        let out = crate::tools::handle_ingest_file(
            &db,
            serde_json::json!({ "path": p.to_string_lossy(), "category": "docs" }),
        )
        .expect("ingest_file should succeed");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["category"], "docs");
        assert!(v["chars"].as_i64().unwrap() > 0);

        let found = db
            .recall(&crate::models::RecallParams {
                query: "cursor pagination".to_string(),
                limit: 5,
                ..Default::default()
            })
            .unwrap();
        assert!(
            found.iter().any(|e| e.body_json.contains("cursor pagination")),
            "ingested document must be recallable"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    #[ignore] // Requires ~10s to create entities; run manually with --ignored
    fn stress_100k_entities_recall_and_decay() {
        // Roadmap target: FTS5 recall < 5s, decay tick < 30s at 100K entities.
        use std::time::Instant;

        let (db, _path) = temp_db();

        // Insert entities in transactions of 5000 for reasonable speed.
        let n = 100_000;
        let start_insert = Instant::now();
        let mut count = 0;
        for chunk in (0..n).collect::<Vec<_>>().chunks(5000) {
            let conn = db.conn().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            for i in chunk {
                let i = *i;
                let key = format!("entity-{:06}", i);
                let body = format!(
                    r#"{{"content":"Entity number {} with some searchable text {} {} {}"}}"#,
                    i,
                    if i % 3 == 0 { "alpha" } else { "" },
                    if i % 5 == 0 { "beta" } else { "" },
                    if i % 7 == 0 { "gamma" } else { "" }
                );
                tx.execute(
                    "INSERT INTO entities (id, category, key, body_json, type, status, retrieval_count, last_accessed_unix_ms, created_at_unix_ms, decay_score, layer)
                     VALUES (?1, 'benchmark', ?2, ?3, 'insight', 'active', 0, 0, 0, 0.5, 'working')",
                    params![format!("ent-{:06}", i), key, body],
                ).unwrap();
                // Keep FTS5 in sync
                tx.execute(
                    "INSERT INTO entities_fts(rowid, body_json) VALUES (last_insert_rowid(), ?1)",
                    params![body],
                ).unwrap();
            }
            tx.commit().unwrap();
            count += chunk.len();
        }
        let insert_elapsed = start_insert.elapsed();
        eprintln!(
            "Inserted {} entities in {:.2}s ({:.0} entities/s)",
            count,
            insert_elapsed.as_secs_f64(),
            count as f64 / insert_elapsed.as_secs_f64()
        );

        // ── RECALL benchmark ──
        let start_recall = Instant::now();
        let results = db.fts5_search(
            &crate::models::RecallParams {
                query: "alpha".to_string(),
                category: None,
                entity_type: None,
                limit: 10,
                offset: 0,
                min_decay: 0.0,
                topic_path: None,
                include_archived: false,
                skip_side_effects: true,
                mode: crate::models::SearchMode::Fts5,
                embedding: None,
                preview_cap: None,
                always_on: None,
                content_weight: 0.0,
                trust_weight: 0.0,
                diversity_halving: 1.0,
                diversity_per_query_share: 0.0,
                recency_half_life_secs: None,
                workspace_hash: None,
            agent_id: None,
            visibility: None,
            },
        )
        .unwrap();
        let recall_elapsed = start_recall.elapsed();
        eprintln!(
            "FTS5 recall returned {} results in {:.3}s",
            results.len(),
            recall_elapsed.as_secs_f64()
        );
        assert!(
            !results.is_empty(),
            "expected at least one result for 'alpha' query"
        );
        assert!(
            recall_elapsed.as_secs_f64() < 5.0,
            "FTS5 recall took {:.3}s — roadmap target is < 5s",
            recall_elapsed.as_secs_f64()
        );

        // ── DECAY benchmark ──
        let start_decay = Instant::now();
        let report = db
            .decay_tick_with_limit(Some(n as i64))
            .expect("decay_tick should succeed at 100K scale");
        let decay_elapsed = start_decay.elapsed();
        eprintln!(
            "Decay tick: updated {} / auto-archived {} / total {} in {:.3}s",
            report.entities_updated, report.auto_archived, report.entities_checked, decay_elapsed.as_secs_f64()
        );
        assert_eq!(report.entities_checked, n as i64);
        assert!(
            decay_elapsed.as_secs_f64() < 30.0,
            "decay tick took {:.3}s — roadmap target is < 30s",
            decay_elapsed.as_secs_f64()
        );

        eprintln!("STRESS TEST PASSED at {} entities", n);
    }


    #[test]
    fn concurrent_reader_writer_no_locks() {
        // Roadmap: verify no "database is locked" errors with concurrent r/w.
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::thread;

        let (db, db_path) = temp_db();

        // Pre-populate 1000 entities so the reader has something to search.
        {
            let conn = db.conn().unwrap();
            let tx = conn.unchecked_transaction().unwrap();
            for i in 0..1000u32 {
                let body = format!(r#"{{"content":"entity {}"}}"#, i);
                tx.execute(
                    "INSERT INTO entities (id, category, key, body_json, type, status, retrieval_count, last_accessed_unix_ms, created_at_unix_ms, decay_score, layer)
                     VALUES (?1, 'stress', ?2, ?3, 'insight', 'active', 0, 0, 0, 0.5, 'working')",
                    params![format!("ent-pre-{:04}", i), format!("key-{}", i), body],
                ).unwrap();
                tx.execute(
                    "INSERT INTO entities_fts(rowid, body_json) VALUES (last_insert_rowid(), ?1)",
                    params![body],
                ).unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db); // close the temp_db connection so the second connection can open fresh

        // Barrier: both threads start together after setup.
        let barrier = Arc::new(Barrier::new(2));
        let reader_failures = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let writer_failures = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let writer_path = db_path.clone();
        let writer_barrier = Arc::clone(&barrier);
        let wf = Arc::clone(&writer_failures);
        let writer = thread::spawn(move || {
            let wdb = crate::db::Database::open(&writer_path).expect("writer db open");
            writer_barrier.wait();
            for i in 0..500 {
                let body = format!(r#"{{"content":"writer entity {}"}}"#, i);
                if let Err(e) = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let conn = wdb.conn().expect("pool conn");
                    let tx = conn.unchecked_transaction()?;
                    tx.execute(
                        "INSERT INTO entities (id, category, key, body_json, type, status, retrieval_count, last_accessed_unix_ms, created_at_unix_ms, decay_score, layer)
                         VALUES (?1, 'stress', ?2, ?3, 'insight', 'active', 0, 0, 0, 0.5, 'working')",
                        params![format!("ent-wr-{:04}", i), format!("wkey-{}", i), body],
                    )?;
                    tx.execute(
                        "INSERT INTO entities_fts(rowid, body_json) VALUES (last_insert_rowid(), ?1)",
                        params![body],
                    )?;
                    tx.commit()?;
                    Ok(())
                })() {
                    eprintln!("writer error at {}: {}", i, e);
                    wf.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });

        let reader_path = db_path.clone();
        let reader_barrier = Arc::clone(&barrier);
        let rf = Arc::clone(&reader_failures);
        let reader = thread::spawn(move || {
            let rdb = crate::db::Database::open(&reader_path).expect("reader db open");
            reader_barrier.wait();
            for _ in 0..100 {
                match rdb.fts5_search(&crate::models::RecallParams {
                    query: "entity".to_string(),
                    category: Some("stress".to_string()),
                    entity_type: None,
                    limit: 10i64,
                    offset: 0i64,
                    min_decay: 0.0f64,
                    topic_path: None,
                    include_archived: false,
                    skip_side_effects: true,
                    mode: crate::models::SearchMode::Fts5,
                    embedding: None,
                    preview_cap: None,
                    always_on: None,
                    content_weight: 0.0f64,
                    trust_weight: 0.0f64,
                    diversity_halving: 1.0f64,
                    diversity_per_query_share: 0.0f64,
                    recency_half_life_secs: None,
                    workspace_hash: None,
                    agent_id: None,
                    visibility: None,
                }) {
                    Ok(_) => {},
                    Err(e) => {
                        eprintln!("reader error: {}", e);
                        rf.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });

        writer.join().expect("writer thread panicked");
        reader.join().expect("reader thread panicked");

        let writer_errs = writer_failures.load(std::sync::atomic::Ordering::Relaxed);
        let reader_errs = reader_failures.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("Concurrent test: writer errors={}, reader errors={}", writer_errs, reader_errs);
        assert_eq!(writer_errs, 0, "writer should have zero errors with concurrent reader");
        assert_eq!(reader_errs, 0, "reader should have zero 'database is locked' errors");
    }


    #[test]
    fn workspace_scoping_isolates_entities() {
        // Roadmap Phase 2 Week 1-3: Agent A's memories invisible to Agent B.
        let (db, _path) = temp_db();

        // Entity in workspace "alpha"
        let mut ent_a = make_entity("ws-a", "shared", "secret", r#"{"content":"alpha apple apricot avocado data one"}"#);
        ent_a.workspace_hash = "alpha".to_string();
        db.remember(&ent_a).unwrap();

        // Entity in workspace "beta"
        let mut ent_b = make_entity("ws-b", "shared", "secret-beta", r#"{"content":"beta banana blueberry cherry data two"}"#);
        ent_b.workspace_hash = "beta".to_string();
        db.remember(&ent_b).unwrap();

        // Global entity (no workspace)
        let ent_g = make_entity("ws-g", "shared", "global-key", r#"{"content":"gamma grape guava melon data three"}"#);
        db.remember(&ent_g).unwrap();

        let base = |ws: Option<String>| crate::models::RecallParams {
            query: "data".to_string(),
            category: None,
            entity_type: None,
            limit: 50,
            offset: 0,
            min_decay: 0.0,
            topic_path: None,
            include_archived: false,
            skip_side_effects: true,
            mode: crate::models::SearchMode::Fts5,
            embedding: None,
            preview_cap: None,
            always_on: None,
            content_weight: 0.0,
            trust_weight: 0.0,
            diversity_halving: 1.0,
            diversity_per_query_share: 0.0,
            recency_half_life_secs: None,
            workspace_hash: ws,
            agent_id: None,
            visibility: None,
        };

        // Scope to "alpha" — should only see ent_a
        let alpha = db.recall(&base(Some("alpha".to_string()))).unwrap();
        let alpha_keys: Vec<&str> = alpha.iter().map(|e| e.key.as_str()).collect();
        assert!(alpha_keys.contains(&"secret"), "alpha scope should see its own entity");
        assert!(!alpha_keys.contains(&"secret-beta"), "alpha scope must NOT see beta entity");
        assert!(!alpha_keys.contains(&"global-key"), "alpha scope must NOT see global entity (scoped query)");

        // Scope to "beta" — should only see ent_b
        let beta = db.recall(&base(Some("beta".to_string()))).unwrap();
        let beta_keys: Vec<&str> = beta.iter().map(|e| e.key.as_str()).collect();

        assert!(beta_keys.contains(&"secret-beta"), "beta scope should see its own entity");
        assert!(!beta_keys.contains(&"secret"), "beta scope must NOT see alpha entity");

        // No scope — sees everything
        let all = db.recall(&base(None)).unwrap();
        let all_keys: Vec<&str> = all.iter().map(|e| e.key.as_str()).collect();
        assert!(all_keys.contains(&"secret"), "unscoped recall sees alpha");
        assert!(all_keys.contains(&"secret-beta"), "unscoped recall sees beta");
        assert!(all_keys.contains(&"global-key"), "unscoped recall sees global");
    }

    #[test]
    fn workspace_hash_roundtrips_through_recall() {
        // workspace_hash must survive the store→recall roundtrip (was a latent
        // bug: always_on/certainty were silently dropped by short SELECT lists).
        let (db, _path) = temp_db();
        let mut ent = make_entity("rt-1", "rt", "key1", r#"{"content":"roundtrip"}"#);
        ent.workspace_hash = "myworkspace".to_string();
        ent.always_on = true;
        ent.certainty = 0.9;
        db.remember(&ent).unwrap();

        let params = crate::models::RecallParams {
            query: "roundtrip".to_string(),
            category: None, entity_type: None, limit: 10, offset: 0, min_decay: 0.0,
            topic_path: None, include_archived: false, skip_side_effects: true,
            mode: crate::models::SearchMode::Fts5, embedding: None, preview_cap: None,
            always_on: None, content_weight: 0.0, trust_weight: 0.0, diversity_halving: 1.0,
            diversity_per_query_share: 0.0, recency_half_life_secs: None, workspace_hash: None,
            agent_id: None,
            visibility: None,
        };
        let results = db.recall(&params).unwrap();
        let found = results.iter().find(|e| e.key == "key1").expect("entity recalled");
        assert_eq!(found.workspace_hash, "myworkspace", "workspace_hash must roundtrip");
        assert!(found.always_on, "always_on must roundtrip (regression: short SELECT dropped it)");
        assert_eq!(found.certainty, 0.9, "certainty must roundtrip");
    }


    #[test]
    fn agent_id_filters_in_recall() {
        // Phase 2 Week 4-6: entities tagged with agent_id are filterable.
        let (db, _path) = temp_db();

        let mut ent_a = make_entity("aid-a", "agents", "agent-a-key", r#"{"content":"alpha agent xyzzy unique data"}"#);
        ent_a.agent_id = "squad-leader".to_string();
        db.remember(&ent_a).unwrap();

        let mut ent_b = make_entity("aid-b", "agents", "agent-b-key", r#"{"content":"beta agent plugh distinct info"}"#);
        ent_b.agent_id = "scout".to_string();
        db.remember(&ent_b).unwrap();

        // No filter — sees both
        let all = db.recall(&crate::models::RecallParams {
            query: "agent".to_string(), agent_id: None,
            ..crate::models::RecallParams::default()
        }).unwrap();
        let all_keys: Vec<_> = all.iter().map(|e| e.key.as_str()).collect();
        assert!(all_keys.contains(&"agent-a-key"));
        assert!(all_keys.contains(&"agent-b-key"));

        // Filter by "squad-leader" — only sees ent_a
        let squad = db.recall(&crate::models::RecallParams {
            query: "agent".to_string(), agent_id: Some("squad-leader".to_string()),
            ..crate::models::RecallParams::default()
        }).unwrap();
        let squad_keys: Vec<_> = squad.iter().map(|e| e.key.as_str()).collect();
        assert!(squad_keys.contains(&"agent-a-key"));
        assert!(!squad_keys.contains(&"agent-b-key"));
        assert_eq!(squad.len(), 1);
    }

    #[test]
    fn agent_id_roundtrips() {
        let (db, _path) = temp_db();
        let mut ent = make_entity("rt-aid", "agents", "k", r#"{"content":"roundtrip"}"#);
        ent.workspace_hash = "scope".to_string();
        ent.agent_id = "secret-agent-man".to_string();
        db.remember(&ent).unwrap();

        let results = db.recall(&crate::models::RecallParams {
            query: "roundtrip".to_string(),
            ..crate::models::RecallParams::default()
        }).unwrap();
        let found = results.iter().find(|e| e.key == "k").unwrap();
        assert_eq!(found.agent_id, "secret-agent-man");
        assert_eq!(found.workspace_hash, "scope");
    }

    #[test]
    fn journal_agent_attribution() {
        let (db, _path) = temp_db();
        let event = crate::models::JournalEvent {
            id: "jrn-agent-1".to_string(),
            event_type: "test".to_string(),
            evaluated_json: "{}".to_string(),
            acted_json: "{}".to_string(),
            forward_json: "{}".to_string(),
            category: "test".to_string(),
            key: "t1".to_string(),
            entity_id: String::new(),
            agent_id: "security-bot".to_string(),
            created_at_unix_ms: now_ms(),
        };
        db.journal(&event).unwrap();

        let events = db.timeline(&crate::models::TimelineParams::default()).unwrap();
        assert!(!events.is_empty());
        let found = events.iter().find(|e| e.id == "jrn-agent-1").unwrap();
        assert_eq!(found.agent_id, "security-bot");
    }

}

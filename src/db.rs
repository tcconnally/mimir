use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::models::{
    CompactReport, DecayReport, Entity, JournalEvent, MemoryLink, RecallParams, StateEntry, Stats,
    TimelineParams, VaultReport,
};
use crate::schema;

/// Format a unix timestamp in milliseconds as an ISO 8601 UTC string.
/// Produces a human-readable date like "2026-06-12T10:58:00Z".
fn chrono_like(ms: i64) -> String {
    let secs = ms / 1000;
    // M-5: emit actual ISO 8601 UTC instead of raw epoch seconds.
    // Avoids chrono dependency by hand-rolling a minimal formatter.
    // Only safe for timestamps from 1970 to ~3000 (no leap-second handling).
    if secs <= 0 {
        return format!("{}", secs); // Unix epoch 0 or negative — return as-is
    }
    let days_since_epoch = secs / 86400;
    let secs_of_day = secs % 86400;
    // Convert days since 1970-01-01 to year/month/day
    let mut y = 1970i64;
    let mut d = days_since_epoch;
    loop {
        let days_in_year = if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) { 366 } else { 365 };
        if d < days_in_year { break; }
        d -= days_in_year;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    while m < 12 && d >= month_days[m] {
        d -= month_days[m];
        m += 1;
    }
    let month = m + 1;
    let day = d + 1;
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, month, day, h, min, s)
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
}

impl Database {
    /// Open a database at `path`, initializing the v0.2.0 schema if needed.
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;

        // Enable WAL for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        // Initialize schema if this is a new database
        schema::initialize_schema(&conn)?;

        Ok(Database {
            conn,
            db_path: path.to_string(),
        })
    }

    /// Simple health check — verify the DB responds.
    pub fn health_check(&self) -> bool {
        self.conn.query_row("SELECT 1", [], |_| Ok(())).is_ok()
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
        let now = now_ms();
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entities WHERE archived = 0",
            [],
            |r| r.get(0),
        )?;

        // Update decay_score for all non-archived entities
        let mut stmt = self
            .conn
            .prepare("SELECT id, last_accessed_unix_ms FROM entities WHERE archived = 0")?;
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
        let mut stmt = self.conn.prepare(
            "SELECT id, body_json FROM entities WHERE category = ?1 AND archived = 0 LIMIT 100",
        )?;
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
                    retrieval_count = retrieval_count + 1
                 WHERE id = ?14",
                params![
                    entity.body_json,
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
                  created_at_unix_ms, last_accessed_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                         ?8, ?9, ?10, ?11,
                         ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18)",
                params![
                    id,
                    entity.category,
                    entity.key,
                    entity.body_json,
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
                // FTS5 query: escape special chars and quote each term
                let escape_fts = |s: &str| -> String {
                    s.chars()
                        .map(|c| match c {
                            '"' | '\'' | '+' | '-' | '*' | '^' | '(' | ')' | '[' | ']' | '{'
                            | '}' | '~' | '!' | '@' | '#' | '$' | '%' | '&' | '/' | ':' | '<'
                            | '>' | '=' | '|' | '?' | ',' | '.' | '`' | ';' | '\\' => ' '.into(),
                            _ => c.to_string(),
                        })
                        .collect::<String>()
                        .trim()
                        .to_string()
                };
                let fts_query = words
                    .iter()
                    .map(|w| {
                        let escaped = escape_fts(w);
                        if escaped.is_empty() {
                            "\"\"".to_string()
                        } else {
                            format!("\"{}\"", escaped.replace('"', "\"\""))
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

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            entity_from_row(row)
        })?;

        let mut items = Vec::new();
        for row in rows {
            let entity = row?;
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
            items.push(entity);
        }

        Ok(items)
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
            entity_from_row(row)
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
        let root = self.get_entity(category, key)?
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
        self._traverse_links(&root.id, &mut traversed, &mut visited, max_depth, max_nodes, 0);

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

                self._traverse_links(&entity.id, traversed, visited, max_depth, max_nodes, current_depth + 1);
                }
                Ok(None) => {
                    // Dangling link — target entity no longer exists
                }
                Err(e) => {
                    eprintln!("mimir: traverse error reading entity {}: {}", link.target_id, e);
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
            entity_from_row(row)
        })?;
        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
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
    pub fn detect_conflicts(
        &self,
        category: &str,
        threshold: f64,
        limit: i64,
        offset: i64,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, key, body_json FROM entities WHERE category = ?1 AND archived = 0
             ORDER BY last_accessed_unix_ms DESC LIMIT 200 OFFSET ?2"
        )?;
        let rows = stmt.query_map(params![category, offset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;

        let entities: Vec<(String, String, String)> = rows.filter_map(|r| r.ok()).collect();
        let mut conflicts = Vec::new();

        for i in 0..entities.len() {
            for j in (i + 1)..entities.len() {
                let (ref id1, ref key1, ref body1) = entities[i];
                let (ref id2, ref key2, ref body2) = entities[j];
                let sim = Self::trigram_similarity(body1, body2);
                if sim < threshold {
                    conflicts.push(serde_json::json!({
                        "entity_a": {"id": id1, "key": key1},
                        "entity_b": {"id": id2, "key": key2},
                        "similarity": sim,
                        "conflict_likely": sim < 0.3
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
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
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
                    format!("mem-{}", &raw[..12])
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
                created_at_unix_ms: now_ms(),
                last_accessed_unix_ms: now_ms(),
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
        ctx.push_str(&format!("\n> {} entities recalled\n", all_entities.len()));

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
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len).collect();
        format!("{}...", truncated)
    }
}

/// Extract an Entity from a SQLite row (shared across recall, get_entity, get_entity_by_id).
fn entity_from_row(row: &rusqlite::Row) -> rusqlite::Result<crate::models::Entity> {
    use crate::models::{Entity, MemoryLink};
    let tags_str: String = row.get::<_, String>(6).unwrap_or_else(|_| "[]".to_string());
    let links_str: String = row.get::<_, String>(13).unwrap_or_else(|_| "[]".to_string());
    let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
    let links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
    let archived: i32 = row.get(11).unwrap_or(0);
    let verified: i32 = row.get(14).unwrap_or(0);

    Ok(Entity {
        id: row.get(0)?,
        category: row.get(1)?,
        key: row.get(2)?,
        body_json: row.get(3)?,
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
        created_at_unix_ms: row.get(16)?,
        last_accessed_unix_ms: row.get(17)?,
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
            created_at_unix_ms: now_ms(),
            last_accessed_unix_ms: now_ms(),
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
}

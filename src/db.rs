use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLink {
    pub target_id: String,
    pub relationship: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub content: String,
    #[serde(rename = "type")]
    pub memory_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub relevance: f64,
    pub decay_score: f64,
    pub retrieval_count: i64,
    pub layer: String,
    pub topic_path: String,
    pub created_at_unix_ms: i64,
    pub last_accessed_unix_ms: i64,
    pub links: Vec<MemoryLink>,
    pub workspace_hash: String,
    pub tags: serde_json::Value,
    pub source: String,
    pub verified: bool,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                type TEXT NOT NULL DEFAULT 'insight',
                summary TEXT DEFAULT '',
                relevance REAL DEFAULT 0.0,
                decay_score REAL DEFAULT 1.0,
                retrieval_count INTEGER DEFAULT 0,
                layer TEXT DEFAULT 'working',
                topic_path TEXT DEFAULT '',
                created_at_unix_ms INTEGER NOT NULL,
                last_accessed_unix_ms INTEGER NOT NULL,
                workspace_hash TEXT DEFAULT '',
                tags TEXT DEFAULT '{}',
                links TEXT DEFAULT '[]',
                source TEXT DEFAULT 'mneme',
                verified INTEGER DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                content_rowid='rowid'
            );",
        )?;

        Ok(Database { conn })
    }

    pub fn health_check(&self) -> bool {
        self.conn
            .execute_batch("SELECT 1 FROM memories LIMIT 1")
            .is_ok()
    }

    pub fn store(&self, item: &MemoryItem) -> Result<(), Box<dyn std::error::Error>> {
        let tags_json = serde_json::to_string(&item.tags)?;
        let links_json = serde_json::to_string(&item.links)?;
        let summary = item.summary.as_deref().unwrap_or("");

        self.conn.execute(
            "INSERT INTO memories (id, content, type, summary, relevance, decay_score,
             retrieval_count, layer, topic_path, created_at_unix_ms, last_accessed_unix_ms,
             workspace_hash, tags, links, source, verified)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                item.id,
                item.content,
                item.memory_type,
                summary,
                item.relevance,
                item.decay_score,
                item.retrieval_count,
                item.layer,
                item.topic_path,
                item.created_at_unix_ms,
                item.last_accessed_unix_ms,
                item.workspace_hash,
                tags_json,
                links_json,
                item.source,
                item.verified as i32,
            ],
        )?;

        // Also insert into FTS index
        self.conn.execute(
            "INSERT INTO memories_fts (rowid, content) VALUES (last_insert_rowid(), ?1)",
            params![item.content],
        )?;

        Ok(())
    }

    // Mirrors MCP recall parameters; keep this flat until a request type exists.
    #[allow(clippy::too_many_arguments)]
    pub fn recall(
        &self,
        query: &str,
        memory_types: &[String],
        max_results: i64,
        workspace_hash: &Option<String>,
        _include_federation: bool,
        _filters: &Option<serde_json::Value>,
        min_decay_score: f64,
        topic_path: &Option<String>,
    ) -> Result<Vec<MemoryItem>, Box<dyn std::error::Error>> {
        // Build conditions and parameters incrementally
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        // Keyword search via FTS5 + LIKE fallback with OR semantics
        if !query.is_empty() {
            let words: Vec<&str> = query.split_whitespace().collect();

            // FTS5 query: join words with OR
            let fts_query = words
                .iter()
                .map(|w| w.replace('\'', "''"))
                .collect::<Vec<_>>()
                .join(" OR ");

            // LIKE fallback: match any word as substring
            let mut like_parts = Vec::new();
            for _word in &words {
                like_parts.push(format!(
                    "content LIKE ?{}",
                    param_values.len() + like_parts.len() + 2
                ));
            }
            // Push FTS query first, then LIKE params
            param_values.push(Box::new(fts_query));
            for word in &words {
                param_values.push(Box::new(format!("%{}%", word.replace('\'', "''"))));
            }

            let fts_placeholder = param_values.len() - words.len() - 1 + 1; // the position of fts_query
            conditions.push(format!(
                "((id IN (SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?{})) OR {})",
                fts_placeholder,
                like_parts.join(" OR ")
            ));
        }

        // Filter by memory types
        if !memory_types.is_empty() {
            let placeholders: Vec<String> = (0..memory_types.len())
                .map(|i| format!("?{}", param_values.len() + i + 1))
                .collect();
            conditions.push(format!("type IN ({})", placeholders.join(",")));
            for t in memory_types {
                param_values.push(Box::new(t.clone()));
            }
        }

        // Filter by workspace hash
        if let Some(ref wh) = workspace_hash {
            if !wh.is_empty() {
                conditions.push(format!("workspace_hash = ?{}", param_values.len() + 1));
                param_values.push(Box::new(wh.clone()));
            }
        }

        // Filter by decay score
        if min_decay_score > 0.0 {
            conditions.push(format!("decay_score >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(min_decay_score));
        }

        // Filter by topic path
        if let Some(ref tp) = topic_path {
            if !tp.is_empty() {
                conditions.push(format!("topic_path LIKE ?{}", param_values.len() + 1));
                param_values.push(Box::new(format!("{}%", tp)));
            }
        }

        // Build final SQL
        let mut sql = String::from(
            "SELECT id, content, type, summary, relevance, decay_score,
             retrieval_count, layer, topic_path, created_at_unix_ms,
             last_accessed_unix_ms, links, workspace_hash, tags, source, verified
             FROM memories",
        );

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        sql.push_str(" ORDER BY retrieval_count DESC, last_accessed_unix_ms DESC");

        sql.push_str(&format!(" LIMIT ?{}", param_values.len() + 1));
        param_values.push(Box::new(max_results));

        // Build param refs
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let links_str: String = row
                .get::<_, String>(11)
                .unwrap_or_else(|_| "[]".to_string());
            let tags_str: String = row
                .get::<_, String>(13)
                .unwrap_or_else(|_| "{}".to_string());
            let links: Vec<MemoryLink> = serde_json::from_str(&links_str).unwrap_or_default();
            let tags: serde_json::Value = serde_json::from_str(&tags_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            Ok(MemoryItem {
                id: row.get(0)?,
                content: row.get(1)?,
                memory_type: row.get(2)?,
                summary: {
                    let s: String = row.get::<_, String>(3).unwrap_or_default();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                },
                relevance: row.get(4)?,
                decay_score: row.get(5)?,
                retrieval_count: row.get(6)?,
                layer: row.get(7)?,
                topic_path: row.get(8)?,
                created_at_unix_ms: row.get(9)?,
                last_accessed_unix_ms: row.get(10)?,
                links,
                workspace_hash: row.get(12)?,
                tags,
                source: row.get(14)?,
                verified: row.get::<_, i32>(15)? != 0,
            })
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }

        // Update retrieval counts for found items
        for item in &items {
            let _ = self.conn.execute(
                "UPDATE memories SET retrieval_count = retrieval_count + 1, last_accessed_unix_ms = ?1 WHERE id = ?2",
                params![now_ms(), item.id],
            );
            let new_relevance = (item.relevance + 0.05).min(1.0);
            let _ = self.conn.execute(
                "UPDATE memories SET relevance = ?1 WHERE id = ?2",
                params![new_relevance, item.id],
            );
        }

        Ok(items)
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

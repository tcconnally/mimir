use rusqlite::{params, Connection};
use serde_json::json;

use crate::db::now_ms;
use crate::models::{MigrationReport, Stats};

/// SQL to create the v0.2.0 schema from scratch.
pub const DDL_V0_2_0: &str = "
CREATE TABLE IF NOT EXISTS entities (
    id TEXT PRIMARY KEY,
    category TEXT NOT NULL DEFAULT 'general',
    key TEXT NOT NULL,
    body_json TEXT NOT NULL DEFAULT '{}',
    status TEXT DEFAULT 'active',
    type TEXT DEFAULT 'insight',
    tags TEXT DEFAULT '[]',
    decay_score REAL DEFAULT 1.0,
    retrieval_count INTEGER DEFAULT 0,
    layer TEXT DEFAULT 'working',
    topic_path TEXT DEFAULT '',
    archived INTEGER DEFAULT 0,
    archive_reason TEXT DEFAULT '',
    links TEXT DEFAULT '[]',
    verified INTEGER DEFAULT 0,
    source TEXT DEFAULT 'agent',
    created_at_unix_ms INTEGER NOT NULL,
    last_accessed_unix_ms INTEGER NOT NULL,
    embedding BLOB,
    always_on INTEGER DEFAULT 0,
    certainty REAL DEFAULT 0.5,
    workspace_hash TEXT DEFAULT '',
    agent_id TEXT DEFAULT ''
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_category_key ON entities(category, key);

CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(body_json, content_rowid='rowid');

CREATE TABLE IF NOT EXISTS journal (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL DEFAULT 'decision',
    evaluated_json TEXT DEFAULT '{}',
    acted_json TEXT DEFAULT '{}',
    forward_json TEXT DEFAULT '{}',
    category TEXT DEFAULT '',
    key TEXT DEFAULT '',
    entity_id TEXT DEFAULT '',
    agent_id TEXT DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_journal_created ON journal(created_at_unix_ms);
CREATE INDEX IF NOT EXISTS idx_journal_entity ON journal(entity_id);

CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL DEFAULT '{}',
    expires_at_unix_ms INTEGER,
    created_at_unix_ms INTEGER NOT NULL
);
";

/// Initialize the v0.2.0 schema on a fresh database.
pub fn initialize_schema(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute_batch(DDL_V0_2_0)?;

    // Add embedding column if it doesn't exist (migration from v0.2.0)
    let has_embedding: bool = conn
        .prepare("SELECT embedding FROM entities LIMIT 1")
        .is_ok();
    if !has_embedding {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN embedding BLOB;")?;
    }

    // Add always_on column (v1.x migration)
    let has_always_on: bool = conn
        .prepare("SELECT always_on FROM entities LIMIT 1")
        .is_ok();
    if !has_always_on {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN always_on INTEGER DEFAULT 0;")?;
    }

    // Add certainty column (v1.x migration)
    let has_certainty: bool = conn
        .prepare("SELECT certainty FROM entities LIMIT 1")
        .is_ok();
    if !has_certainty {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN certainty REAL DEFAULT 0.5;")?;
    }

    // Add workspace_hash column (v1.2.0 migration — multi-workspace scoping)
    let has_workspace_hash: bool = conn
        .prepare("SELECT workspace_hash FROM entities LIMIT 1")
        .is_ok();
    if !has_workspace_hash {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN workspace_hash TEXT DEFAULT '';")?;
    }

    // Add agent_id column to entities (v1.2.0 — agent attribution)
    let has_agent_id: bool = conn.prepare("SELECT agent_id FROM entities LIMIT 1").is_ok();
    if !has_agent_id {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN agent_id TEXT DEFAULT '';")?;
    }

    // Add agent_id column to journal (v1.2.0 — journal attribution)
    let has_journal_agent: bool = conn.prepare("SELECT agent_id FROM journal LIMIT 1").is_ok();
    if !has_journal_agent {
        conn.execute_batch("ALTER TABLE journal ADD COLUMN agent_id TEXT DEFAULT '';")?;
    }

    Ok(())
}

/// Check if a database has the v0.2.0 entities table.
#[allow(dead_code)]
pub fn is_v0_2_0(conn: &Connection) -> Result<bool, Box<dyn std::error::Error>> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='entities'",
        [],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Check if a database has the old v0.1.x memories table.
pub fn has_v0_1_memories(conn: &Connection) -> Result<bool, Box<dyn std::error::Error>> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memories'",
        [],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Get total entity count.
#[allow(dead_code)]
pub fn entity_count(conn: &Connection) -> Result<i64, Box<dyn std::error::Error>> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;
    Ok(count)
}

/// Migrate from v0.1.x schema to v0.2.0.
///
/// Opens the old DB, reads all memories, writes them as entities into the new schema,
/// and returns a MigrationReport.
pub fn migrate_from_v0_1(
    old_path: &str,
    conn: &Connection,
) -> Result<MigrationReport, Box<dyn std::error::Error>> {
    let old_conn = Connection::open(old_path)?;

    if !has_v0_1_memories(&old_conn)? {
        return Ok(MigrationReport {
            total_old_memories: 0,
            entities_created: 0,
            entities_updated: 0,
            errors: vec!["No v0.1.x memories table found in source DB".to_string()],
            completed_at_unix_ms: now_ms(),
        });
    }

    // Ensure target has v0.2.0 schema
    initialize_schema(conn)?;

    let mut stmt = old_conn.prepare(
        "SELECT id, content, type, summary, relevance, decay_score, retrieval_count,
                layer, topic_path, created_at_unix_ms, last_accessed_unix_ms,
                workspace_hash, tags, links, source, verified
         FROM memories",
    )?;

    let old_memories = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,         // id
            row.get::<_, String>(1)?,         // content
            row.get::<_, String>(2)?,         // type
            row.get::<_, Option<String>>(3)?, // summary
            row.get::<_, f64>(4)?,            // relevance
            row.get::<_, f64>(5)?,            // decay_score
            row.get::<_, i64>(6)?,            // retrieval_count
            row.get::<_, String>(7)?,         // layer
            row.get::<_, String>(8)?,         // topic_path
            row.get::<_, i64>(9)?,            // created_at_unix_ms
            row.get::<_, i64>(10)?,           // last_accessed_unix_ms
            row.get::<_, String>(11)?,        // workspace_hash
            row.get::<_, String>(12)?,        // tags
            row.get::<_, String>(13)?,        // links
            row.get::<_, String>(14)?,        // source
            row.get::<_, i32>(15)?,           // verified
        ))
    })?;

    let mut total = 0i64;
    let mut created = 0i64;
    let updated = 0i64;
    let mut errors: Vec<String> = Vec::new();

    for row in old_memories {
        total += 1;
        let (
            id,
            content,
            mem_type,
            summary,
            _relevance,
            decay_score,
            retrieval_count,
            layer,
            topic_path,
            created_at,
            last_accessed,
            workspace_hash,
            tags_str,
            links_str,
            source,
            verified,
        ) = match row {
            Ok(r) => r,
            Err(e) => {
                errors.push(format!("Row {} read error: {}", total, e));
                continue;
            }
        };

        // Build body_json: wrap content + summary
        let body = serde_json::to_string(&json!({
            "content": content,
            "summary": summary.unwrap_or_default(),
        }))
        .unwrap_or_else(|_| "{}".to_string());

        // Parse tags as JSON array, inject workspace_hash if present
        let mut tags_value: serde_json::Value =
            serde_json::from_str(&tags_str).unwrap_or(json!([]));
        if !workspace_hash.is_empty() {
            if let Some(arr) = tags_value.as_array_mut() {
                arr.push(json!(format!("workspace:{}", workspace_hash)));
            }
        }
        let tags_json = serde_json::to_string(&tags_value).unwrap_or_else(|_| "[]".to_string());

        // Category and key: derive from type + truncated id
        let category = "general".to_string();
        let key = format!("migrated-{}", &id[..id.len().min(20)]);

        let verified_int = if verified != 0 { 1 } else { 0 };

        let result = conn.execute(
            "INSERT OR REPLACE INTO entities
             (id, category, key, body_json, status, type, tags,
              decay_score, retrieval_count, layer, topic_path,
              archived, archive_reason, links, verified, source,
              created_at_unix_ms, last_accessed_unix_ms)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6,
                     ?7, ?8, ?9, ?10,
                     0, '', ?11, ?12, ?13,
                     ?14, ?15)",
            params![
                id,
                category,
                key,
                body,
                mem_type,
                tags_json,
                decay_score,
                retrieval_count,
                layer,
                topic_path,
                links_str,
                verified_int,
                source,
                created_at,
                last_accessed,
            ],
        );

        match result {
            Ok(_) => created += 1,
            Err(e) => errors.push(format!("Migrate error for id={}: {}", id, e)),
        }
    }

    // Rebuild FTS5 index
    let _ = conn.execute(
        "INSERT INTO entities_fts (rowid, body_json)
         SELECT rowid, body_json FROM entities",
        [],
    );

    Ok(MigrationReport {
        total_old_memories: total,
        entities_created: created,
        entities_updated: updated,
        errors,
        completed_at_unix_ms: now_ms(),
    })
}

/// Gather database statistics across all tables.
pub fn gather_stats(conn: &Connection, db_path: &str) -> Result<Stats, Box<dyn std::error::Error>> {
    let total_entities: i64 = conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))?;

    let by_category = query_grouped_counts(conn, "entities", "category")?;
    let by_type = query_grouped_counts(conn, "entities", "type")?;
    let by_layer = query_grouped_counts(conn, "entities", "layer")?;

    let total_journal: i64 = conn.query_row("SELECT COUNT(*) FROM journal", [], |r| r.get(0))?;

    let total_state: i64 = conn.query_row("SELECT COUNT(*) FROM state", [], |r| r.get(0))?;

    let db_size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);

    let oldest: Option<i64> = conn
        .query_row("SELECT MIN(created_at_unix_ms) FROM entities", [], |r| {
            r.get(0)
        })
        .ok();
    let newest: Option<i64> = conn
        .query_row("SELECT MAX(created_at_unix_ms) FROM entities", [], |r| {
            r.get(0)
        })
        .ok();

    Ok(Stats {
        total_entities,
        by_category,
        by_type,
        by_layer,
        total_journal_events: total_journal,
        total_state_entries: total_state,
        db_file_size_bytes: db_size,
        oldest_unix_ms: oldest,
        newest_unix_ms: newest,
    })
}

fn query_grouped_counts(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let sql = format!(
        "SELECT {}, COUNT(*) FROM {} GROUP BY {} ORDER BY COUNT(*) DESC",
        column, table, column
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)
                .unwrap_or_else(|_| "(null)".to_string()),
            r.get::<_, i64>(1).unwrap_or(0),
        ))
    })?;

    let mut map = serde_json::Map::new();
    for (key, count) in rows.flatten() {
        map.insert(key, json!(count));
    }
    Ok(serde_json::Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn temp_db() -> (Connection, String) {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("mimir-test-schema-{}.db", uuid::Uuid::new_v4()));
        let path_str = path.to_str().unwrap().to_string();
        let conn = Connection::open(&path_str).expect("open test db");
        (conn, path_str)
    }

    #[test]
    fn initializes_schema_on_new_db() {
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("init schema");
        assert!(is_v0_2_0(&conn).unwrap());
    }

    #[test]
    fn detects_v0_1_memories_table() {
        let (conn, _path) = temp_db();
        conn.execute_batch("CREATE TABLE memories (id TEXT PRIMARY KEY, content TEXT);")
            .expect("create v0.1 memories");
        assert!(has_v0_1_memories(&conn).unwrap());
        assert!(!is_v0_2_0(&conn).unwrap());
    }

    #[test]
    fn migration_from_v0_1_preserves_data() {
        let (old_conn, old_path) = temp_db();
        old_conn
            .execute_batch(
                "CREATE TABLE memories (
                    id TEXT PRIMARY KEY, content TEXT NOT NULL,
                    type TEXT DEFAULT 'insight', summary TEXT DEFAULT '',
                    relevance REAL DEFAULT 0.0, decay_score REAL DEFAULT 1.0,
                    retrieval_count INTEGER DEFAULT 0, layer TEXT DEFAULT 'working',
                    topic_path TEXT DEFAULT '', created_at_unix_ms INTEGER NOT NULL,
                    last_accessed_unix_ms INTEGER NOT NULL, workspace_hash TEXT DEFAULT '',
                    tags TEXT DEFAULT '{}', links TEXT DEFAULT '[]', source TEXT DEFAULT 'mimir',
                    verified INTEGER DEFAULT 0
                );",
            )
            .expect("create v0.1 schema");

        let now = now_ms();
        old_conn
            .execute(
                "INSERT INTO memories (id, content, type, created_at_unix_ms, last_accessed_unix_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params!["mem-test1", "Test content", "insight", now, now],
            )
            .expect("insert test memory");
        drop(old_conn);

        let (new_conn, _new_path) = temp_db();
        let report = migrate_from_v0_1(&old_path, &new_conn).expect("migrate");

        assert_eq!(report.total_old_memories, 1);
        assert_eq!(report.entities_created, 1);
        assert!(report.errors.is_empty());

        // Verify entity exists
        let count: i64 = new_conn
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify body_json contains original content
        let body: String = new_conn
            .query_row(
                "SELECT body_json FROM entities WHERE id = 'mem-test1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(body.contains("Test content"));

        // Cleanup old db
        let _ = std::fs::remove_file(&old_path);
    }

    #[test]
    fn gather_stats_returns_expected_shape() {
        let (conn, path) = temp_db();
        initialize_schema(&conn).expect("init schema");

        let now = now_ms();
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, created_at_unix_ms, last_accessed_unix_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params!["mem-1", "decision", "test-decision", "{}", now, now],
        )
        .unwrap();

        let stats = gather_stats(&conn, &path).unwrap();
        assert_eq!(stats.total_entities, 1);
        assert!(stats.db_file_size_bytes > 0);
        let _ = std::fs::remove_file(&path);
    }
}

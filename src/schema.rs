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
    -- Sign-bit signature of `embedding` (v2.13.0, dim/8 bytes): bit i set iff
    -- embedding[i] > 0. dense_search Hamming-prefilters on this instead of
    -- reading every full embedding blob once the vault is large enough.
    -- Written by store_embedding; backfilled by the v6 migration.
    emb_sig BLOB,
    always_on INTEGER DEFAULT 0,
    certainty REAL DEFAULT 0.5,
    -- Persistent importance floor (v2.13.0). Set by mimir_score; decay_tick and
    -- cohere floor decay_score at this value, so an explicit score survives the
    -- recency-based recompute instead of being erased by the next tick
    -- (fidelity > recency). 0.0 = unset, no effect.
    importance REAL DEFAULT 0.0,
    workspace_hash TEXT DEFAULT '',
    agent_id TEXT DEFAULT '',
    visibility TEXT DEFAULT 'workspace',
    -- Bi-temporal facts (v2.4.0). Two time axes plus a supersession link, so a
    -- fact can be retired without deleting history. All NULL/'' here means
    -- \"valid since creation, currently true, never superseded\" — the behavior
    -- before bi-temporal support, so existing rows need no interpretation change.
    valid_from_unix_ms INTEGER,      -- when the fact became true in the world (NULL = since creation)
    valid_to_unix_ms INTEGER,        -- when it stopped being true (NULL = still true)
    recorded_at_unix_ms INTEGER,     -- transaction time: when Mneme first knew it (backfilled = created_at)
    invalidated_at_unix_ms INTEGER,  -- transaction time: when Mneme retired it (NULL = live)
    supersedes TEXT DEFAULT '',      -- id of the entity this one replaced
    superseded_by TEXT DEFAULT '',   -- id of the entity that replaced this one

    -- Efficacy tracking (v2.10.0 — PMB-inspired follow-rate scoring). Tracks
    -- whether a lesson/convention/insight actually gets FOLLOWED by the agent,
    -- not just recalled. follow_rate feeds into decay_tick as a composite
    -- weight so rules that get ignored decay out of recall, and rules that
    -- earn their place resist decay even without recency.
    follow_count INTEGER DEFAULT 0,      -- times confirmed/detected as followed
    miss_count INTEGER DEFAULT 0,        -- times confirmed/detected as NOT followed
    follow_rate REAL DEFAULT 0.0,        -- follow_count / (follow_count + miss_count), 0.0 if no attempts
    efficacy_status TEXT DEFAULT 'unverified'  -- 'unverified' | 'useful' | 'dead'
);

-- Identity index: (category, key, workspace_hash) — #339. Created in
-- initialize_schema's gated block, NOT here: on a legacy DB this ungated DDL
-- runs before the ALTER that adds workspace_hash, so an index referencing the
-- column here would fail the whole batch.

-- Recall ranking index: lets the browse path (WHERE archived=0 [+ residual
-- filters] ORDER BY retrieval_count DESC, last_accessed_unix_ms DESC LIMIT k)
-- seek the archived=0 partition and read rows already in rank order, avoiding a
-- full table scan + temp-b-tree sort. EXPLAIN-verified: ~224x on global browse,
-- ~66x on workspace-scoped browse at 30k rows. (#209)
CREATE INDEX IF NOT EXISTS idx_entities_recall ON entities(archived, retrieval_count DESC, last_accessed_unix_ms DESC);

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
    audit_hash TEXT DEFAULT '',
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

-- Superseded fact versions (v2.4.0 — bi-temporal facts). When a remember()
-- overwrites an existing (category,key,workspace_hash) with new content, the prior
-- row is snapshotted here with invalidated_at set, so live reads stay one-row-per-key
-- (entities + its UNIQUE(category,key,workspace_hash) are untouched) while history is kept for
-- as-of / time-travel queries. A version was live during
-- [recorded_at_unix_ms, invalidated_at_unix_ms). superseded_by points at the
-- live entity id that replaced it. body_json carries the same encryption as
-- entities (ciphertext if a key is configured).
CREATE TABLE IF NOT EXISTS entity_history (
    history_id TEXT PRIMARY KEY,
    id TEXT NOT NULL,
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
    always_on INTEGER DEFAULT 0,
    certainty REAL DEFAULT 0.5,
    workspace_hash TEXT DEFAULT '',
    agent_id TEXT DEFAULT '',
    visibility TEXT DEFAULT 'workspace',
    valid_from_unix_ms INTEGER,
    valid_to_unix_ms INTEGER,
    recorded_at_unix_ms INTEGER,
    invalidated_at_unix_ms INTEGER,
    supersedes TEXT DEFAULT '',
    superseded_by TEXT DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL,
    last_accessed_unix_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entity_history_id ON entity_history(id);
CREATE INDEX IF NOT EXISTS idx_entity_history_catkey ON entity_history(category, key, invalidated_at_unix_ms);
";

/// Current schema migration level, stamped into `PRAGMA user_version` once all
/// the column-add migrations below have been applied. Bump this whenever you add
/// a new ALTER-probe migration in `initialize_schema`, or existing databases
/// (already at the previous level) will skip it.
const SCHEMA_VERSION: i64 = 6;

/// Initialize the v0.2.0 schema on a fresh database.
pub fn initialize_schema(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    // DDL is all IF NOT EXISTS, so it stays ungated: it both creates a fresh DB
    // and back-fills newer objects (e.g. idx_entities_recall) on older ones.
    conn.execute_batch(DDL_V0_2_0)?;

    // The column-add migrations below each prepare a throwaway `SELECT col LIMIT 1`
    // probe. `open` runs several times per process, so on a fully-migrated DB this
    // is pure wasted work. Gate it on PRAGMA user_version: run once when behind,
    // then stamp current and skip on every subsequent open. (#208)
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if user_version >= SCHEMA_VERSION {
        return Ok(());
    }

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

    // Add audit_hash column to journal (v2.0 — cryptographic audit log)
    let has_audit_hash: bool = conn.prepare("SELECT audit_hash FROM journal LIMIT 1").is_ok();
    if !has_audit_hash {
        conn.execute_batch("ALTER TABLE journal ADD COLUMN audit_hash TEXT DEFAULT '';")?;
    }

    // Add visibility column (v1.2.0 — access controls)
    let has_visibility: bool = conn.prepare("SELECT visibility FROM entities LIMIT 1").is_ok();
    if !has_visibility {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN visibility TEXT DEFAULT 'workspace';")?;
    }

    // Add bi-temporal columns (v2.4.0 — bi-temporal facts). Valid time
    // (valid_from/valid_to), transaction time (recorded_at/invalidated_at), and
    // supersession links. All additive; existing rows keep their meaning.
    if conn.prepare("SELECT valid_from_unix_ms FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN valid_from_unix_ms INTEGER;")?;
    }
    if conn.prepare("SELECT valid_to_unix_ms FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN valid_to_unix_ms INTEGER;")?;
    }
    if conn.prepare("SELECT recorded_at_unix_ms FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN recorded_at_unix_ms INTEGER;")?;
    }
    if conn.prepare("SELECT invalidated_at_unix_ms FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN invalidated_at_unix_ms INTEGER;")?;
    }
    if conn.prepare("SELECT supersedes FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN supersedes TEXT DEFAULT '';")?;
    }
    if conn.prepare("SELECT superseded_by FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN superseded_by TEXT DEFAULT '';")?;
    }

    // Add efficacy-tracking columns (v2.10.0 — PMB-inspired follow-rate scoring).
    if conn.prepare("SELECT follow_count FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN follow_count INTEGER DEFAULT 0;")?;
    }
    if conn.prepare("SELECT miss_count FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN miss_count INTEGER DEFAULT 0;")?;
    }
    if conn.prepare("SELECT follow_rate FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN follow_rate REAL DEFAULT 0.0;")?;
    }
    if conn.prepare("SELECT efficacy_status FROM entities LIMIT 1").is_err() {
        conn.execute_batch(
            "ALTER TABLE entities ADD COLUMN efficacy_status TEXT DEFAULT 'unverified';",
        )?;
    }
    // Backfill transaction time for pre-existing rows: a fact's recorded_at is
    // when Mneme first stored it, i.e. its created_at. (No-op on a fresh DB.)
    conn.execute_batch(
        "UPDATE entities SET recorded_at_unix_ms = created_at_unix_ms \
         WHERE recorded_at_unix_ms IS NULL;",
    )?;

    // Live-fact filter index. Created here (not in the ungated DDL) because it
    // references invalidated_at_unix_ms, which on a migrating DB only exists
    // after the ALTER above. NULL = live; recall will exclude non-NULL rows.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_entities_invalidated \
         ON entities(invalidated_at_unix_ms);",
    )?;

    // v5: persistent importance floor (see the column comment in the DDL).
    if conn.prepare("SELECT importance FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN importance REAL DEFAULT 0.0;")?;
    }

    // v6: sign-bit embedding signatures for the dense-search prefilter, plus a
    // backfill for embeddings stored before the column existed. Bounded work:
    // one pass over embedded rows, ~50 bytes written per row.
    if conn.prepare("SELECT emb_sig FROM entities LIMIT 1").is_err() {
        conn.execute_batch("ALTER TABLE entities ADD COLUMN emb_sig BLOB;")?;
    }
    {
        let mut stmt = conn.prepare(
            "SELECT id, embedding FROM entities \
             WHERE embedding IS NOT NULL AND emb_sig IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let pending: Vec<(String, Vec<u8>)> = rows.filter_map(|r| r.ok()).collect();
        drop(stmt);
        for (id, blob) in pending {
            let emb: Vec<f32> = blob
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let sig = crate::db::embedding_signature(&emb);
            conn.execute(
                "UPDATE entities SET emb_sig = ?1 WHERE id = ?2",
                params![sig, id],
            )?;
        }
    }

    // v4 (#339): identity becomes (category, key, workspace_hash). A plain
    // (category, key) uniqueness made cross-workspace key collisions
    // unstorable, which is what forced mimir_share's "copy into workspace" to
    // clobber the source row. Created here (after the workspace_hash ALTER,
    // like idx_entities_invalidated) rather than in the ungated DDL. Safe on
    // a populated DB — the old constraint was strictly tighter, so no
    // existing rows can collide. Create-then-drop, so uniqueness is never
    // unenforced.
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_category_key_ws \
         ON entities(category, key, workspace_hash); \
         DROP INDEX IF EXISTS idx_entities_category_key;",
    )?;

    // Stamp the migration level so subsequent opens skip the probe block above.
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;

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
    fn stamps_user_version_and_gates_migration_probes() {
        // Fresh init stamps the current schema version.
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("init schema");
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "fresh init must stamp the schema version");

        // Re-running on an already-current DB is a no-op that preserves data and
        // leaves the version untouched (the probe block is skipped).
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, created_at_unix_ms, last_accessed_unix_ms)
             VALUES ('v-test', 'insight', 'k', '{}', 0, 0)",
            [],
        )
        .unwrap();
        initialize_schema(&conn).expect("re-init should be a no-op");
        let v2: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v2, SCHEMA_VERSION);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entities WHERE id='v-test'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "re-init must not drop data");
    }

    #[test]
    fn migrates_pre_versioned_db_missing_a_column() {
        // Simulate a legacy DB at user_version=0 that predates the visibility
        // column: the gate must still run the probes and add the column, then
        // stamp the version so later opens skip.
        let (conn, _path) = temp_db();
        // Base v0.2.0 columns the DDL's indexes reference, but WITHOUT the
        // later ALTER-added columns (embedding/always_on/certainty/
        // workspace_hash/agent_id/visibility, journal agent_id/audit_hash).
        conn.execute_batch(
            "CREATE TABLE entities (
                id TEXT PRIMARY KEY, category TEXT NOT NULL DEFAULT 'general', key TEXT NOT NULL,
                body_json TEXT NOT NULL DEFAULT '{}', archived INTEGER DEFAULT 0,
                retrieval_count INTEGER DEFAULT 0,
                created_at_unix_ms INTEGER NOT NULL, last_accessed_unix_ms INTEGER NOT NULL
             );
             CREATE TABLE journal (
                id TEXT PRIMARY KEY, entity_id TEXT DEFAULT '',
                created_at_unix_ms INTEGER NOT NULL
             );",
        )
        .unwrap();
        assert!(
            conn.prepare("SELECT visibility FROM entities LIMIT 1").is_err(),
            "precondition: legacy table lacks visibility"
        );

        initialize_schema(&conn).expect("migrate legacy db");

        assert!(
            conn.prepare("SELECT visibility FROM entities LIMIT 1").is_ok(),
            "visibility column must be added during gated migration"
        );
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn migrates_unique_index_to_workspace_scoped_identity() {
        // v4 (#339): a v3-era DB with the two-column unique index and existing
        // rows must come out with the three-column index, the old index
        // dropped, and cross-workspace key collisions storable.
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("fresh init");
        // Rewind to the v3 state: old index back, new index gone, version 3.
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_entities_category_key_ws;
             CREATE UNIQUE INDEX idx_entities_category_key ON entities(category, key);
             PRAGMA user_version = 3;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, workspace_hash, created_at_unix_ms, last_accessed_unix_ms)
             VALUES ('mig-a', 'note', 'k', '{}', 'ws-alpha', 0, 0)",
            [],
        )
        .unwrap();

        initialize_schema(&conn).expect("v3 -> v4 migration");

        let old_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_entities_category_key'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_idx, 0, "old two-column unique index must be dropped");
        let new_idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_entities_category_key_ws'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_idx, 1, "workspace-scoped unique index must exist");

        // Same (category, key) in a different workspace now inserts…
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, workspace_hash, created_at_unix_ms, last_accessed_unix_ms)
             VALUES ('mig-b', 'note', 'k', '{}', 'ws-beta', 0, 0)",
            [],
        )
        .expect("cross-workspace key collision must be storable after v4");
        // …while a true duplicate in the SAME workspace is still rejected.
        assert!(
            conn.execute(
                "INSERT INTO entities (id, category, key, body_json, workspace_hash, created_at_unix_ms, last_accessed_unix_ms)
                 VALUES ('mig-c', 'note', 'k', '{}', 'ws-alpha', 0, 0)",
                [],
            )
            .is_err(),
            "same-workspace duplicate must still violate uniqueness"
        );
    }

    #[test]
    fn migration_backfills_embedding_signatures() {
        // v6: embeddings stored before emb_sig existed must get a signature
        // during the gated migration, matching what store_embedding writes.
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("fresh init");
        // Rewind: drop the column's data by simulating a pre-v6 row.
        let emb: Vec<f32> = vec![1.0, -2.0, 0.5, -0.1];
        let blob: Vec<u8> = emb.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, embedding, emb_sig,
                                   created_at_unix_ms, last_accessed_unix_ms)
             VALUES ('sig-1', 'note', 'k', '{}', ?1, NULL, 0, 0)",
            params![blob],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();

        initialize_schema(&conn).expect("v5 -> v6 migration");

        let sig: Vec<u8> = conn
            .query_row("SELECT emb_sig FROM entities WHERE id = 'sig-1'", [], |r| r.get(0))
            .expect("emb_sig must be backfilled");
        assert_eq!(sig, crate::db::embedding_signature(&emb));
    }

    #[test]
    fn adds_bitemporal_columns_and_backfills_recorded_at() {
        // A legacy DB (no bi-temporal columns) with one row predating the migration.
        let (conn, _path) = temp_db();
        conn.execute_batch(
            "CREATE TABLE entities (
                id TEXT PRIMARY KEY, category TEXT NOT NULL DEFAULT 'general', key TEXT NOT NULL,
                body_json TEXT NOT NULL DEFAULT '{}', archived INTEGER DEFAULT 0,
                retrieval_count INTEGER DEFAULT 0,
                created_at_unix_ms INTEGER NOT NULL, last_accessed_unix_ms INTEGER NOT NULL
             );
             CREATE TABLE journal (
                id TEXT PRIMARY KEY, entity_id TEXT DEFAULT '',
                created_at_unix_ms INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entities (id, category, key, body_json, created_at_unix_ms, last_accessed_unix_ms)
             VALUES ('e1', 'general', 'k', '{}', 111, 222)",
            [],
        )
        .unwrap();
        assert!(
            conn.prepare("SELECT recorded_at_unix_ms FROM entities LIMIT 1").is_err(),
            "precondition: legacy table lacks the bi-temporal columns"
        );

        initialize_schema(&conn).expect("migrate legacy db to bi-temporal schema");

        // All six bi-temporal columns must now exist.
        for col in [
            "valid_from_unix_ms",
            "valid_to_unix_ms",
            "recorded_at_unix_ms",
            "invalidated_at_unix_ms",
            "supersedes",
            "superseded_by",
        ] {
            assert!(
                conn.prepare(&format!("SELECT {col} FROM entities LIMIT 1")).is_ok(),
                "column {col} must be added during migration"
            );
        }

        // recorded_at backfilled to created_at; the row is live (not invalidated)
        // and unbounded in valid time — i.e. unchanged in meaning.
        let recorded: i64 = conn
            .query_row("SELECT recorded_at_unix_ms FROM entities WHERE id='e1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded, 111, "recorded_at must backfill to created_at");
        let invalidated: Option<i64> = conn
            .query_row("SELECT invalidated_at_unix_ms FROM entities WHERE id='e1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(invalidated, None, "existing rows must be live (not invalidated)");
        let valid_from: Option<i64> = conn
            .query_row("SELECT valid_from_unix_ms FROM entities WHERE id='e1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(valid_from, None, "existing rows must be valid since creation");

        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn fresh_db_has_bitemporal_columns_and_live_index() {
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("init schema");
        assert!(conn.prepare("SELECT invalidated_at_unix_ms FROM entities LIMIT 1").is_ok());
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_entities_invalidated'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "idx_entities_invalidated should be created on a fresh DB");
    }

    #[test]
    fn creates_recall_ranking_index() {
        let (conn, _path) = temp_db();
        initialize_schema(&conn).expect("init schema");
        // Index must exist...
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_entities_recall'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "idx_entities_recall should be created");
        // ...and the recall browse query must use it (no full scan / temp sort).
        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT id FROM entities WHERE archived = 0 \
                 ORDER BY retrieval_count DESC, last_accessed_unix_ms DESC LIMIT 20",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        let joined = plan.join(" | ");
        assert!(
            joined.contains("idx_entities_recall"),
            "recall query should use idx_entities_recall, got: {joined}"
        );
        assert!(
            !joined.to_uppercase().contains("TEMP B-TREE"),
            "recall query should not need a temp-b-tree sort, got: {joined}"
        );
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

# Mimir

> Persistent memory for AI agents. Structured entity model. SQLite + FTS5. MCP-native. Fully local.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rust-lang.org)

## What is Mimir?

Mimir is a lightweight **MCP JSON-RPC 2.0 stdio server** that gives AI agents durable
memory across sessions. Agents store structured entities, journal their decisions,
and manage transient state — all through standard MCP tools.

It uses **SQLite with full-text search (FTS5)** across three tables: entities
(structured, idempotent), journal (append-only event log), and state (key-value
with TTL). No API keys, no embeddings model, no LLM required. The binary makes
zero network calls at runtime. You own the database.

Works with any MCP host: Claude Desktop, Cursor, OpenClaw, Hermes Agent, Perseus, etc.

---

## Quick Start

### Option 1: One-shot bootstrap

```bash
curl -sSL https://raw.githubusercontent.com/tcconnally/mimir/main/scripts/bootstrap.sh | bash
```

Idempotent — safe to re-run. Set `FORCE=1` to force a rebuild.

### Option 2: Build from source

```bash
git clone https://github.com/tcconnally/mimir.git
cd mimir
cargo build --release
cp target/release/mimir ~/.local/bin/
```

**Requirements:** Rust 1.70+ (stable), a C compiler (rusqlite bundles SQLite).

---

## MCP Configuration

Add Mimir as an MCP server in your host's config:

### Claude Desktop / Cursor / OpenClaw

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": ["--db", "/home/YOU/.mimir/data/mimir.db"]
    }
  }
}
```

### Hermes Agent

```yaml
mcp_servers:
  mimir:
    command: "mimir"
    args: ["--db", "~/.mimir/data/mimir.db"]
```

### Perseus

```yaml
mimir:
  enabled: true
  transport: "stdio"
  command: ["mimir", "--db", "~/.mimir/data/mimir.db"]
  timeout_s: 30.0
  merge_strategy: "local_first"
  fallback_to_local: true
  circuit_breaker:
    threshold: 3
    cooldown: 120
  context_categories: ["decision", "architecture", "convention"]
  context_limit: 10
```

---

## MCP Tools (v0.2.0)

### Entity tools

| Tool | Description |
|---|---|
| `mimir_remember` | Store or update an entity. Idempotent by (category, key). |
| `mimir_recall` | Search entities with FTS5 + LIKE fallback, filtered by category/type/topic. |
| `mimir_forget` | Soft-delete an entity (sets archived=1). Recoverable. |
| `mimir_link` | Create a relationship link from one entity to another. |
| `mimir_unlink` | Remove a link between entities. |

### Journal tools

| Tool | Description |
|---|---|
| `mimir_journal` | Append a journal event (decision/observation/action) with evaluated/acted/forward. |
| `mimir_timeline` | Query journal events by time range with optional filters. |

### State tools

| Tool | Description |
|---|---|
| `mimir_state_set` | Set a key-value state entry with optional TTL (auto-expires). |
| `mimir_state_get` | Get a state value. Returns null if expired or missing. |
| `mimir_state_delete` | Delete a state entry. |
| `mimir_state_list` | List state keys, optionally filtered by prefix. |

### Management tools

| Tool | Description |
|---|---|
| `mimir_health` | Check server and database health. |
| `mimir_stats` | Full database statistics across all three tables. |
| `mimir_compact` | Archive entities below a decay threshold (supports dry-run). |
| `mimir_migrate` | Migrate a v0.1.x database to v0.2.0 schema. |
| `mimir_context` | Return pre-formatted markdown context block for session injection. |
| `mimir_workspace_list` | List all distinct entity categories. |

---

## Database Schema

```sql
-- Entities: structured, idempotent by UNIQUE(category, key)
CREATE TABLE entities (
    id TEXT PRIMARY KEY, category TEXT NOT NULL, key TEXT NOT NULL,
    body_json TEXT NOT NULL DEFAULT '{}', status TEXT DEFAULT 'active',
    type TEXT DEFAULT 'insight', tags TEXT DEFAULT '[]',
    decay_score REAL DEFAULT 1.0, retrieval_count INTEGER DEFAULT 0,
    layer TEXT DEFAULT 'working', topic_path TEXT DEFAULT '',
    archived INTEGER DEFAULT 0, archive_reason TEXT DEFAULT '',
    links TEXT DEFAULT '[]', verified INTEGER DEFAULT 0,
    source TEXT DEFAULT 'agent',
    created_at_unix_ms INTEGER NOT NULL,
    last_accessed_unix_ms INTEGER NOT NULL,
    UNIQUE(category, key)
);
CREATE VIRTUAL TABLE entities_fts USING fts5(body_json, content_rowid='rowid');

-- Journal: append-only event log with time-range access
CREATE TABLE journal (
    id TEXT PRIMARY KEY, event_type TEXT NOT NULL DEFAULT 'decision',
    evaluated_json TEXT DEFAULT '{}', acted_json TEXT DEFAULT '{}',
    forward_json TEXT DEFAULT '{}', category TEXT DEFAULT '',
    key TEXT DEFAULT '', entity_id TEXT DEFAULT '',
    created_at_unix_ms INTEGER NOT NULL
);

-- State: key-value with optional TTL
CREATE TABLE state (
    key TEXT PRIMARY KEY, value_json TEXT NOT NULL DEFAULT '{}',
    expires_at_unix_ms INTEGER, created_at_unix_ms INTEGER NOT NULL
);
```

---

## Entity Model

The core concept in Mimir v0.2.0 is the **entity**: a structured fact with a
composite key of `(category, key)`. This makes storage idempotent — call
`mimir_remember` with the same category and key as many times as you want,
and it updates the existing entity instead of creating a duplicate.

Categories are user-defined. Common patterns:

| Category | Example key | Body |
|---|---|---|
| `decision` | `use-postgres-16` | `{"decision": "Use PostgreSQL 16", "rationale": "..."}` |
| `architecture` | `auth-module` | `{"component": "Auth", "stack": "JWT + SQLite"}` |
| `convention` | `pr-review-required` | `{"rule": "All PRs require review before merge"}` |
| `project` | `perseus` | `{"name": "Perseus", "repo": "tcconnally/perseus", "version": "1.0.7"}` |

---

## Key Properties

- **Zero runtime deps** — static binary with bundled SQLite, no network needed
- **Structured entity model** — idempotent upsert by (category, key)
- **Category-filtered search** — narrow recall to specific categories
- **Journal events** — append-only log with evaluated/acted/forward structure
- **State with TTL** — key-value store with automatic expiration
- **Entity linking** — create navigable relationships between entities
- **FTS5 keyword search** — Relevance-ranked results (by retrieval count + recency) with LIKE fallback
- **No LLM required** — stores and retrieves directly; no embeddings needed
- **MCP-native** — standard JSON-RPC 2.0 over stdio
- **Single-file database** — one SQLite file; easy to backup, copy, or inspect

---

## Offline

Mimir is fully offline after build. No telemetry, no API calls, no network requests —
ever. The binary never dials home. You own every byte.

---

## Roadmap (v0.5.0+)

### ✅ Implemented
- **23 MCP tools** — Full CRUD for entities, links, journal, state, vault, workspace context
- **Ebbinghaus decay** — Time-based memory fading with retrieval boosts via `mimir_decay`
- **Layer promotion** — Three-tier memory (buffer → working → core) based on retrieval count
- **Vault export/import** — Export entities to Markdown files, import from vault directories
- **Graph traversal** — Walk entity link graphs via `mimir_traverse`
- **Workspace context** — Pre-formatted context blocks for AI agent session injection
- **State management** — Key-value TTL state entries via `mimir_state_*` tools
- **Conflict detection** — Near-duplicate detection via trigram similarity
- **JSON-RPC 2.0** — Full stdio MCP server implementation
- **FTS5 + LIKE search** — SQLite full-text search with substring fallback

### 🚧 Planned
- **Semantic search** — Optional embedding-based recall for fuzzy matching
- **Cross-workspace federation** — Share entities across workspace boundaries
- **Web dashboard** — Browser-based memory explorer and visualization

## License

MIT — see [LICENSE](./LICENSE).

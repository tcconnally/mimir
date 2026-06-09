# Mimir

> Persistent memory for AI agents. SQLite + FTS5. MCP-native. Fully local.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rust-lang.org)

## What is Mimir?

Mimir is a lightweight **MCP JSON-RPC 2.0 stdio server** that gives AI agents durable
memory across sessions. Agents store facts they learn, and Mimir recalls them when
needed — so the agent doesn't start from zero every time.

It uses **SQLite with full-text search (FTS5)**. No API keys, no embeddings model, no
LLM required. The binary makes zero network calls at runtime. You own the database.

Works with any MCP host: Claude Desktop, Cursor, OpenClaw, Hermes Agent, etc.

---

## Quick Start

### Option 1: One-shot bootstrap

A single command that installs Rust (if needed), builds Mimir from source, and sets
everything up:

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

Add Mimir as an MCP server in your host's config. Pick your tool:

### Claude Desktop

`claude_desktop_config.json`:

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

### Cursor

`.cursor/mcp.json`:

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

### OpenClaw

In your OpenClaw MCP config:

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

`~/.hermes/config.yaml`:

```yaml
mcp_servers:
  mimir:
    command: "mimir"
    args: ["--db", "~/.mimir/data/mimir.db"]
```

---

## MCP Tools

| Tool | Description |
|------|-------------|
| `mimir_store` | Store a memory with content, type (`insight`/`architecture`/`decision`), tags, and importance |
| `mimir_recall` | Search memories by keyword query (FTS5 + LIKE fallback), filtered by type, workspace, topic |
| `mimir_health` | Check server and database health |

### Key Properties

- **Zero runtime deps** — static binary with bundled SQLite, no network needed
- **Keyword search** — FTS5 for BM25-ranked results, LIKE fallback for multi-word queries
- **No LLM required** — stores and retrieves memories directly; no fact extraction, no embeddings
- **MCP-native** — standard JSON-RPC 2.0 over stdio; works with any MCP host
- **Single-file database** — one SQLite file with FTS5 index; easy to backup, copy, or inspect

---

## Usage

### Start the MCP server

```bash
mimir --db ~/.mimir/data/mimir.db
```

The legacy `mimir serve --db ... --mcp` form still works for older MCP host
configs. The `--mcp` flag is deprecated because stdio MCP mode is always on.

### Show version

```bash
mimir --version
```

### Override database path

```bash
export MIMIR_DB_PATH=/custom/path/mimir.db
mimir
```

### Manual MCP testing

```bash
# Pipe JSON-RPC directly
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}' | mimir --db /tmp/test.db
```

---

## Database Schema

```sql
CREATE TABLE memories (
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
    source TEXT DEFAULT 'mimir',
    verified INTEGER DEFAULT 0
);

CREATE VIRTUAL TABLE memories_fts USING fts5(content, content_rowid='rowid');
```

---

## Offline

Mimir is fully offline after build. No telemetry, no API calls, no network requests —
ever. The binary never dials home. You own every byte.

---

## Roadmap

**Current:** v0.1.1 — direct MCP server mode

| Feature | Status |
|---------|--------|
| MCP JSON-RPC 2.0 stdio server | ✅ |
| Keyword search (FTS5 + LIKE) | ✅ |
| Memory store with metadata | ✅ |
| SQLite persistence | ✅ |
| Embedding-based vector search | 🔜 v0.2 |
| Ebbinghaus decay algorithm | 🔜 v0.2 |
| Cross-workspace federation | 🔜 v0.3 |
| SSE transport | 🔜 v0.3 |

---

## Using Mimir with Perseus

Mimir is also the default memory backend for [Perseus](https://github.com/tcconnally/perseus),
a live context engine for AI agents. If you use Perseus, add to `.perseus/config.yaml`:

```yaml
mimir:
  enabled: true
  transport: "stdio"
  command: ["mimir", "--db", "~/.mimir/data/mimir.db"]
  timeout_s: 10.0
  merge_strategy: "local_first"
  fallback_to_local: true
  circuit_breaker:
    threshold: 3
    cooldown: 120
```

Then add `@memory` to `.perseus/context.md` and Perseus will call `mimir_recall` at
render time to populate context with relevant memories.

---

## License

MIT — see [LICENSE](./LICENSE).

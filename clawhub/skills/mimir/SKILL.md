---
name: mimir-memory
description: Self-hosted persistent memory for OpenClaw agents via Mimir MCP — 30 tools, hybrid search, AES-256 encryption, zero external dependencies
---

# Mimir — Self-Hosted Persistent Agent Memory

## Purpose

This skill connects your OpenClaw agent to Mimir, a self-hosted Rust binary that provides durable, encrypted persistent memory via stdio MCP. No cloud, no API keys, no Docker — just one binary serving 23 memory tools.

**Mimir runs entirely on your machine.** No data leaves your environment. No external service sees your agent's memories. Every stored memory is AES-256-GCM encrypted at rest.

## What Mimir Does

### Full persistent memory lifecycle

Your agent can **remember** facts, decisions, and context; **recall** them across sessions with keyword search; **search** semantically via dense embeddings; and **forget** stale memories. All memory operations are durable and survive OpenClaw restarts.

### Hybrid search

Mimir combines BM25 keyword search (FTS5) with dense vector embeddings via Reciprocal Rank Fusion (RRF). Your agent gets the best of both worlds — exact keyword matches and semantic similarity in a single query.

### Encryption at rest

All stored data is AES-256-GCM encrypted. Even if someone accesses the database file, they can't read your agent's memories without the encryption key.

### Memory lifecycle management

Mimir applies Ebbinghaus decay to memories — rarely-used facts fade and eventually archive. Your agent's context stays sharp without manual cleanup. Run a `mimir_cohere` grooming pass to auto-link related memories, promote frequently-used ones, and archive decayed ones.

### No external dependencies

Mimir is a single Rust binary (~8MB). No Docker, no PostgreSQL, no Redis, no cloud service. Drop it in, start it, connect via stdio MCP. It runs anywhere OpenClaw runs — Linux, macOS, Windows, even a Raspberry Pi.

## Available Tools (23 total)

### Core CRUD
- `mimir_remember` — Store a fact, decision, or observation with category, key, tags, and confidence
- `mimir_recall` — Keyword search across all stored memories with FTS5
- `mimir_get_entity` — Retrieve full details of a specific memory
- `mimir_forget` — Soft-delete a memory (recoverable)

### Semantic search
- `mimir_embed` — Generate and store dense embeddings for vector search
- `mimir_search_memories` — Semantic search via dense embeddings (requires `--llm-endpoint`)
- `mimir_ask` — Ask a natural language question, get a grounded answer with cited sources

### Memory lifecycle
- `mimir_cohere` — Autonomous grooming: promote hot memories, link related ones, archive stale
- `mimir_decay` — Recalculate Ebbinghaus decay scores across all memories
- `mimir_prune` — Bulk archive low-decay or old memories
- `mimir_compact` — Archive memories below a decay threshold

### Knowledge graph
- `mimir_link` — Create relationships between memories
- `mimir_unlink` — Remove stale relationships
- `mimir_traverse` — Walk the relationship graph from any memory

### Journal & timeline
- `mimir_journal` — Append structured decision/observation log entries
- `mimir_timeline` — Query the journal by time range and event type

### Vault (import/export)
- `mimir_vault_export` — Export all memories to Obsidian-compatible .md files
- `mimir_vault_import` — Import .md vault files, idempotent (no duplicates)

### State & proactive recall
- `mimir_state_set` / `mimir_state_get` / `mimir_state_delete` — Key-value state with optional TTL
- `mimir_recall_when` — Proactive just-in-time memory: surfaces relevant memories before tool calls
- `mimir_conflicts` — Detect contradictory or duplicate memories for review

### Monitoring
- `mimir_health` — Health check
- `mimir_stats` — Entity counts by category, database size, date range
- `mimir_context` — Pre-formatted markdown context block for session injection
- `mimir_workspace_list` — List all knowledge domains in the database

## Setup Instructions

### Step 1 — Install Mimir

Choose one:

**Download binary (fastest):**
```bash
# Linux x86_64
curl -L https://github.com/Perseus-Computing-LLC/mneme/releases/latest/download/mimir-linux-x86_64 -o mimir
chmod +x mimir
sudo mv mimir /usr/local/bin/
```

**Build from source (requires Rust):**
```bash
git clone https://github.com/Perseus-Computing-LLC/mneme.git
cd mimir
cargo build --release
sudo cp target/release/mimir /usr/local/bin/
```

**Python client (optional, for scripts):**
```bash
pip install mimir-client
```

### Step 2 — Configure Mimir as an MCP server in OpenClaw

Add to your OpenClaw MCP servers config:

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": ["--db", "~/.openclaw/mimir/mimir.db"],
      "env": {
        "MIMIR_ENCRYPTION_KEY": "${MIMIR_ENCRYPTION_KEY}"
      }
    }
  }
}
```

For semantic search with embeddings, also set:
```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": [
        "--db", "~/.openclaw/mimir/mimir.db",
        "--llm-endpoint", "http://localhost:11434"
      ],
      "env": {
        "MIMIR_ENCRYPTION_KEY": "${MIMIR_ENCRYPTION_KEY}"
      }
    }
  }
}
```

### Step 3 — Generate an encryption key (optional but recommended)

```bash
# Generate a 32-byte key
MIMIR_ENCRYPTION_KEY=$(openssl rand -hex 32)
echo "MIMIR_ENCRYPTION_KEY=$MIMIR_ENCRYPTION_KEY" >> ~/.openclaw/.env
```

Without an encryption key, Mimir stores data unencrypted (still local).

### Step 4 — Initialize and verify

```bash
# Create the database directory
mkdir -p ~/.openclaw/mimir

# Start Mimir once to initialize
mimir --db ~/.openclaw/mimir/mimir.db --health

# Verify it's running
mimir --db ~/.openclaw/mimir/mimir.db --stats
```

Then start a new OpenClaw session. Your agent now has access to all 23 Mimir memory tools.

### Step 5 — Web dashboard (optional)

```bash
# Start the web dashboard on port 8789
mimir --db ~/.openclaw/mimir/mimir.db --dashboard --port 8789
# Open http://localhost:8789
```

## Data Handling & Privacy

Mimir is entirely self-hosted. No data leaves your machine.

- **What gets stored:** Only what your agent explicitly passes to `mimir_remember` or `mimir_journal` tool calls. No automatic capture, no silent monitoring.
- **Where it's stored:** A local SQLite database at the path you specify (`--db`). You control the file.
- **Encryption:** AES-256-GCM at rest when an encryption key is provided. Without a key, data is stored in plaintext SQLite (still local).
- **Who can read it:** Only processes with access to the database file and encryption key. No network access by default.
- **Retention:** Memories decay naturally via Ebbinghaus scoring. You control decay thresholds. Nothing is deleted without your agent's action.
- **No telemetry:** No analytics, no usage tracking, no phone-home. Mimir is a local binary.
- **MIT licensed:** Fully open source. You can audit the code, fork it, embed it.

## Constraints

- **No cloud sync:** Mimir is local-only by design. Use `mimir_vault_export` and git for backup/sharing.
- **Embeddings require Ollama or compatible endpoint:** Semantic search needs `--llm-endpoint` pointing to an Ollama instance or compatible embedding API. Keyword search (FTS5) works without it.
- **Single-writer:** Mimir uses SQLite. One process at a time. Works perfectly for a single-agent OpenClaw setup.

## Complementary Skills

Pair Mimir with these ClawHub skills for a complete memory stack:

- `memory-audit-guardian` — Weekly memory governance audit
- `skill-from-memory` — Extract reusable skills from stored memories
- `knox-governance` — Audit logging of all memory operations

## CI / Automation

To run Mimir in CI or scheduled jobs:
```bash
# Start Mimir in the background
mimir --db /tmp/mimir_ci.db &

# Run a coherence grooming pass nightly
mimir --db ~/.openclaw/mimir/mimir.db --cohere

# Export to vault for git backup
mimir --db ~/.openclaw/mimir/mimir.db --vault-export ~/mimir-vault/
```

## Links

- GitHub: https://github.com/Perseus-Computing-LLC/mneme
- Website: https://perseus.observer/mimir
- Python client: https://pypi.org/project/mimir-client/
- Smithery: https://smithery.ai/server/mimir

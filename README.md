# Mimir

> Persistent memory for AI agents. Structured entity model. SQLite + FTS5 + hybrid vector search. MCP-native. Fully local.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rust-lang.org)
[![Version](https://img.shields.io/badge/version-1.0.1-green.svg)](https://github.com/Perseus-Computing-LLC/mimir/releases)

## What is Mimir?

Mimir is a lightweight **MCP JSON-RPC 2.0 stdio server** that gives AI agents durable
memory across sessions. Agents store structured entities, journal their decisions,
manage transient state, generate embeddings, query with hybrid search, and ingest
external data — all through **30 MCP tools**.

It uses **SQLite with FTS5 + dense vector search** across three tables: entities
(structured, idempotent), journal (append-only event log), and state (key-value
with TTL). Optional Ollama integration enables RAG (`mimir_ask`) and embedding
generation (`mimir_embed`). A built-in web dashboard provides visual exploration.

Works with any MCP host: Claude Desktop, Cursor, OpenClaw, Hermes Agent, Perseus, etc.

---

## Quick Start

```bash
# Build from source
git clone https://github.com/Perseus-Computing-LLC/mimir.git
cd mimir
cargo build --release

# Linux/macOS: use cargo install to avoid macOS security kill (Killed: 9)
cargo install --path .

# Or copy manually (Linux only — macOS will kill the unsigned binary)
cp target/release/mimir ~/.local/bin/

# Or download the binary
curl -sSL https://github.com/Perseus-Computing-LLC/mimir/releases/download/v1.0.0/mimir-v1.0.0-linux-x86_64 -o mimir
chmod +x mimir && mv mimir ~/.local/bin/
```

**Requirements:** Rust 1.70+ (stable), a C compiler (rusqlite bundles SQLite).

---

## Install

```bash
# Python client
pip install mimir

# Or download the standalone binary (no Python needed)
curl -L https://github.com/Perseus-Computing-LLC/mimir/releases/latest/download/mimir-linux-x86_64 -o mimir
chmod +x mimir
./mimir --db ./memory.db
```

## Quickstart

```python
from mimir import MimirClient

client = MimirClient("./memory.db")
client.remember("Hello world — my first persistent memory!", category="demo")
results = client.recall("first memory")
print(results[0].content)
# "Hello world — my first persistent memory!"
```

## Why Mimir vs Alternatives

| | Mimir | Mem0 | Letta | Zep |
|---|---|---|---|---|
| **Deployment** | Single binary | Cloud + self-host | Docker/Postgres | Docker/Postgres |
| **Dependencies** | None (SQLite embedded) | Python + vector DB | Postgres + Python | Postgres + Go |
| **Encryption** | AES-256-GCM ✅ | ❌ | ❌ | ❌ |
| **Hybrid Search** | BM25 + Dense + RRF | Vector only | Vector only | Vector + Graph |
| **MCP Tools** | 23 | 5 | 8 | 0 |
| **Offline/Local** | ✅ Fully local | Cloud-dependent | Docker needed | Docker needed |
| **License** | MIT | Apache 2.0 | Apache 2.0 | Apache 2.0 |

Mimir is for teams that want **production memory without infrastructure** — no Postgres, no Docker, no cloud services. Just one binary.

## Features

### Hybrid Search
- **FTS5 keyword search** with LIKE fallback and stemming expansion
- **Dense vector search** via cosine similarity on stored embeddings
- **Reciprocal Rank Fusion (RRF)** — combine keyword + vector results
- **Query expansion** — Porter stemming variants for broader recall

### RAG & Embeddings
- **`mimir_ask`** — natural language Q&A over stored memories via Ollama
- **`mimir_embed`** — generate and store dense vectors via Ollama `/api/embed`
- Supports single-entity and batch-category embedding

### Encryption
- **AES-256-GCM** transparent encryption for entity `body_json`
- Opt-in via `--encryption-key` flag
- `mimir keygen` subcommand for key generation
- FTS5 index stays plaintext for search

### Web Dashboard
- Built-in Axum HTTP server (`mimir serve --web --port 8767`)
- Dark-themed dashboard with search, entity table, vis.js graph, timeline
- Default bind: `127.0.0.1` (use `--web-bind 0.0.0.0` to expose)
- Separate SQLite connection in WAL mode for concurrent reads

### External Connectors
- **GitHub issues connector** — ingest issues/PRs by repo, rate-limit aware
- **File watcher** — scan directories for `.md`/`.txt`/`.json` files with content-hash dedup
- **`mimir_ingest`** — trigger connector syncs, dry-run preview
- YAML-based connector config via `--connectors-config`

### Data Lifecycle
- **`mimir_prune`** — bulk archive by category, decay threshold, or age
- **Ebbinghaus decay** with retrieval boosts and configurable archiving
- **Near-duplicate detection** via trigram similarity
- **Vault export/import** — markdown files with YAML frontmatter

---

## MCP Tools (30 tools)

### Entity tools
| Tool | Description |
|---|---|
| `mimir_remember` | Store/update entity. Idempotent by (category, key). |
| `mimir_recall` | Search with FTS5/dense/hybrid modes, filters, stemming expansion. |
| `mimir_recall_when` | Proactive just-in-time recall: surface entities whose `recall_when` triggers match a given context. |
| `mimir_get_entity` | Fetch one entity by ID with full `body_json` (e.g. to read a result truncated by `preview_cap`). |
| `mimir_forget` | Soft-delete (archived=1). |
| `mimir_link` | Create relationship links between entities. |
| `mimir_unlink` | Remove entity links. |

### Search & RAG
| Tool | Description |
|---|---|
| `mimir_ask` | RAG: recall context, query Ollama, return grounded answer with sources. |
| `mimir_embed` | Generate dense vectors via Ollama `/api/embed`. Single or batch. |
| `mimir_ingest` | Trigger connector syncs (GitHub, file watcher). |

### Journal tools
| Tool | Description |
|---|---|
| `mimir_journal` | Append journal event with evaluated/acted/forward structure. |
| `mimir_timeline` | Query journal by time range with optional filters. |

### State tools
| Tool | Description |
|---|---|
| `mimir_state_set` | Set key-value state with optional TTL. |
| `mimir_state_get` | Get state value. Returns null if expired/missing. |
| `mimir_state_delete` | Delete state entry. |
| `mimir_state_list` | List state keys, optionally filtered by prefix. |

### Management
| Tool | Description |
|---|---|
| `mimir_health` | Server and DB health check. |
| `mimir_stats` | Full DB statistics across all tables. |
| `mimir_compact` | Archive entities below decay threshold (supports dry-run). |
| `mimir_migrate` | Migrate v0.1.x DB to v0.2.0 schema. |
| `mimir_context` | Pre-formatted markdown context for session injection. |
| `mimir_workspace_list` | List all distinct entity categories. |
| `mimir_prune` | Bulk archive by category, decay, or age. |
| `mimir_cohere` | Autonomous coherence pass: promote buffer→working, apply decay, auto-link related entities, archive stale. |

### Graph & analysis
| Tool | Description |
|---|---|
| `mimir_traverse` | Walk entity link graph up to configurable depth. |
| `mimir_score` | Assign quality score (0.0-1.0). |
| `mimir_conflicts` | Detect near-duplicate entities via trigram similarity. |
| `mimir_decay` | Recalculate Ebbinghaus decay scores. |

### Vault
| Tool | Description |
|---|---|
| `mimir_vault_export` | Export entities to .md files with YAML frontmatter. |
| `mimir_vault_import` | Import from .md vault directory (idempotent). |

---

## CLI

```
mimir serve --db /data/mimir.db
mimir serve --web --port 8767 --encryption-key ~/.mimir/secret.key
mimir serve --llm-endpoint http://localhost:11434/api/generate --llm-model llama3
mimir serve --connectors-config ~/.mimir/connectors.yaml
mimir keygen --key-file ~/.mimir/secret.key
mimir migrate --from old.db --to new.db
```

### Flags
| Flag | Description |
|---|---|
| `--db` | SQLite database path (default: `~/.mimir/data/mimir.db`) |
| `--web` | Start web dashboard |
| `--port` | Dashboard port (default: 8767) |
| `--web-bind` | Dashboard bind address (default: 127.0.0.1) |
| `--encryption-key` | AES-256-GCM key file path |
| `--llm-endpoint` | Ollama API endpoint for `mimir_ask` and `mimir_embed` |
| `--llm-model` | Ollama model name (default: llama3) |
| `--connectors-config` | Path to connectors.yaml |


## MCP Configuration

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

### Perseus

```yaml
mimir:
  enabled: true
  transport: "stdio"
  command: ["mimir", "--db", "~/.mimir/data/mimir.db"]
  timeout_s: 30.0
  merge_strategy: "local_first"
  fallback_to_local: true
  context_categories: ["decision", "architecture", "convention"]
  context_limit: 10
```

---

## Connector Config

```yaml
connectors:
  github:
    enabled: true
    token: "${GITHUB_TOKEN}"
    repos:
      - Perseus-Computing-LLC/mimir
      - Perseus-Computing-LLC/perseus
    days_past: 90
    max_items_per_repo: 500
  file_watcher:
    enabled: true
    paths:
      - ~/Documents/notes
      - ~/projects
    extensions:
      - .md
      - .txt
    debounce_ms: 1500
```

---

## Key Properties

- **30 MCP tools** — full CRUD, search, RAG, embeddings, connectors, lifecycle
- **Hybrid search** — FTS5 + dense vectors + RRF fusion
- **Encryption at rest** — AES-256-GCM, opt-in, transparent
- **Web dashboard** — built-in, browser-based, dark theme
- **Zero runtime deps** — static binary with bundled SQLite
- **No LLM required** — core operations work offline; Ollama optional for RAG/embeddings
- **Single-file database** — easy backup, copy, inspect
- **Fully offline** — no telemetry, no API calls, no network by default

---

## License

MIT — see [LICENSE](./LICENSE).

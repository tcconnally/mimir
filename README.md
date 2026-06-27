# Mimir

<!-- mcp-name: io.github.Perseus-Computing-LLC/mimir -->

> **Persistent Memory for AI Agents — MCP-Native. Local-First. Zero Dependencies.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rust-lang.org)
[![Version](https://img.shields.io/badge/version-2.2.1-green.svg)](https://github.com/Perseus-Computing-LLC/mimir/releases)
[![LangGraph](https://img.shields.io/badge/integrations-LangGraph-blue)](integrations/langgraph/)
[![CrewAI](https://img.shields.io/badge/integrations-CrewAI-orange)](integrations/crewai/)
[![AutoGen](https://img.shields.io/badge/integrations-AutoGen-purple)](integrations/autogen/)
[![MCP Tools](https://img.shields.io/badge/MCP%20tools-40-brightgreen)]()

Mimir is a single Rust binary that gives AI agents durable memory across sessions.
**One binary. One file. No Docker. No Postgres. No cloud.** Just persistent memory
that works with any MCP host.

## One-Line Install

```bash
curl -sSf https://raw.githubusercontent.com/Perseus-Computing-LLC/mimir/main/scripts/install.sh | sh
```

That's it. Mimir is installed to `~/.local/bin/mimir`. Start it:

```bash
mimir serve --db ~/.mimir/data/mimir.db
```

Connect any MCP host (Claude Desktop, Cursor, Hermes Agent, Perseus, etc.):

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": ["serve", "--db", "~/.mimir/data/mimir.db"]
    }
  }
}
```

## 30-Second Quickstart

```bash
# Start Mimir
mimir serve --db memory.db &
sleep 1

# Remember a fact (via MCP JSON-RPC on stdio)
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"mimir_remember","arguments":{"category":"demo","key":"hello","body_json":"{\"text\":\"Hello from Mimir!\"}"}}}' | mimir serve --db memory.db

# Search for it
echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mimir_recall","arguments":{"query":"Hello"}}}' | mimir serve --db memory.db
```

## Why Mimir

Mimir is the **only** memory engine that is simultaneously MCP-native,
local-first, zero-dependency, AND agent-first.

### Comparison Matrix

| | Mimir | Mem0 | Letta | Zep |
|---|---|---|---|---|
| **Deployment** | Single binary (~8MB) | Cloud + self-host | Docker/Postgres | Docker/Postgres |
| **Dependencies** | None (SQLite embedded) | Python + vector DB | Postgres + Python | Postgres + Go |
| **MCP-Native** | ✅ 40 tools | ❌ Not MCP-native | ❌ Not MCP-native | ❌ Not MCP-native |
| **Offline/Local** | ✅ Fully local | Cloud-dependent | Docker needed | Docker needed |
| **Encryption** | AES-256-GCM ✅ | ❌ | ❌ | ❌ |
| **Hybrid Search** | BM25 + Dense + RRF | Vector only | Vector only | Vector + Graph |
| **Entity Lifecycle** | Decay + Promote + Archive | ❌ | ❌ | ❌ |
| **Entity Graph** | Link + Traverse | ❌ | ❌ | ✅ |
| **Journal Audit Trail** | ✅ Immutable | ❌ | ❌ | ❌ |
| **State Management** | ✅ Key-value + TTL | ❌ | ❌ | ❌ |
| **MCP Tools** | 40 | 5 | 8 | 0 |
| **GitHub Stars** | ~20 | ~55K | ~15K | ~3K |
| **License** | MIT | Apache 2.0 | Apache 2.0 | Apache 2.0 |

[Full comparison: Mimir vs Mem0 →](docs/comparison/mimir-vs-mem0.md)
[vs Letta →](docs/comparison/mimir-vs-letta.md)
[vs Zep →](docs/comparison/mimir-vs-zep.md)

### Stress Test: 100K Entities

Mimir handles production workloads on modest hardware:

| Metric | Result |
|---|---|
| **100K entity insert** | 1.01s (98,732 entities/s) |
| **FTS5 recall (10 results)** | 0.022s |
| **Decay tick (100K entities)** | 1.317s (batched, transactional) |
| **Memory (100K entities)** | ~85MB RSS |
| **DB file size (100K)** | ~45MB (with FTS5 index) |

Run it yourself: `cargo test stress_100k --release -- --ignored --nocapture`

## Framework Integrations

Ready-to-use adapters that make Mimir the default memory backend for
popular AI agent frameworks:

| Framework | Integration | Type |
|---|---|---|
| [**LangGraph**](integrations/langgraph/) | `MimirStore` | `BaseStore` implementation |
| [**CrewAI**](integrations/crewai/) | `MimirMemoryTool` | Agent tool |
| [**AutoGen**](integrations/autogen/) | `MimirMemory` | `Memory` implementation |

Each adapter:
- Connects via MCP stdio subprocess (persistent session)
- Maps the framework's memory interface to Mimir tools
- Comes with a README quickstart (5 minutes to working)
- Has passing tests with mocked MCP transport

Any MCP-compatible framework works with Mimir directly. See
[Awesome Mimir](awesome-mimir.md) for the full list.

## 40 MCP Tools

### Entity CRUD
| Tool | Description |
|---|---|
| `mimir_remember` | Store/update entity. Idempotent by (category, key). |
| `mimir_recall` | Search with FTS5/dense/hybrid modes, filters, stemming expansion. |
| `mimir_recall_when` | Proactive just-in-time recall: surface entities whose `recall_when` triggers match. |
| `mimir_get_entity` | Fetch one entity by ID with full `body_json`. |
| `mimir_forget` | Soft-delete (archived=1). |

### Search & RAG
| Tool | Description |
|---|---|
| `mimir_ask` | RAG: recall context, query LLM, return grounded answer with sources. |
| `mimir_embed` | Generate dense vectors via Ollama or OpenAI-compatible endpoint. |
| `mimir_context` | Pre-formatted markdown block for session injection. |
| `mimir_ingest` | Trigger connector syncs (GitHub, file watcher). |

### Graph
| Tool | Description |
|---|---|
| `mimir_link` | Create typed relationship links between entities. |
| `mimir_unlink` | Remove entity links. |
| `mimir_traverse` | Walk entity link graph up to configurable depth. |

### Journal
| Tool | Description |
|---|---|
| `mimir_journal` | Append structured event with actor attribution. |
| `mimir_timeline` | Query journal by time range with filters. |

### State
| Tool | Description |
|---|---|
| `mimir_state_set` | Set key-value state with optional TTL. |
| `mimir_state_get` | Get state value. Returns null if expired. |
| `mimir_state_delete` | Delete state entry. |
| `mimir_state_list` | List state keys, optionally filtered by prefix. |

### Lifecycle
| Tool | Description |
|---|---|
| `mimir_decay` | Recalculate Ebbinghaus decay scores (batched 1000-entity transactions). |
| `mimir_prune` | Bulk archive by category, decay threshold, or age. |
| `mimir_purge` | Permanently delete archived entities + VACUUM. Destructive. |
| `mimir_cohere` | Autonomous coherence grooming pass — promote, decay, link, archive. |
| `mimir_autocohere` | Full atomic grooming: cohere → decay → compact in one pass (supports dry-run). |
| `mimir_compact` | Archive entities below decay threshold. |
| `mimir_reindex` | Rebuild FTS5 search index from entities table. |

### Quality
| Tool | Description |
|---|---|
| `mimir_score` | Assign quality score (0.0-1.0). |
| `mimir_conflicts` | Detect near-duplicate entities via trigram similarity. |
| `mimir_correct` | Structured correction capture for learning from errors. |
| `mimir_supersede` | Mark a new fact as superseding an old one (sets the old entity to `deprecated`). |

### Vault & Federation
| Tool | Description |
|---|---|
| `mimir_vault_export` | Export entities to .md files with YAML frontmatter. |
| `mimir_vault_import` | Import from .md vault directory (idempotent). |
| `mimir_federate` | Copy entities between workspaces. |
| `mimir_share` | Share one entity (by category + key) into another workspace, preserving content. |
| `mimir_workspace_list` | List all distinct entity categories. |

### Metrics & Ops
| Tool | Description |
|---|---|
| `mimir_stats` | Full DB statistics across all tables. |
| `mimir_health` | Server and DB health check. |
| `mimir_bench` | Performance benchmark tracking. |
| `mimir_maintenance` | DB maintenance: dedup, orphan detection, VACUUM, FTS5 reindex (supports dry-run). |
| `mimir_synthesize` | LLM session synthesis — extract lessons from transcripts. |
| `mimir_migrate` | Migrate v0.1.x DB to current schema. |

## CLI

```bash
# Server
mimir serve --db /data/mimir.db
mimir serve --web --port 8767 --encryption-key ~/.mimir/secret.key
mimir serve --llm-endpoint http://localhost:11434/api/generate --llm-model llama3
mimir serve --transport sse --port 8787 --mcp-token my-secret-token

# Maintenance (operate directly on DB, no server needed)
mimir stats          --db /data/mimir.db
mimir forget         --db /data/mimir.db --category decision --key stale-choice --reason "superseded"
mimir prune          --db /data/mimir.db --category junk --min-decay 0.1 --dry-run
mimir purge          --db /data/mimir.db --dry-run
mimir decay          --db /data/mimir.db
mimir reindex        --db /data/mimir.db
mimir vault-export   --db /data/mimir.db --vault-dir ./export/
mimir vault-import   --db /data/mimir.db --vault-dir ./export/

# Key management
mimir keygen --key-file ~/.mimir/secret.key
```

### Flags

| Flag | Description |
|---|---|
| `--db` | SQLite database path (default: `~/.mimir/data/mimir.db`) |
| `--web` | Start web dashboard |
| `--port` | Dashboard port (default: 8767) |
| `--web-bind` | Dashboard bind address (default: 127.0.0.1) |
| `--transport` | MCP transport: `stdio` (default), `sse`, or `http` |
| `--mcp-token` | Bearer token for SSE/HTTP transport auth |
| `--encryption-key` | AES-256-GCM key file path |
| `--llm-endpoint` | LLM API endpoint for `mimir_ask` and embeddings |
| `--llm-model` | LLM model name (default: llama3) |
| `--llm-api-key` | API key for LLM endpoints (OpenAI, Azure, etc.) |
| `--embedding-endpoint` | OpenAI-compatible embedding endpoint |
| `--connectors-config` | Path to connectors.yaml |

## Features

### Hybrid Search
- **Offline dense search out of the box** — a quantized all-MiniLM-L6-v2 model is
  compiled into the binary, so semantic recall works with **zero config and zero
  network** (no Ollama, no API key, no model download). Build a lean binary
  without it via `cargo build --no-default-features`.
- **FTS5 keyword search** with LIKE fallback and Porter stemming expansion
- **Dense vector search** via cosine similarity on stored embeddings
- **Reciprocal Rank Fusion (RRF)** — combine keyword + vector results
- **Query expansion** — automatic stemming variants for broader recall

### Memory Lifecycle
- **Ebbinghaus decay** — memories naturally fade unless retrieved (refresh on access)
- **Layer promotion** — buffer → working → core based on access frequency
- **Automatic archival** — stale entities archive; purge to permanently delete + VACUUM
- **Always-on entities** — pin critical memories for unconditional session injection

### RAG & Embeddings
- **`mimir_ask`** — natural language Q&A over stored memories via any LLM (Ollama, OpenAI, etc.)
- **`mimir_embed`** — generate and store dense vectors via Ollama or OpenAI-compatible `/v1/embeddings`
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
- YAML-based connector config via `--connectors-config`

### Multi-Transport
- **stdio** (default) — zero-config, works with any MCP host
- **SSE** — Server-Sent Events for HTTP-based MCP clients
- **HTTP** — REST-style MCP endpoint
- **Bearer token auth** — for SSE/HTTP transports

## Perseus Integration

Mimir is the default memory backend for [Perseus](https://perseus.observer):

```yaml
mimir:
  enabled: true
  transport: "stdio"
  command: ["mimir", "serve", "--db", "~/.mimir/data/mimir.db"]
  timeout_s: 30.0
  merge_strategy: "local_first"
  fallback_to_local: true
  context_categories: ["decision", "architecture", "convention"]
  context_limit: 10
```

## Government & Federal Procurement

Mimir is built for government deployment from the ground up.

| Capability | Status |
|---|---|
| **License** | MIT — no copyleft, no GPL/AGPL |
| **SBOM** | [Published](./docs/SBOM.md) — NTIA minimum elements |
| **Air-gapped** | Fully offline — no telemetry, no API calls, no network by default |
| **Encryption at rest** | AES-256-GCM, transparent, opt-in |
| **Audit trail** | Immutable journal with chain-of-custody |
| **Supply chain** | SLSA attestation in progress |

**For federal buyers:** See [docs/federal-buyers.md](./docs/federal-buyers.md) for
procurement information, compliance status, and deployment models (air-gapped,
on-premises, classified environments).

Perseus Computing LLC is a US-owned small business. SAM.gov registration in progress.
NAICS: 541715, 541511, 541512.

## License

MIT — see [LICENSE](./LICENSE).

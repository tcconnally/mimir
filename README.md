# Mimir

<!-- mcp-name: io.github.Perseus-Computing-LLC/mimir -->

> **Persistent Memory for AI Agents — MCP-Native. Local-First. Zero Dependencies.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rust-lang.org)
[![Version](https://img.shields.io/badge/version-2.7.0-green.svg)](https://github.com/Perseus-Computing-LLC/mimir/releases)
[![LangGraph](https://img.shields.io/badge/integrations-LangGraph-blue)](integrations/langgraph/)
[![CrewAI](https://img.shields.io/badge/integrations-CrewAI-orange)](integrations/crewai/)
[![AutoGen](https://img.shields.io/badge/integrations-AutoGen-purple)](integrations/autogen/)
[![MCP Tools](https://img.shields.io/badge/MCP%20tools-46-brightgreen)]()

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

## Works With Every MCP Client

Mimir is a standard MCP **stdio** server — the same `mimir serve` command works
everywhere. Run `mimir doctor` to validate your install and print this matrix locally.

| Client | Status | Config | 
|---|---|---|
| Claude Desktop | ✅ | `claude_desktop_config.json` |
| Claude Code / Hermes | ✅ | `.mcp.json` / `config.yaml` |
| Cursor | ✅ | `.cursor/mcp.json` |
| Windsurf | ✅ | `mcp_config.json` |
| VS Code + Continue.dev | ✅ | `config.json` |
| Zed | ✅ | `settings.json` |
| Codex CLI | ✅ | `~/.codex/config.toml` |

Copy-paste config snippets for each: **[docs/clients/](docs/clients/)**.

## Why Mimir

Mimir is the **only** memory engine that is simultaneously MCP-native,
local-first, zero-dependency, AND agent-first.

### Comparison Matrix

| | Mimir | Mem0 | Letta | Zep |
|---|---|---|---|---|
| **Deployment** | Single binary (~8MB) | Cloud + self-host | Docker/Postgres | Docker/Postgres |
| **Dependencies** | None (SQLite embedded) | Python + vector DB | Postgres + Python | Postgres + Go |
| **MCP-Native** | ✅ 46 tools | ❌ Not MCP-native | ❌ Not MCP-native | ❌ Not MCP-native |
| **Offline/Local** | ✅ Fully local | Cloud-dependent | Docker needed | Docker needed |
| **Encryption** | AES-256-GCM ✅ | ❌ | ❌ | ❌ |
| **Hybrid Search** | BM25 + Dense + RRF | Vector only | Vector only | Vector + Graph |
| **Entity Lifecycle** | Decay + Promote + Archive | ❌ | ❌ | ❌ |
| **Entity Graph** | Link + Traverse | ❌ | ❌ | ✅ |
| **Journal Audit Trail** | ✅ Immutable | ❌ | ❌ | ❌ |
| **State Management** | ✅ Key-value + TTL | ❌ | ❌ | ❌ |
| **MCP Tools** | 46 | 5 | 8 | 0 |
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

## 46 MCP Tools

### Entity CRUD
| Tool | Description |
|---|---|
| `mimir_remember` | Store/update entity. Idempotent by (category, key); a content change snapshots the prior version into history. |
| `mimir_recall` | Search with FTS5/dense/hybrid modes, filters, stemming expansion. |
| `mimir_recall_layer` | Recall from a specific biomimetic layer (world, episodic, semantic). |
| `mimir_recall_when` | Proactive just-in-time recall: surface entities whose `recall_when` triggers match. |
| `mimir_get_entity` | Fetch one entity by ID with full `body_json`. |
| `mimir_as_of` | Bi-temporal time-travel: the version of a fact (category + key) that was live at a past instant. |
| `mimir_history` | List every superseded version of a fact (category + key), newest first — the full version trail (companion to `mimir_as_of`). |
| `mimir_forget` | Soft-delete (archived=1). |

### Search & RAG
| Tool | Description |
|---|---|
| `mimir_ask` | RAG: recall context, query LLM, return grounded answer with sources. |
| `mimir_embed` | Generate dense vectors via the bundled model, Ollama, or OpenAI-compatible endpoint. |
| `mimir_semantic_search` | Dense-only semantic search shortcut — find entities by meaning, ranked purely by embedding similarity (no keyword fallback). |
| `mimir_context` | Pre-formatted markdown block for session injection. |
| `mimir_ingest` | Trigger connector syncs (GitHub, file watcher). |
| `mimir_ingest_file` | Locally extract a document's text (plaintext/markdown always; DOCX/PDF with the `multimodal` feature) and store it as a recallable entity. |
| `mimir_extract` | Local, deterministic, rule-based knowledge extraction (facts / preferences / temporal events / episodes) from text or a stored entity. Read-only. |

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
| `mimir_conflicts` | Detect conflicting entities via trigram similarity; opt-in `resolve=true` invalidates the lower-certainty side into history (reversible, dry-run by default). |
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
mimir obsidian-sync  ~/obsidian-vault/Mimir/          # one-shot export to an Obsidian vault
mimir obsidian-sync  ~/obsidian-vault/Mimir/ --watch  # continuous sync on every memory change

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

## Your AI Memory in Obsidian

Mimir is your AI agent's long-term memory — and it doubles as **your** second
brain. Every entity your agent remembers exports to a plain Markdown note with
YAML frontmatter, so your AI's memory becomes a navigable personal knowledge
base inside the tools you already use: **Obsidian, Logseq, or Notion.**

```bash
# Export your entire memory to an Obsidian vault as linked Markdown notes
mimir obsidian-sync ~/obsidian-vault/Mimir/

# Keep it live — re-export automatically on every memory change
mimir obsidian-sync ~/obsidian-vault/Mimir/ --watch
```

Open the vault in Obsidian and you get a graph of your agent's knowledge.

**WikiLink backlinks.** When one entity links to another (via `mimir_link` or a
`depends_on` / `implements` / `references` relationship), the exported note gets
a `## Links` section with `[[WikiLink]]` backlinks that resolve natively in
Obsidian's graph view:

```markdown
---
id: cli-de8dfb8364b6
category: architecture
key: api
type: insight
decay_score: 0.5000
---

{"content":"axum service"}

## Links

- [[cli-99756b494c7d|database]] (depends_on)
```

Links resolve **by entity id** (notes are written as `<id>.md`) so they never
break, and Obsidian shows the human-readable `key` as the link label. Open the
graph view and your agent's architecture, decisions, and insights become a
clickable knowledge map.

**`--watch`** polls Mimir's cheap, deterministic state digest on an interval and
re-exports only when memory actually changes. It naturally catches every
`mimir_remember` write with no filesystem-watcher dependency and no coupling to
the server. Tune the interval with `MIMIR_SYNC_INTERVAL_SECS` (default: 2s).

### Other PKM tools

| Tool | How |
|---|---|
| **Obsidian** | `mimir obsidian-sync <vault>` — WikiLinks resolve in the graph view out of the box. |
| **Logseq** | Point `obsidian-sync` at your Logseq graph directory. Logseq reads the same `[[WikiLink]]` syntax and Markdown frontmatter. |
| **Notion** | Run `mimir vault-export`, then use Notion's *Import → Markdown & CSV* to pull the notes in. |

Unlike cloud-only "second brain" tools, Mimir runs **100% local**, is written in
**Rust**, encrypts at rest with **AES-256-GCM**, and applies **decay scoring** so
stale memories fade — your knowledge base stays yours and stays fresh.

## Features

### Semantic Search (on by default)
- **Bundled, in-process embeddings** — a quantized all-MiniLM-L6-v2 model
  (384-dim) is compiled into the binary, so dense/semantic search works with
  **zero config and zero network**: no Ollama, no API key, no model download.
  This is the default build (`bundled-embeddings` feature).
- **Auto-embed on write (#271)** — `mimir_remember` embeds each new (or
  content-changed) entity **synchronously** as it is written, using the bundled
  model. Single-entity embedding is deterministic and LRU-cached, so it is cheap
  and adds no background tasks. Embedding failures are non-fatal (logged to
  stderr); the write always succeeds.
- **Hybrid is the default recall mode (#271)** — `mimir_recall(query=...)` with
  no `mode` flag automatically selects **hybrid** (dense + keyword fused via RRF)
  whenever embeddings exist, and transparently falls back to **fts5** keyword
  search when none do. No manual `mimir_embed` step, no flags to remember.
- **`mimir_semantic_search(query, limit)`** — a one-tool shortcut for pure
  dense, meaning-based search (no keyword fallback) when you just want "find
  things like this".
- **Optional alternate embedder** — to use **Ollama** or any OpenAI-compatible
  `/v1/embeddings` endpoint instead of the bundled model, set `--llm-endpoint`
  (and `--embedding-endpoint` / `--llm-api-key` as needed). This is entirely
  optional; the bundled model is used by default.
- Build a lean binary without bundled embeddings via
  `cargo build --no-default-features` — recall then defaults to keyword search
  unless a remote embedder is configured.

### Hybrid Search internals
- **FTS5 keyword search** with LIKE fallback and Porter stemming expansion
- **Dense vector search** via cosine similarity on stored embeddings
- **Reciprocal Rank Fusion (RRF)** — combine keyword + vector results
- **Query expansion** — automatic stemming variants for broader recall
### Memory Lifecycle

Mimir models memory using three biomimetic layers, inspired by human memory pathways:

- **World (Core):** Slow-decaying, global facts about the environment.
- **Episodic (Buffer):** Fast-decaying, session-specific interaction history.
- **Semantic (Working):** Medium-decaying, general knowledge and learned concepts.

You can interact with these layers directly using the `mimir_recall_layer` tool or by specifying the `layer` parameter in `mimir_remember`.

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

## Privacy Policy

Mimir is a **local-first MCP server** — it runs entirely on your machine.

### Data Collection
- **No data collection.** Mimir does not collect, transmit, or phone home any user data, usage statistics, or telemetry.
- All data remains in your local SQLite database file.

### Data Usage & Storage
- All memory entities, journal entries, and state are stored locally in a SQLite database at the path you specify via `--db`.
- Optional **AES-256-GCM encryption at rest** is available — when enabled, entity bodies are encrypted before storage.
- No data is shared with Perseus Computing LLC or any third party.

### Third-Party Sharing
- **None.** Mimir is fully air-gapped by default. No API calls, no cloud services, no external network requests.
- The optional dense vector embeddings feature uses a locally-compiled model — no external embedding API is called.

### Data Retention
- You control retention: entities can be soft-deleted (`mimir_forget`), archived (via decay/compact), or permanently purged (`mimir_purge`).
- No automatic off-machine backup is performed.

### Contact
- **Email:** privacy@perseus.observer
- **GitHub:** [Perseus-Computing-LLC/mimir](https://github.com/Perseus-Computing-LLC/mimir)

## License

MIT — see [LICENSE](./LICENSE).

# Mimir Roadmap

## What Mimir Is

A local-first persistent memory engine for AI agents. MCP-native. Single static binary.
Zero runtime dependencies. Structured entity model with journal events and state management.

## What Mimir Is Not

- Not a knowledge graph or entity extraction engine
- Not a cloud service or SaaS
- Not a replacement for a vector database
- Not dependent on any specific AI assistant or framework

---

## v0.1 — MVP

**Status:** ✅ Shipped (2026-05)

- SQLite + FTS5 keyword search with LIKE fallback
- MCP JSON-RPC 2.0 stdio server
- Three tools: `mimir_store`, `mimir_recall`, `mimir_health`
- Single static binary, bundled SQLite, zero runtime deps

---

## v0.2.0 — Structured Entity Model

**Status:** ✅ Shipped (2026-06-10)

### Three-table schema
- **entities** — idempotent by UNIQUE(category, key), FTS5-indexed
- **journal** — append-only event log with evaluated/acted/forward structure
- **state** — key-value with optional TTL and auto-expiration

### Entity tools
- `mimir_remember` — idempotent entity upsert by (category, key)
- `mimir_recall` — FTS5 search with category, type, topic, decay filters
- `mimir_forget` — soft-delete (archived=1) with reason
- `mimir_link` / `mimir_unlink` — entity relationship graph

### Journal tools
- `mimir_journal` — append structured events (decision/observation/action)
- `mimir_timeline` — time-range query with category/type/entity filters

### State tools
- `mimir_state_set` — key-value with optional TTL
- `mimir_state_get` — retrieve with auto-expiration check
- `mimir_state_delete` / `mimir_state_list` — management

### Management
- `mimir_stats` — full statistics across all three tables
- `mimir_compact` — archive entities below decay threshold
- `mimir_migrate` — CLI subcommand for v0.1.x → v0.2.0 migration
- `mimir_context` — pre-formatted markdown context block for session injection
- `mimir_workspace_list` — list all distinct categories

### Perseus integration
- Rewrote `mimir_connector.py` for entity model
- Removed Sibyl Memory dependency entirely
- Mimir is now the sole persistent memory backend for Perseus

---

## v1.0.0 — Intelligence & Distribution

**Status:** ✅ Shipped (2026-06-15)

This release transforms Mimir from a storage engine into an intelligent memory system.
Every v0.2.x and v0.5 goal was absorbed into this release (the intermediate version numbers
were skipped — v1.0.0 includes everything planned through v0.5 plus more).

### Confidence decay (was v0.2.1)
- Ebbinghaus decay algorithm: scores degrade over time, reset on recall
- Layer progression: buffer → working → core based on retrieval_count
- Near-duplicate detection via trigram similarity at store time
- `mimir_decay` tool for manual decay recalculation
- Auto-archive of stale entities via `mimir_compact`

### Semantic search (was v0.3)
- Hybrid search: FTS5 keyword + dense embeddings + RRF (Reciprocal Rank Fusion)
- Bundled embedding model via Ollama `/api/embed`
- Query expansion with Porter stemming for morphological variants
- `mimir_recall` with `search_mode`: fts5, dense, hybrid
- `mimir_embed` tool for explicit embedding generation

### Memory synthesis (was v0.5)
- Memory chain traversal via `mimir_traverse` (follow entity relationships)
- Quality scoring via `mimir_score` (agents rate memories 0-1)
- Conflict detection via `mimir_conflicts` (contradictory facts flagged)
- RAG via `mimir_ask` — NL Q&A with Ollama + cited sources

### Vault & portability
- `.md` vault export/import via `mimir_vault_export` / `mimir_vault_import`
- Human-readable, git-trackable, Obsidian-compatible markdown files
- SQLite remains the operational store; vault is the portable representation

### External connectors
- GitHub issues connector via `mimir_ingest`
- File watcher connector for watching directories
- Extensible connector framework for third-party data sources

### Security & operations
- AES-256-GCM encryption at rest for `body_json`
- `mimir migrate` subcommand for key generation
- Web dashboard (Axum HTTP server) with `--web --port` flags
- Entity graph visualization, search, stats in dashboard
- Smithery + Glama marketplace listings with full tool metadata

### Quality & polish
- Deep-dive code review (11 issues resolved)
- Second-pass review (10 issues resolved)
- Compiler warnings eliminated
- CI smoke-test workflow
- Claims audit against codebase
- Glama TDQS improvements (outputSchema + annotations)

**Total tools: 28 MCP tools**

---

## v1.1.0 — Distribution & Ecosystem (current)

**Target:** "Mimir everywhere."

### Integration guides (in progress)
- Claude Code integration guide
- Cursor integration guide
- LangGraph MimirStore adapter
- CrewAI MimirMemory provider
- AutoGen MimirContext plugin

### Transport expansion
- SSE/HTTP transport for non-stdio MCP hosts
- Docker image with pre-built binary (Alpine multi-stage)
- One-line install: `curl | bash` bootstrap verified on macOS, Linux, WSL

### Quality
- Glama TDQS score improvement (outputSchema + annotations on remaining tools)
- Smithery capability discovery fix (ensure all 28 tools appear)
- Windows CI in GitHub Actions
- Stress tests at 100K+ entity scale

### Discovery
- Submit to curated MCP server directories
- Appear in "awesome-mcp" lists
- Write comparison page vs Mem0, Sibyl, Holographic

---

## v1.2.0 — Multi-Agent & Federation

**Target:** "One memory engine, many agents, many workspaces."

- Workspace scoping with `workspace_hash`
- Agent identity tracking on stored memories
- Cross-workspace federation via vault sync
- Merge conflict resolution for concurrent writes
- Per-workspace access controls and visibility rules

---

## v1.3.0 — Offline Embeddings

**Target:** "Truly zero-dependency semantic search."

- Bundle all-MiniLM-L6-v2 via `ort` (ONNX Runtime) or `candle`
- Remove Ollama dependency for hybrid search
- Optional: still support external embedding endpoints
- 80MB binary size increase, zero network calls

---

## v2.0 — Platform

**Target:** "Mimir as infrastructure."

- gRPC transport alongside MCP
- Clustering with leader election
- Read replicas for high-availability deployments
- Audit log with cryptographic chaining
- Managed cloud option (Mimir Cloud)

---

## Design Principles

1. **Zero runtime dependencies.** The binary is self-contained.
2. **Offline-first.** All core operations work without internet.
3. **MCP-native.** Every feature ships as an MCP tool.
4. **Agent-first, not human-first.** Tools are designed for AI agents.
5. **Compose, don't integrate.** Mimir does persistent memory; composes with Perseus, Obsidian, Git.
6. **Local-first, cloud-optional.** Run it anywhere; cloud features are additive.

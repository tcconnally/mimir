# Mimir — 12-Month Delivery Plan (Jul 2026 → Jun 2027)

> **Version:** v1.0.1 · **Last updated:** 2026-06-19
> **Repo:** [Perseus-Computing-LLC/mimir](https://github.com/Perseus-Computing-LLC/mimir)
>
> This document is the single source of truth for Mimir's roadmap. Each phase
> breaks down into concrete, shipable tasks with success criteria.

---

## Current State

| Metric | Value |
|---|---|
| **Version** | v1.0.1 (shipped 2026-06-19) |
| **MCP tools** | 31 (`mimir_reindex` added in #164) |
| **LOC (Rust)** | ~8,500 |
| **Storage** | SQLite + FTS5 + dense embedding BLOBs |
| **Transport** | MCP stdio (primary), SSE/HTTP (in progress) |
| **Encryption** | AES-256-GCM at rest (opt-in) |
| **Tests** | 18 passing, zero failing |
| **CI** | GitHub Actions (ubuntu-latest, build + test) |

### Open issues to close before v1.1.0

| Issue | Priority | Description | Status |
|---|---|---|---|
| [#165](https://github.com/Perseus-Computing-LLC/mimir/issues/165) | HIGH | 50+ `.unwrap()` calls in db.rs | ✅ Fixed on 2026-06-19 |
| [#166](https://github.com/Perseus-Computing-LLC/mimir/issues/166) | MEDIUM | Web dashboard doesn't inherit encryption key | ✅ Already fixed in v1.0.1 |

---

## Design Principles (Non-Negotiable)

1. **Zero runtime dependencies.** The binary is self-contained.
2. **Offline-first.** All core operations work without internet.
3. **MCP-native.** Every feature ships as an MCP tool.
4. **Agent-first, not human-first.**
5. **Compose, don't integrate.**
6. **Local-first, cloud-optional.**

---

## Phase 1: v1.1.0 — Distribution & Ecosystem

**Theme:** "Mimir everywhere."
**Timeline:** Q3 2026 (Jul–Sep) · **13 weeks**

### Week 1–2: Integration Guide Polish

| Task | Owner | Deliverable | Success |
|---|---|---|---|
| Claude Code guide | AI | `docs/integration/claude-code.md` | User follows guide, Mimir tools appear in Claude Code |
| Cursor guide | AI | `docs/integration/cursor.md` | User follows guide, Mimir tools appear in Cursor |
| General MCP guide | AI | `docs/integration/general-mcp.md` | Works with any MCP-compatible host |

**Status:** All three files exist as drafts. Polish pass needed: add screenshots, verify exact config snippets, test on fresh installs.

### Week 3–4: Framework Adapters

| Task | Deliverable | Success |
|---|---|---|
| LangGraph `MimirStore` | `integrations/langgraph/mimir_store.py` | LangGraph agent reads/writes via Mimir |
| CrewAI `MimirMemory` | `integrations/crewai/mimir_memory.py` | CrewAI agent uses Mimir as memory backend |
| AutoGen `MimirContext` | `integrations/autogen/mimir_context.py` | AutoGen agent injects Mimir context |

**Pattern for all adapters:** Subprocess stdio → `mimir_call()` → framework memory interface.
Reference: `mimir-development` skill, `references/framework-integration-pattern.md`.

### Week 5–6: Transport Expansion

| Task | Deliverable | Success |
|---|---|---|
| SSE/HTTP transport ship | `src/transport.rs` — working SSE + HTTP endpoints | `curl -N http://localhost:PORT/sse` streams events |
| MCP token auth | `--mcp-token` flag, `Authorization: Bearer` on transport routes | Unauthorized requests get 401 |
| Docker image | `Dockerfile` (Alpine multi-stage) → `ghcr.io/perseus-computing-llc/mimir` | `docker run ghcr.io/...` works with `--db /data/mimir.db` |

**SSE/HTTP transport status:** Core implementation in `src/transport.rs` (SSE stream + POST `/message`). `build_transport_router` now accepts `auth_token: Option<String>`. Mainline wiring needs `--mcp-token` CLI flag.

### Week 7–8: Quality & Discovery

| Task | Deliverable | Success |
|---|---|---|
| Glama TDQS improvement | `outputSchema` + `annotations` on all 31 tools | Glama score ≥ 85 |
| Smithery listing audit | All 31 tools appear in Smithery | `smithery.yaml` + `server.json` verified |
| Windows CI | `windows-latest` runner in `.github/workflows/test.yml` | Green build on Windows |

**Glama pattern:** JSON Schema per return type, `readOnlyHint` / `destructiveHint` annotations, usage guidance disambiguation. Reference: `mimir-development` skill → `references/glama-quality.md`.

### Week 9–10: Stress Testing

| Task | Deliverable | Success |
|---|---|---|
| 100K entity scale test | Script that inserts, recalls, decays 100K entities | <5s FTS5 recall, <30s decay tick |
| Decay batching | Batch UPDATEs in chunks of 1000 | Linear memory, no O(n) fsync |
| Concurrent read/write | Multiple stdio clients sharing one DB | No "database is locked" errors |

**Note:** `decay_tick` was fixed in v1.0.0 (wrapped in transaction). At 100K entities, this becomes ~100K UPDATEs in one transaction — may need chunking.

### Week 11–13: Release

| Task | Deliverable |
|---|---|
| Changelog generation | `git log v1.0.1..HEAD` → `CHANGELOG.md` |
| Release build | `cargo build --release`, strip binary |
| GitHub Release | Tag `v1.1.0`, release notes, binary assets |
| One-line install | `curl -sSf https://get.mimir.perseus.observer | sh` |

---

## Phase 2: v1.2.0 — Multi-Agent & Federation

**Theme:** "One memory engine, many agents, many workspaces."
**Timeline:** Q4 2026 (Oct–Dec) · **13 weeks**

### Week 1–3: Workspace Scoping

| Task | Deliverable | Success |
|---|---|---|
| `workspace_hash` column | Schema migration: add `workspace_hash TEXT DEFAULT ''` to entities, journal, state | Zero-downtime migration on production DB |
| Workspace-scoped queries | All CRUD + query tools accept optional `workspace_hash` | Agent A's memories invisible to Agent B |
| Perseus multi-workspace wiring | `perseus.yaml` per-workspace with `workspace_hash` | `perseus render` scopes to workspace |

### Week 4–6: Agent Identity

| Task | Deliverable | Success |
|---|---|---|
| `agent_id` column | Schema migration | Every entity tracks which agent wrote it |
| `mimir_context` agent filter | `--agent-id` filter on context injection | Agent sees only its own + shared memories |
| Agent attribution in journal | `journal` table gets `agent_id` | Timeline shows who did what |

### Week 7–9: Cross-Workspace Federation

| Task | Deliverable | Success |
|---|---|---|
| Federated vault sync | `mimir_vault_export --workspace A` → `mimir_vault_import --workspace B` | Round-trip preserves all entities |
| Merge conflict resolution | `last_write_wins` + `three_way_merge` strategies | Conflicting entities flagged, not lost |
| `mimir_federate` tool | New MCP tool: pull entities from another workspace | Cross-workspace knowledge sharing |

### Week 10–11: Access Controls

| Task | Deliverable | Success |
|---|---|---|
| `visibility` column | `private`, `workspace`, `public` visibility levels | Private entities never leave workspace |
| `mimir_share` tool | Share entity to another workspace | Shared entity appears in target workspace |
| Token-based workspace access | `--workspace-token` for cross-workspace auth | Unauthorized access returns error |

### Week 12–13: Release

Same release checklist as v1.1.0: changelog, build, tag, release, one-line install update.

---

## Phase 3: v1.3.0 — Offline Embeddings

**Theme:** "Truly zero-dependency semantic search."
**Timeline:** Q1 2027 (Jan–Mar) · **13 weeks**

### Week 1–4: ONNX Runtime Integration

| Task | Deliverable | Success |
|---|---|---|
| Bundle all-MiniLM-L6-v2 | `ort` crate + model file compiled into binary | `mimir_embed` works with no Ollama |
| Model quantization | INT8 quantization → ~23MB model | Binary size increase ≤ 80MB |
| Fallback chain | Local ONNX → external endpoint → error | Graceful degradation |

### Week 5–7: Embedding Pipeline

| Task | Deliverable | Success |
|---|---|---|
| Background embedding generation | `mimir_embed --background` for async batch embedding | New entities auto-embedded |
| Incremental re-embedding | Only embed entities missing `embedding` BLOB | No redundant work |
| Embedding cache | In-memory LRU cache of recent embeddings | Repeated queries hit cache |

### Week 8–10: Hybrid Search Improvements

| Task | Deliverable | Success |
|---|---|---|
| Approximate nearest neighbor | HNSW index via `usearch` or brute-force + SIMD | <100ms for 100K vectors |
| RRF tuning | Configurable `k` parameter for RRF fusion | User-adjustable dense/sparse balance |
| Search benchmark suite | `cargo bench` with 10K/100K entity datasets | Regression detection in CI |

### Week 11–13: Release

Same checklist. Target binary size: ≤12MB stripped (from current ~8MB).

---

## Phase 4: v2.0 — Platform

**Theme:** "Mimir as infrastructure."
**Timeline:** Q2 2027 (Apr–Jun) · **13 weeks**

### Week 1–4: gRPC Transport

| Task | Deliverable | Success |
|---|---|---|
| Protobuf service definition | `mimir.proto` with all 31 tools as RPCs | `protoc` generates client stubs |
| `tonic` server | gRPC server alongside existing MCP stdio | `grpcurl` lists all services |
| Streaming RPCs | `WatchJournal`, `StreamContext` for real-time updates | Client receives push notifications |

### Week 5–7: High Availability

| Task | Deliverable | Success |
|---|---|---|
| Read replicas | `--replica-of` flag, async log shipping | Read queries hit replica, writes hit primary |
| Leader election | `--cluster` mode with Raft consensus | Automatic failover on primary death |
| WAL shipping | SQLite WAL frames streamed to replicas | Replicas ≤1s behind primary |

### Week 8–10: Audit & Compliance

| Task | Deliverable | Success |
|---|---|---|
| Cryptographic audit log | SHA-256 chain: each journal entry hashes previous | Tamper-evident audit trail |
| `mimir_audit_verify` tool | Verify chain integrity end-to-end | Detects any single-bit modification |
| Retention policies | `--retention-days` auto-archives old entities | DB size stays bounded |

### Week 11–12: Mimir Cloud MVP

| Task | Deliverable | Success |
|---|---|---|
| Managed API | `api.mimir.perseus.observer` — REST + gRPC | `mimir connect --cloud` works |
| Usage-based billing | Stripe metered billing per entity/month | First paying customer |
| Cloud dashboard | Web dashboard with multi-tenant isolation | User sees only their workspaces |

### Week 13: Release

Major version bump. Migration guide from v1.x. Backward-compatible MCP tool surface.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| ONNX crate instability | Medium | High (blocks v1.3.0) | Start spike in v1.2.0; fall back to `candle` |
| SQLite concurrency ceiling | Medium | High (blocks v2.0 HA) | Evaluate libSQL or RocksDB as v2.0 pre-work |
| FTS5 scale limits (1M+ entities) | Low | Medium | Paginate + cache; evaluate Tantivy |
| Embedding bundle size | Low | Medium | INT4 quantization; optional download |
| MCP protocol churn | Low | Low | Pin protocol version; add compatibility layer |

---

## Success Metrics

| Phase | Metric | Target |
|---|---|---|
| v1.1.0 | GitHub stars | 200+ |
| v1.1.0 | Glama TDQS | ≥85 |
| v1.1.0 | Framework integrations | 3 shipped (LangGraph, CrewAI, AutoGen) |
| v1.2.0 | Concurrent workspaces | 10 simultaneous |
| v1.2.0 | Cross-workspace sync time | <5s for 10K entities |
| v1.3.0 | Binary size | ≤12MB with embedded model |
| v1.3.0 | Dense search latency | <100ms at 100K scale |
| v2.0 | HA failover time | <5s |
| v2.0 | Mimir Cloud customers | ≥1 paying |

---

## Build & Deploy

```bash
REPO="/opt/data/webui/minions/.minions-data/workspace/mimir"
cd "$REPO"
source "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release
cp target/release/mimir /usr/local/bin/mimir
cp target/release/mimir /opt/data/webui/minions/.minions-data/mimir/mimir
cargo test
```

**Production paths:**
| What | Path |
|---|---|
| Binary (persistent) | `/opt/data/webui/minions/.minions-data/mimir/mimir` |
| Binary (in-container) | `/usr/local/bin/mimir` |
| Production DB | `/opt/data/webui/minions/.minions-data/mimir/mimir.db` |
| Workspace repo | `/opt/data/webui/minions/.minions-data/workspace/mimir` |

---

## Appendix: Competitive Landscape

| System | Stars | Type | MCP-Native | Local-First | Zero Deps | Agent-First |
|---|---|---|---|---|---|---|
| **Mimir** | — | Memory engine | ✅ | ✅ | ✅ | ✅ |
| Mem0 | ~55K | Cloud memory | ❌ | ❌ | ❌ | Partial |
| Letta | ~15K | Agent runtime | Partial | Partial | ❌ | ✅ |
| OMEGA | ~2K | Local memory | Partial | ✅ | ❌ | Partial |
| Zep | ~3K | Cloud memory | ❌ | ❌ | ❌ | Partial |

Mimir is the only system that is simultaneously MCP-native, local-first, zero-dependency, and agent-first.

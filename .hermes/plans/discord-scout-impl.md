# Implementation Plan: mcp-pager + recall_when + coherence daemon

All three features go into Mimir (Rust). No new schema columns needed — all new
fields go inside the existing `body_json` column (flexible JSON).

## 1. Native Pagination (mcp-pager pattern)

**Goal:** Add cursor/offset pagination to large-result tools so they don't overflow
context windows. Pattern: return `has_more`, `total`, `next_offset` metadata.

**Files:**
- `src/models.rs` — add `offset: i64` to `RecallParams`, `TimelineParams`
- `src/tools.rs` — add `offset` to `RecallArgs`, `TimelineArgs`; compute `has_more` by fetching `limit+1` rows
- `src/db.rs` — add `OFFSET` clause to fts5_search when offset > 0

**Tools affected:**
- `mimir_recall` — add offset param, return pagination metadata
- `mimir_timeline` — add offset param, return pagination metadata
- `mimir_context` — add offset param (already has limit)

## 2. recall_when — Proactive Recall Hooks

**Goal:** Entities can declare trigger conditions (`recall_when` fields), and a new
`mimir_recall_when` tool matches incoming context against those triggers for
just-in-time memory injection.

**Files:**
- `src/tools.rs` — add `recall_when` to `RememberArgs`; add `RecallWhenArgs` and handler
- `src/db.rs` — add `recall_when_search()` method
- `src/mcp.rs` — register `mimir_recall_when` tool

**How it works:**
- On `mimir_remember`, an optional `recall_when: ["trigger text", ...]` is stored in body_json
- `mimir_recall_when(context_text)` searches entities where any recall_when trigger matches
  the given context, returning sorted by relevance
- The Perseus context engine can call this before tool calls to inject relevant memories

## 3. Coherence Daemon (mimir_cohere)

**Goal:** A background maintenance tool that an agent or cron can call to groom
the memory. Promotes, decays, links, and archives entities.

**Files:**
- `src/models.rs` — add `CohereReport` struct
- `src/db.rs` — add `cohere()` method
- `src/tools.rs` — add handler
- `src/mcp.rs` — register `mimir_cohere` tool

**Operations:**
1. **Promote** — entities in "buffer" layer with retrieval_count ≥ 3 → "working"
2. **Decay** — apply Ebbinghaus decay to all non-archived entities
3. **Link** — find entities with high semantic overlap (shared tags/topics) and auto-link them
4. **Archive** — entities with decay_score < 0.05 → archived
5. **Return** — CohereReport with counts for each operation

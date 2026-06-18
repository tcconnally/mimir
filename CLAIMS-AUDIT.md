# Claims Audit — mimir

> **Update (2026-06):** The tool surface has grown since this audit. As of v1.0.1
> there are **30** registered `mimir_*` tools (verified in `tools/list` and the
> `tools/call` dispatch). The "23" figure below was accurate for v0.5.0 and is
> kept as a historical record. See #130.

**Date:** 2026-06-12 · **Audited:** README.md vs code on `main` (v0.5.0)

## Findings

### LOW — no material gaps found in this repo

Claims checked against `src/`:

- **"MCP-native"** — full JSON-RPC stdio server (`initialize`, `tools/list`, `tools/call`); verified live by the new smoke test, which performs a real handshake against a fresh database and asserts `mimir_remember` is advertised. ✓
- **"SQLite + FTS5"** — schema.rs builds FTS5 tables; recall uses FTS5 queries. ✓
- **"Fully local"** — no network dependencies in Cargo.toml runtime deps. ✓
- **23 MCP tools** — exactly 23 distinct `mimir_*` tool names registered in `src/mcp.rs`. ✓
- **Rust test coverage** — 15 `#[test]` functions across db/schema/main/mcp. ✓

### Note for downstream consumers

The perseus README describes Mimir's tools with names that don't exist
(`mimir_store`, `mimir_entity_*`, `mimir_layer_*`, `mimir_decay_config`).
Actual surface: `mimir_remember`, `mimir_recall`, `mimir_forget`,
`mimir_link`/`mimir_unlink`/`mimir_traverse`, `mimir_journal`,
`mimir_timeline`, `mimir_state_*`, `mimir_vault_*`, `mimir_decay`,
`mimir_compact`, `mimir_conflicts`, `mimir_context`, `mimir_score`,
`mimir_stats`, `mimir_health`, `mimir_migrate`, `mimir_workspace_list`.
That finding is filed against the perseus repo, not this one.

# Claims Audit — mimir

**Date:** 2026-06-28 (refreshed) · **Audited:** README.md vs code on `main` (v2.6.0)

## Findings

### LOW — no material gaps found

Claims verified against `src/`:

- **43 MCP tools** — exactly 43 distinct `mimir_*` tool names registered in source (`src/*.rs`). README, badge (v2.6.0), and the "43 MCP Tools" section all agree. ✓
- **MCP-native** — full JSON-RPC stdio server (`initialize`, `tools/list`, `tools/call`). ✓
- **SQLite + FTS5** — schema builds FTS5 tables; recall uses FTS5 queries. ✓
- **AES-256-GCM encrypted** — encryption at rest for entity bodies. ✓
- **Fully local / zero-dependency** — no network runtime deps in `Cargo.toml`. ✓
- **Sub-millisecond recall** — bundled offline embeddings, no external model download. ✓

## History

- 2026-06-12 (v0.5.0): 23 tools. 2026-06 interim: 30 tools (#130). Now **43** (v2.6.0).
  Earlier figures kept as historical record only.

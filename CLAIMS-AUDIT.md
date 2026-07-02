# Claims Audit — Perseus Vault (formerly Mimir/Mneme)

**Date:** 2026-07-01 (refreshed) · **Audited:** README.md vs code on `main` (v2.13.0)

## Findings

### LOW — no material gaps found

Claims verified against `src/`:

- **49 MCP tools** — exactly 49 distinct `mimir_*` tool names registered in
  source (`src/mcp.rs` TOOLS schema); each is additionally exposed under
  `mneme_*` and `perseus_vault_*` aliases (same handler, not counted).
  README badge, comparison table, and the "49 MCP Tools" section all agree. ✓

  Verify the count against source (this is the authoritative command — re-run
  it and update README/manifest.json/glama.json whenever a tool is added):

  ```bash
  grep -o '"name": "mimir_[a-z_]*"' src/mcp.rs | sort -u | wc -l
  ```

- **MCP-native** — full JSON-RPC stdio server (`initialize`, `tools/list`, `tools/call`). ✓
- **SQLite + FTS5** — schema builds FTS5 tables; recall uses FTS5 queries. ✓
- **AES-256-GCM encrypted** — encryption at rest for entity bodies. ✓
- **Fully local / zero-dependency** — no network runtime deps in `Cargo.toml`. ✓
- **Sub-millisecond recall** — bundled offline embeddings, no external model download. ✓

## History

- 2026-06-12 (v0.5.0): 23 tools. 2026-06 interim: 30 tools (#130). 2026-06-28
  (v2.6.0): 46 (#271 mimir_semantic_search, #269 mimir_recall_layer, review
  follow-up mimir_history). Now **49** (#327 mimir_consolidate, #332
  mimir_follow, #345 mimir_memories).
  Earlier figures kept as historical record only.

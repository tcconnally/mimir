# Changelog

All notable changes to Mimir are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [2.8.0] - 2026-06-30

### Changed
- **Product rename: Mimir → Mneme.** Avoids a trademark/SEO collision with
  Grafana Mimir and a same-niche competitor also named Mimir. The crate and
  `[[bin]]` are now `mneme`; the default database for fresh installs is
  `~/.mimir/data/mneme.db` (an existing `mimir.db` at that path is still used
  automatically, so upgraders keep their data — see `default_db_path()` in
  `src/main.rs`). Every `mimir_*` MCP tool is now also registered under the
  equivalent `mneme_*` name — both dispatch to the same handler, so existing
  MCP host configs that call `mimir_remember`/`mimir_recall`/etc. keep working
  unchanged during the transition. `mimir doctor`/`--help` output now refers to
  the `mneme` binary. Internal-only Rust identifiers (`MimirGrpcServer`, the
  optional `grpc` feature's generated `Mimir`/`MimirServer` proto types) are
  renamed to their `Mneme` equivalents with no back-compat surface, since
  nothing outside the binary depends on them.

### Fixed
- **`layer` filter on `mimir_recall` now actually filters (#269 follow-up).** The
  `layer` recall parameter was accepted but never applied — `RecallParams.layer`
  was a dead field. It now filters by biomimetic layer in all three modes:
  keyword (`fts5_search`) and BM25 (`fts5_bm25_search`) pre-filter in-query, and a
  mode-agnostic post-filter in `recall()` covers the dense arm of dense/hybrid
  (which scores vectors without `RecallParams` access). Aliases world/episodic/
  semantic are normalized to core/buffer/working at the tools layer.

### Added
- **`mimir_history` tool (code-review follow-up).** The bi-temporal `history_versions`
  reader (v2.4.0) was complete and tested but no tool exposed it — you could time-travel
  to one instant via `mimir_as_of` but couldn't list a fact's full version trail. Wired a
  `mimir_history` tool that returns all superseded versions of a (category, key), newest
  first. Tool count 45 → **46**; README badge/table/section, `server.json`, and
  `CLAIMS-AUDIT.md` reconciled (they had drifted to 44/43).

### Removed
- **Dead `EncryptionManager::decrypt`.** Fully superseded by `decrypt_body` (the
  legacy/auth-failure-classifying variant); the old method had zero callers and was the
  exact footgun the security fix replaced. Removed so it can't be reintroduced.

- **`mimir doctor` + verified client compatibility matrix (#272).** New `mimir doctor`
  subcommand validates the local install (binary path, db path) and prints the MCP
  stdio config plus a compatibility matrix for Claude Desktop, Claude Code/Hermes,
  Cursor, Windsurf, VS Code+Continue.dev, Zed, and Codex CLI. Added a "Works With
  Every MCP Client" table to the README and copy-paste config snippets in
  `docs/clients/`. Mimir is a standard MCP stdio server, so the same command works
  everywhere — this documents and self-checks it.
- **`include_confidence` on `mimir_recall` (#287).** Opt-in (default false): each result
  gains a normalized `confidence` (0.0–1.0) rolled up from rank, trust (verified/certainty),
  and decay — a single number for callers/UIs instead of eyeballing raw signals. Purely
  presentation-layer; ranking math and existing snapshots are unchanged.

### Security
- **Decryption failures no longer silently return ciphertext.** On an encrypted DB,
  the read path (`entity_from_row`), FTS reindex, and the history content-change
  check used `decrypt(...).unwrap_or(raw)`, so any authentication failure — wrong
  key, or AAD-mismatched / tampered ciphertext (exactly what AES-256-GCM + AAD exist
  to detect) — was swallowed and the raw ciphertext was returned/indexed as if it
  were the plaintext body. That nullified the integrity guarantee: an attacker who
  could write to the DB file could tamper with a body and have it surface
  undetected. New `EncryptionManager::decrypt_body` classifies the input as
  decrypted plaintext, a legacy plaintext row (a real JSON body is never valid
  base64, so mixed DBs still work), or an authentication failure — and read paths
  now refuse to return the bytes on failure (a clear error sentinel + stderr warning
  for recall; an empty FTS entry so ciphertext is never indexed). Regression tests
  cover roundtrip, legacy-plaintext passthrough, and tamper / wrong-AAD / wrong-key
  rejection.

## [2.7.0] - 2026-06-28

### Distribution
- **Published to the Official MCP Registry (#270).** Fixed `server.json` (valid
  `oci` package on GHCR, current version and 43 tool count, dropped a stale
  install line) and added the OCI ownership label to the Docker image, so Mimir
  is discoverable at registry.modelcontextprotocol.io and the directories that
  crawl it (Glama, PulseMCP, mcp.so).

## [2.6.0] - 2026-06-28

Round-3 hardening & efficiency: a data-loss fix on encrypted databases, an
ingest DoS guard, a lean-build injection fix, and recall-quality + perf
improvements.

### Changed
- **Hybrid recall over-fetches each arm before RRF fusion.** The dense and BM25
  keyword arms were each pre-truncated to `limit` *before* being fused, so a hit
  ranked just past `limit` in one arm but strong in the other — or one that lands
  just past `limit` in *both* yet would have the best *fused* score — could never
  enter fusion. Each arm is now fetched at a larger candidate pool (≈`5×limit`,
  capped) and RRF truncates to `limit` afterward. Strictly a recall-quality
  improvement; still fully read-only and byte-deterministic (verified by the
  existing idempotency/#125 tests + a new `hybrid_over_fetches_arms_before_fusion`
  test that pins the cross-arm consensus hit). The `mimir-recall-mini` headline
  metrics are unchanged (24 docs saturate at `limit=10`), but the benchmark
  signature updates as the fused tail re-orders.
- **Conflict scan window is now an explicit, wider constant.** `detect_conflicts`
  / `resolve_conflicts` hard-coded a `LIMIT 200` candidate window (the O(window²)
  pairwise scan only ever looked at the 200 most-recently-accessed entities per
  call). Replaced the magic number with a documented `CONFLICT_SCAN_WINDOW` (500),
  widening coverage; still paged by `offset`.

### Performance
- **Scalar `dense_search` fallback precomputes the query norm once.** The
  non-`bundled-embeddings` (lean-build) cosine path recomputed the query vector's
  norm for every candidate; it is constant across a search, so it is now computed
  once and only the dot product + candidate norm are per-row. No effect on the
  default (vectorized ndarray) build.

### Fixed
- **`mimir_reindex` no longer breaks keyword search on encrypted databases.**
  `reindex_fts` did a raw `INSERT … SELECT body_json`, which on an encrypted DB
  copied **ciphertext** into the FTS5 index — silently breaking all keyword and
  hybrid recall until re-ingest (the recovery tool corrupted the very index it was
  meant to rebuild). It now decrypts each body (AAD `category:key`) and indexes the
  plaintext, matching what `remember` writes. Unencrypted DBs keep the fast bulk
  copy. Regression test added.

### Security
- **Bounded file size for `mimir_ingest_file` (#236 hardening).** Document ingestion
  read the entire file into memory with no size limit, then copied the text into a
  JSON body and the FTS index — a single huge or maliciously-sized file could OOM
  the server (denial of service). Ingestion now rejects files larger than a
  configurable cap (`MIMIR_MAX_INGEST_BYTES`, default 50 MiB) **before** reading,
  for plaintext, DOCX and PDF alike. Regression test added.
- **Python embedding fallback no longer interpolates text into its script.** The
  lean-build ONNX fallback (`generate_with_python`) escaped only `\` and `'` when
  embedding the (agent/user-controlled) text into a `python3 -c` source string, so
  a newline or other control character could break out of the string literal — a
  code-injection / DoS hazard. The tokenizer path, model path and text are now
  passed as **`argv`** (never parsed as code). Affects only `--no-default-features`
  builds (the default uses the in-process ONNX runtime).

## [2.5.0] - 2026-06-27

Bi-temporal facts, completed: conflicting facts can now be actively resolved
(not just detected), with the loser superseded into history rather than deleted.

### Added
- **Opt-in conflict invalidation (#253).** `mimir_conflicts` gains `resolve=true`:
  the lower-certainty side of a clear conflict is invalidated — superseded into
  `entity_history` and removed from the live table, so it drops out of recall but
  stays reversible and time-travelable via `mimir_as_of`. Conservative by design:
  `dry_run` defaults to **true** (an accidental `resolve` previews, never mutates),
  and pairs whose certainties are within `certainty_margin` (default 0.2) are
  skipped as ambiguous. Detection (`resolve=false`) is unchanged and remains the
  default. New `Database::invalidate_entity` / `Database::resolve_conflicts`.

### Tested
- **History-resurrection invariant guard (#257).** Locks in that superseded
  versions and conflict-invalidated losers (both in `entity_history`) are never
  resurfaced by `decay_tick` or `recall` — the architecture already guarantees
  this; the guard fails loudly if a future change breaks it.

## [2.4.0] - 2026-06-27

Bi-temporal facts: Mimir now keeps a fact's prior versions when it changes and
can answer "what did we believe at time T?" — pure SQLite, local, no cloud.

### Added
- **Bi-temporal fact history (#249, #250, #251).** When `remember()` overwrites
  an existing `(category, key)` with new content, the prior version is now
  snapshotted into a new `entity_history` table instead of being lost. Each
  entity gains two time axes — **valid time** (`valid_from`/`valid_to`) and
  **transaction time** (`recorded_at`/`invalidated_at`) — plus
  `supersedes`/`superseded_by` links. The live `entities` table stays
  one-row-per-key (its `UNIQUE(category, key)`, recall, and dedup paths are
  untouched), so default recall remains live-only by construction. An identical
  re-assertion creates no version (idempotent, compared on plaintext).
- **`mimir_as_of` tool + `Database::as_of(category, key, as_of_unix_ms)`.**
  Bi-temporal time-travel: returns the version of a fact that was live at a past
  instant (or `found=false` if it had not been recorded yet). Brings the MCP
  tool count to **43**.

### Changed
- `recorded_at_unix_ms` is now set to `created_at_unix_ms` on insert; the
  `user_version` 1→2 migration backfills it for existing rows and adds the
  bi-temporal columns + the `idx_entities_invalidated` live-fact index.

### Documentation
- Reconciled the README tool count (badge / comparison table / section header)
  from a stale **40** to the actual **43**, adding the missing `mimir_extract`,
  `mimir_ingest_file` (both shipped in 2.3.0) and `mimir_as_of` rows.

## [2.3.0] - 2026-06-27

Local, offline knowledge tooling — structured extraction and multimodal document
ingestion — plus a reproducible recall-quality benchmark and a relevance-aware,
deterministic hybrid retrieval path.

### Added
- **Local multimodal document ingestion (#236).** New `mimir_ingest_file` tool
  extracts a document's text **locally** (no cloud, no network) and stores it as a
  recallable entity. Plaintext / markdown / structured-text work in any build;
  **DOCX and PDF** are supported when built with the new optional
  `--features multimodal` (pulls `zip` + `pdf-extract`), keeping the lean default
  binary dependency-free. Brings the MCP tool count to **42**.
- **Local knowledge extraction (#234).** New `mimir_extract` tool turns raw text
  (or a stored entity) into structured items — facts, preferences, temporal
  events, episodes — via a fully **local, deterministic, rule-based** extractor:
  no cloud LLM, no embedding/API call, no network (unlike GoodMem/Synap, which
  require a Gemini key). **Read-only and strictly opt-in** — the remember/recall
  paths and the zero-dependency story are unchanged. An `Extractor` trait is the
  plugin point for future strategies (`strategy: "none"` is a no-op). Brings the
  MCP tool count to **41**.
- **Reproducible offline recall-quality benchmark (#247).** New `benchmark/recall/`
  measures recall@k / MRR across `fts5` / `dense` / `hybrid` modes by driving the
  real binary over MCP stdio with the **bundled** ONNX model — no network, no API
  key, no LLM — and emits a signed, re-runnable `report.json`. On the
  paraphrase-heavy `mimir-recall-mini` set the offline dense model reaches **91.7%
  recall@1 / 100% recall@5**, making the local-first promise measurable.

### Changed
- **Relevance-aware, deterministic hybrid recall (#247).** The hybrid (Reciprocal
  Rank Fusion) keyword arm now drops stopwords and ranks by **BM25 relevance**
  instead of popularity, is dropped entirely when it finds no content match, and
  is fused at a reduced dense-primary weight — so a paraphrase query no longer
  dilutes a confident dense ranking. RRF breaks score ties by entity id and the
  hybrid recall path is fully read-only, making all three modes **byte-stable
  run-to-run**. Hybrid recall@1 on `mimir-recall-mini`: **20.8% → 87.5%** (MRR
  0.44 → 0.92).

### Documentation
- **Threat model + encryption spec (#246).** Added `docs/THREAT-MODEL.md` and
  `docs/ENCRYPTION.md` and corrected SECURITY.md overclaims. AES-256-GCM encrypts
  only `entities.body_json`; the FTS5 index and metadata are **plaintext** (pair
  with OS disk encryption).

## [2.2.1] - 2026-06-27

### Fixed
- **Docker/Alpine image builds again (#242).** The bundled-embeddings default
  (#237/#238) broke the musl Docker build — `ort` (ONNX Runtime) prebuilt
  binaries are glibc-only and the download chain needs `openssl-sys`, absent on
  Alpine. The Firecracker/sandbox image now builds **lean** (`--no-default-features`),
  restoring a working static-musl binary and the GHCR publish. (Native binaries
  remain bundled-by-default; a semantic-search Docker image would need a glibc base.)

## [2.2.0] - 2026-06-27

Local-first semantic memory, now true out of the box and on every platform, plus
the first time-aware retrieval control. The headline since `2.1.0`: dense/hybrid
search works with zero config and zero network by default.

### Added
- **Time-aware / recency-boosted hybrid recall (#235).** `mimir_recall` accepts an
  optional `recency_half_life_secs` for `mode: "hybrid"`. When set, each fused
  (RRF) result's score is multiplied by `0.5^(age / half_life)` based on the
  memory's creation time, so recent context outranks older but lexically/semantically
  similar hits. **Default off** — omitting it preserves the existing relevance-only
  ranking exactly. Fully local, no new dependency; memories with no creation
  timestamp are never penalized.
- **Offline dense/hybrid search out of the box (#237).** A quantized
  all-MiniLM-L6-v2 model (int8, ~23 MB, 384-dim) is now fetched once by `build.rs`
  and **compiled into the binary**, and the embedding backend is **enabled by
  default**. Semantic recall works with zero config and zero network — no Ollama,
  no API key, no first-run model download — making the local-first / fully-offline
  promise literally true. Build a lean binary without the embedding stack via
  `cargo build --no-default-features`.

### Fixed
- **Native ONNX embedding now passes `token_type_ids`.** The `ort` inference path
  sent only `input_ids` + `attention_mask`; the (quantized) BERT graph requires
  the `token_type_ids` input (all-zeros for a single sequence), so native
  embedding failed at runtime. Now passed explicitly.

### CI
- The default build (now bundled-embeddings) is built **and tested** on **Linux,
  Windows MSVC, and macOS** (#239) — including an end-to-end test that runs real
  inference through the compiled-in model — confirming the single-binary
  semantic-search claim on every platform a developer runs. Added a `lite-build`
  job guarding `--no-default-features`.

[2.2.1]: https://github.com/Perseus-Computing-LLC/mneme/compare/v2.2.0...v2.2.1
[2.2.0]: https://github.com/Perseus-Computing-LLC/mneme/compare/v2.1.0...v2.2.0

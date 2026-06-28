# Changelog

All notable changes to Mimir are documented here. This project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- **`mimir_reindex` no longer breaks keyword search on encrypted databases.**
  `reindex_fts` did a raw `INSERT … SELECT body_json`, which on an encrypted DB
  copied **ciphertext** into the FTS5 index — silently breaking all keyword and
  hybrid recall until re-ingest (the recovery tool corrupted the very index it was
  meant to rebuild). It now decrypts each body (AAD `category:key`) and indexes the
  plaintext, matching what `remember` writes. Unencrypted DBs keep the fast bulk
  copy. Regression test added.

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

[2.2.1]: https://github.com/Perseus-Computing-LLC/mimir/compare/v2.2.0...v2.2.1
[2.2.0]: https://github.com/Perseus-Computing-LLC/mimir/compare/v2.1.0...v2.2.0

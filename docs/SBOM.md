# Mimir SBOM (Software Bill of Materials)
## For Federal Procurement Compliance

**Package:** mimir v2.0.0
**License:** MIT
**Repository:** https://github.com/Perseus-Computing-LLC/mneme
**Language:** Rust (edition 2021)
**Format:** SPDX Lite / NTIA Minimum Elements

---

## SBOM Metadata

| Field | Value |
|---|---|
| Supplier | Perseus Computing LLC |
| Supplier Contact | perseus@perseus.observer |
| SBOM Author | Perseus Computing LLC |
| Timestamp | 2026-06-20T14:08:00-05:00 |
| SBOM Format | NTIA Minimum Elements + SPDX Lite |

---

## Dependency Inventory

### Runtime Dependencies (Direct)

| Crate | Version | License | Crate Type |
|---|---|---|---|
| serde | 1.x | MIT OR Apache-2.0 | Serialization framework |
| serde_json | 1.x | MIT OR Apache-2.0 | JSON support |
| rusqlite | 0.31 (bundled) | MIT | SQLite bindings (bundles libsqlite3) |
| uuid | 1.x | MIT OR Apache-2.0 | UUID generation (v4) |
| clap | 4.x | MIT OR Apache-2.0 | CLI argument parsing |
| rust-stemmers | 1.2 | MIT OR Apache-2.0 | Porter stemming for FTS5 |
| aes-gcm | 0.10 | MIT OR Apache-2.0 | AES-256-GCM encryption |
| rand | 0.8 | MIT OR Apache-2.0 | Random number generation |
| base64 | 0.22 | MIT OR Apache-2.0 | Base64 encoding |
| axum | 0.7 | MIT | Web framework |
| tokio | 1.x (full) | MIT | Async runtime |
| tower-http | 0.5 | MIT | HTTP middleware (CORS) |
| ureq | 2.x | MIT OR Apache-2.0 | HTTP client |
| notify | 6.x | CC0-1.0 | File system watcher |
| serde_yaml | 0.9 | MIT OR Apache-2.0 | YAML support |
| async-stream | 0.3 | MIT | Async stream macros |
| futures | 0.3 | MIT OR Apache-2.0 | Async primitives |

### Bundled/Embedded

| Component | Version | License |
|---|---|---|
| SQLite (libsqlite3) | bundled via rusqlite 0.31 | Public Domain |

### Optional Dependencies

| Crate | Version | License | Required For |
|---|---|---|---|
| ort | 2.0.0-rc.12 | MIT | ONNX Runtime (bundled embeddings) |
| tokenizers | 0.23 | Apache-2.0 | Text tokenization |
| ndarray | 0.16 | MIT OR Apache-2.0 | Numerical arrays |
| tonic | 0.13 | MIT | gRPC server |
| prost | 0.14 | Apache-2.0 | Protobuf |
| prost-types | 0.14 | Apache-2.0 | Protobuf well-known types |
| tokio-stream | 0.1 | MIT | Async streaming for gRPC |

### Dev Dependencies

| Crate | Version | License |
|---|---|---|
| tower | 0.5 | MIT |

---

## Supply Chain Summary

| Metric | Value |
|---|---|
| Total direct dependencies (runtime) | 17 |
| Embedded dependencies | 1 (SQLite) |
| Optional dependencies | 7 |
| Dependencies with known CVEs | 0 |
| Copyleft licenses (GPL/AGPL) | 0 |
| Foreign-owned crates | 0 (all crates.io, all permissive) |
| Cryptography implemented | AES-256-GCM (via aes-gcm 0.10) |

---

## Security Assessment

- [x] All dependencies are MIT/Apache-2.0 licensed — no copyleft risk
- [x] SQLite bundled via rusqlite — no system library dependency
- [x] AES-256-GCM encryption at rest for stored entities
- [x] No network dependencies required at runtime (HTTP/GitHub connectors optional)
- [x] ureq is a minimalist HTTP client — small attack surface
- [ ] No code signing on release binaries (TODO)
- [ ] No SLSA provenance attestations (TODO for FedRAMP)
- [ ] Third-party security audit not yet performed (TODO for FedRAMP ATO)

---

## Cryptographic Module Listing

| Module | Algorithm | Key Size | Purpose |
|---|---|---|---|
| aes-gcm 0.10 | AES-256-GCM | 256-bit | Entity body encryption at rest |
| uuid 1.x | UUID v4 (random) | 122-bit entropy | Entity ID generation |

---

## NTIA Minimum Elements Checklist

- [x] Supplier name: Perseus Computing LLC
- [x] Component name: mimir
- [x] Version string: 2.0.0
- [x] Unique identifier: crates.io:mimir@2.0.0
- [x] Dependency relationship: listed above
- [x] SBOM author: Perseus Computing LLC
- [x] Timestamp: included

# Software Bill of Materials (SBOM) for Mimir

This Software Bill of Materials (SBOM) lists the direct dependencies of the Mimir project, version 2.0.0, to assist with federal procurement compliance and transparency.

## NTIA Minimum Elements Checklist

*   **Suppliers:** Perseus-Computing-LLC
*   **Component Name:** mimir
*   **Component Version:** 2.0.0
*   **Timestamp:** 2026-06-20T12:00:00Z (YYYY-MM-DDTHH:MM:SSZ)
*   **Author of SBOM data:** Hermes Agent

## Direct Rust Crate Dependencies

The following crates are direct dependencies as specified in `Cargo.toml`:

| Crate Name     | Version   | License (where available) |
|----------------|-----------|---------------------------|
| serde          | 1         | MIT / Apache-2.0          |
| serde_json     | 1         | MIT / Apache-2.0          |
| rusqlite       | 0.31      | MIT / Apache-2.0          |
| uuid           | 1         | MIT / Apache-2.0          |
| clap           | 4         | MIT / Apache-2.0          |
| rust-stemmers  | 1.2       | MIT                       |
| aes-gcm        | 0.10      | Apache-2.0 / MIT          |
| rand           | 0.8       | MIT / Apache-2.0          |
| base64         | 0.22      | MIT / Apache-2.0          |
| axum           | 0.7       | MIT / Apache-2.0          |
| tokio          | 1         | MIT / Apache-2.0          |
| tower-http     | 0.5       | MIT / Apache-2.0          |
| ureq           | 2         | MIT                       |
| notify         | 6         | MIT                       |
| serde_yaml     | 0.9       | MIT / Apache-2.0          |
| async-stream   | 0.3       | Apache-2.0                |
| futures        | 0.3       | MIT / Apache-2.0          |

**Optional Dependencies (when features are enabled):**

| Crate Name     | Version    | Feature            | License (where available) |
|----------------|------------|--------------------|---------------------------|
| ort            | 2.0.0-rc.12| bundled-embeddings | MIT / Apache-2.0          |
| tokenizers     | 0.23       | bundled-embeddings | Apache-2.0                |
| ndarray        | 0.16       | bundled-embeddings | MIT / Apache-2.0          |
| tonic          | 0.13       | grpc               | MIT / Apache-2.0          |
| prost          | 0.14       | grpc               | Apache-2.0                |
| prost-types    | 0.14       | grpc               | Apache-2.0                |
| tokio-stream   | 0.1        | grpc               | MIT / Apache-2.0          |

---
This SBOM was generated on 2026-06-20 by Hermes Agent.

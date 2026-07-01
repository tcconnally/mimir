# Glama-compatible Dockerfile for Perseus Vault (formerly Mneme/Mimir)
# Builds a static musl binary for Firecracker microVM sandbox execution.
#
# This is the LEAN build (--no-default-features): no bundled ONNX embeddings.
# The bundled-embeddings default (#237) links ONNX Runtime via `ort`, whose
# prebuilt binaries are glibc-only and don't work on Alpine/musl (and the
# download path pulls in openssl-sys, absent here). A single static musl binary
# is the right artifact for the Firecracker sandbox; FTS5 keyword recall works
# out of the box, and dense/hybrid search can use an external embedder. For a
# semantic-search-by-default image, use a glibc base (see issue/roadmap).
FROM rust:1.96-alpine AS builder
RUN apk add --no-cache musl-dev sqlite-dev
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY build.rs ./
RUN cargo build --release --no-default-features && strip target/release/perseus-vault

FROM alpine:3.21
# Ownership proof for the MCP Registry (must equal server.json "name").
# NOTE: left as "mimir" deliberately — the MCP Registry name is a separately
# re-verified identity (see server.json), not part of this code-level rename.
LABEL io.modelcontextprotocol.server.name="io.github.Perseus-Computing-LLC/mimir"
RUN apk add --no-cache sqlite-libs
COPY --from=builder /app/target/release/perseus-vault /usr/local/bin/perseus-vault
# Perseus Vault rename (transition release): keep "mneme" and "mimir" symlinks
# so existing `docker run`/compose configs that override CMD with either older
# command name keep working unchanged.
RUN ln -s /usr/local/bin/perseus-vault /usr/local/bin/mneme && \
    ln -s /usr/local/bin/perseus-vault /usr/local/bin/mimir
ENTRYPOINT ["/usr/local/bin/perseus-vault"]
CMD ["serve", "--db", "/data/perseus-vault.db"]

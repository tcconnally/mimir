# Glama-compatible Dockerfile for Mimir
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
RUN cargo build --release --no-default-features && strip target/release/mimir

FROM alpine:3.21
RUN apk add --no-cache sqlite-libs
COPY --from=builder /app/target/release/mimir /usr/local/bin/mimir
ENTRYPOINT ["/usr/local/bin/mimir"]
CMD ["serve", "--db", "/data/mimir.db"]

# Mimir — persistent memory for AI agents
# Single binary, zero runtime deps, < 15 MB image

FROM rust:1.85-alpine AS builder
RUN apk add --no-cache musl-dev sqlite-dev
WORKDIR /build
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl \
    && strip target/x86_64-unknown-linux-musl/release/mimir

FROM scratch
COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/mimir /mimir
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/mimir"]
CMD ["--db", "/data/mimir.db", "--web", "--web-port", "8080"]

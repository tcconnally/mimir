# Contributing to Mimir

Thanks for wanting to help! Mimir is a Rust MCP server for persistent AI agent
memory. Contributions of all kinds are welcome — code, docs, bug reports, feature
ideas.

## Development Setup

```bash
git clone https://github.com/Perseus-Computing-LLC/mneme.git
cd mimir

# Build (requires Rust 1.70+)
cargo build --release

# Run tests
cargo test

# Run with a test database
cargo run -- --db /tmp/mimir-test.db
```

**Project structure:**

```
src/
  main.rs    — CLI entrypoint, arg parsing
  mcp.rs     — MCP JSON-RPC 2.0 protocol (stdio)
  tools.rs   — Tool implementations (store, recall, health, stats)
  db.rs      — SQLite + FTS5 storage layer
```

## Pull Request Workflow

1. Fork the repo
2. Create a feature branch (`git checkout -b feat/my-feature`)
3. Make your changes
4. Run `cargo test` and `cargo fmt`
5. Push and open a PR against `main`

Keep PRs focused — one concern per PR. If you're fixing a bug, add a test.

## Code Style

- `cargo fmt` (standard Rust formatting)
- `cargo clippy` for linting
- Keep functions small and single-purpose
- Add doc comments for public items

## Questions?

Open a [discussion](https://github.com/Perseus-Computing-LLC/mneme/discussions) or file an
issue with the `question` label.

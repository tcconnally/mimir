# General MCP Integration Guide

Mimir is an MCP stdio server. It works with **any** MCP-compatible client.

## Bootstrap (60 seconds)

```bash
# Install Mimir
curl -sSL https://raw.githubusercontent.com/Perseus-Computing-LLC/mneme/main/scripts/bootstrap.sh | bash

# Create data directory
mkdir -p ~/.mimir/data

# Verify it works
/usr/local/bin/mimir --version
```

## MCP Client Configuration

All MCP clients use the same pattern. The exact config format varies by client:

### stdio transport (universal)

```yaml
# Generic config
command: /usr/local/bin/mimir
args:
  - "--db"
  - "~/.mimir/data/mimir.db"
```

### Client-specific formats

| Client | Config file | Format |
|---|---|---|
| **Claude Code** | `claude mcp add` | CLI command (see [guide](claude-code.md)) |
| **Cursor** | `.cursor/mcp.json` | JSON (see [guide](cursor.md)) |
| **Codex** | `.codex/mcp.json` or `~/.codex/mcp.json` | JSON |
| **Hermes Agent** | `config.yaml` | YAML |
| **Continue** | `~/.continue/config.json` | JSON |
| **Cline** | VS Code settings | JSON |
| **Roo Code** | `.roomodes` | JSON |

### Hermes Agent config

```yaml
mcp_servers:
  mimir:
    command: "/usr/local/bin/mimir"
    args: ["--db", "/home/YOUR_USER/.mimir/data/mimir.db"]
    timeout: 60
    connect_timeout: 30
```

### Codex config

```json
{
  "mcpServers": {
    "mimir": {
      "command": "/usr/local/bin/mimir",
      "args": ["--db", "~/.mimir/data/mimir.db"]
    }
  }
}
```

### Continue config

```json
{
  "experimental": {
    "mcpServers": {
      "mimir": {
        "command": "/usr/local/bin/mimir",
        "args": ["--db", "~/.mimir/data/mimir.db"]
      }
    }
  }
}
```

## All 30 Tools

| Category | Tools |
|---|---|
| **CRUD** | `mimir_remember`, `mimir_recall`, `mimir_forget`, `mimir_get_entity`, `mimir_recall_when` |
| **Graph** | `mimir_link`, `mimir_unlink`, `mimir_traverse` |
| **Journal** | `mimir_journal`, `mimir_timeline` |
| **State** | `mimir_state_set`, `mimir_state_get`, `mimir_state_delete`, `mimir_state_list` |
| **AI** | `mimir_ask` (RAG), `mimir_embed` (embeddings), `mimir_cohere` (synthesis) |
| **Connectors** | `mimir_ingest` (GitHub issues, file watcher) |
| **Lifecycle** | `mimir_decay`, `mimir_prune`, `mimir_compact`, `mimir_score` |
| **Quality** | `mimir_conflicts` |
| **Vault** | `mimir_vault_export`, `mimir_vault_import` |
| **Ops** | `mimir_health`, `mimir_stats`, `mimir_migrate`, `mimir_context`, `mimir_workspace_list` |

## Encryption

Mimir supports AES-256-GCM encryption at rest for `body_json`. Opt-in:

```bash
# Generate key
mimir keygen --key-file ~/.mimir/secret.key

# Use with any client (add --encryption-key to args)
/usr/local/bin/mimir --db ~/.mimir/data/mimir.db --encryption-key ~/.mimir/secret.key
```

## Docker

```bash
docker run -v ~/.mimir/data:/data ghcr.io/Perseus-Computing-LLC/mneme:latest --db /data/mimir.db
```

## What Mimir Is Not

- ❌ Not a vector database — it's a persistent memory engine
- ❌ Not a cloud service — everything runs locally
- ❌ Not tied to any AI framework — works with any MCP client
- ❌ Not an embedding endpoint — uses Ollama for embeddings (optional)

## Design Philosophy

> Mimir is memory for machines. It remembers what your agents learn so they don't start cold every session. Everything is stored locally, searchable via FTS5 + hybrid search, and exportable as plain Markdown files. No API keys, no cloud dependencies, no vendor lock-in.

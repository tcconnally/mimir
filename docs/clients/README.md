# Mimir — MCP Client Setup

Mimir is a standard **MCP stdio server**, so it works with every MCP-compatible
client. The command is always the same:

```
mimir serve --db ~/.mimir/data/mimir.db
```

Run `mimir doctor` to validate your install and print this matrix locally.
Run `mimir connect --client <name>` to auto-wire a client's config file
(merges a `mimir` MCP stanza into it, backing up the original first — no
manual JSON/YAML/TOML editing required).

| Client | Status | Config file | Notes |
|---|---|---|---|
| Claude Desktop | ✅ Works | `claude_desktop_config.json` | Most common host |
| Claude Code / Hermes | ✅ Works | `.mcp.json` or `~/.hermes/config.yaml` | Verified |
| Cursor | ✅ Works | `.cursor/mcp.json` | |
| Windsurf | ✅ Works | `mcp_config.json` | |
| VS Code + Continue.dev | ✅ Works | `config.json` (`mcpServers`) | |
| Zed | ✅ Works | `settings.json` (`context_servers`) | |
| Codex CLI | ✅ Works | `~/.codex/config.toml` | |

---

## Copy-paste config

### Claude Desktop — `claude_desktop_config.json`
```json
{ "mcpServers": { "mimir": { "command": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } }
```

### Claude Code — `.mcp.json` (project root)
```json
{ "mcpServers": { "mimir": { "command": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } }
```

### Hermes — `~/.hermes/config.yaml`
```yaml
mcp_servers:
  mimir:
    command: mimir
    args: ["serve", "--db", "~/.mimir/data/mimir.db"]
```

### Cursor — `.cursor/mcp.json`
```json
{ "mcpServers": { "mimir": { "command": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } }
```

### Windsurf — `mcp_config.json`
```json
{ "mcpServers": { "mimir": { "command": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } }
```

### VS Code + Continue.dev — `config.json`
```json
{ "mcpServers": { "mimir": { "command": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } }
```

### Zed — `settings.json`
```json
{ "context_servers": { "mimir": { "command": { "path": "mimir", "args": ["serve", "--db", "~/.mimir/data/mimir.db"] } } } }
```

### Codex CLI — `~/.codex/config.toml`
```toml
[mcp_servers.mimir]
command = "mimir"
args = ["serve", "--db", "~/.mimir/data/mimir.db"]
```

> Use an absolute `--db` path if your client runs Mimir from a different working
> directory. Everything else is identical across clients because Mimir speaks
> plain MCP stdio.

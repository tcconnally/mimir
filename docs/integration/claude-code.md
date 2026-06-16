# Integrating Mimir with Claude Code

Claude Code is Anthropic's CLI coding agent. It supports custom MCP servers
via configuration, allowing Mimir to serve as persistent long-term memory
across coding sessions.

## Quick Start

### 1. Install Mimir

```bash
# One-shot bootstrap (recommended)
curl -sSL https://raw.githubusercontent.com/tcconnally/mimir/main/scripts/bootstrap.sh | bash

# Or via cargo
cargo install mimir
```

Verify:
```bash
mimir --version
# Expected: mimir 1.0.0
```

### 2. Create a data directory

```bash
mkdir -p ~/.mimir/data
```

### 3. Configure Claude Code

Claude Code reads MCP server config from `.mcp.json` in your project root,
or from `~/.claude.json` for global configuration.

**Project-level** (recommended — travels with the repo):

Create `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": ["--db", "/home/YOUR_USER/.mimir/data/mimir.db"]
    }
  }
}
```

Replace `/home/YOUR_USER/.mimir/data/mimir.db` with the absolute path to your
database. Do NOT use `~` — tilde expansion may not work in the MCP spawn context.

**Global** (applies to all projects):

Add the same `mcpServers` block to `~/.claude.json`.

### 4. Verify

Launch Claude Code in your project directory:

```bash
claude
```

Ask:

> List your available tools. Do you have access to Mimir tools?

You should see `mimir_remember`, `mimir_recall`, `mimir_context`, and other
Mimir tools in the tool list.

## Usage Patterns

### Persisting decisions across sessions

> I just decided to use SQLite for the caching layer instead of Redis.
> Remember this architectural decision.

Claude Code will call `mimir_remember` to store the entity.

### Resuming context from a previous session

> What architectural decisions did I make about caching in this project?

Claude Code will call `mimir_recall` to retrieve relevant entities.

### Getting a session summary

> Give me the recent memory context for this project.

Claude Code will call `mimir_context` which returns a pre-formatted markdown
block suitable for session injection.

### Recording journal events

> Log this as a decision: we're dropping PostgreSQL support in favor of SQLite.

Claude Code will call `mimir_journal` to append a structured event.

## Troubleshooting

### Mimir tools don't appear

1. **Absolute paths:** Ensure the `--db` argument uses a full absolute path.
   `/home/user/.mimir/data/mimir.db` not `~/.mimir/data/mimir.db`.

2. **Binary on PATH:** Run `which mimir`. If not found, install it or use
   the full path in the `command` field: `/usr/local/bin/mimir`.

3. **Database writable:** The directory containing `mimir.db` must be writable
   by the user running Claude Code.

4. **Restart Claude Code:** MCP servers are discovered at startup. After
   changing config, restart Claude Code with `/exit` and relaunch.

### Permission denied on database

```bash
chmod 755 ~/.mimir/data
chmod 644 ~/.mimir/data/mimir.db
```

### Mimir exits immediately

Run Mimir manually to check for startup errors:

```bash
mimir --db ~/.mimir/data/mimir.db
# Should hang waiting for stdin (this is correct — MCP stdio server)

# If it exits with an error, check:
# - SQLite is available (ldd $(which mimir) | grep sqlite)
# - Database file is not corrupted (mimir --db /tmp/test.db to try a fresh DB)
```

### Multiple Claude Code instances

SQLite WAL mode supports concurrent readers. If you see "database is locked",
another process has an exclusive lock. Kill orphaned Mimir processes:

```bash
ps aux | grep '[m]imir'
kill <PID>
```

## Advanced

### Using a project-specific database

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": ["--db", "/home/YOU/projects/my-project/.mimir/mimir.db"]
    }
  }
}
```

This keeps project memories isolated.

### Web dashboard

Mimir includes an optional web dashboard for browsing entities:

```bash
mimir --db ~/.mimir/data/mimir.db --web --port 8767
```

Open `http://localhost:8767` in a browser. The dashboard shows entity lists,
search, graph visualization, and journal events.

### Encryption at rest

Generate a key and use it:

```bash
mimir keygen --key-file ~/.mimir/secret.key
```

Then in `.mcp.json`:

```json
{
  "mcpServers": {
    "mimir": {
      "command": "mimir",
      "args": [
        "--db", "/home/YOU/.mimir/data/mimir.db",
        "--encryption-key", "/home/YOU/.mimir/secret.key"
      ]
    }
  }
}
```

The `body_json` column of entities is now AES-256-GCM encrypted. FTS5 indexes
remain plaintext for search.

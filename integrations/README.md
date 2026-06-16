# Mimir Integrations

Ready-to-use adapters that connect Mimir to popular AI agent frameworks.

## Available Integrations

| Framework | Type | Directory |
|---|---|---|
| **LangGraph** (LangChain) | `BaseStore` implementation | [`langgraph/`](langgraph/) |
| **Claude Code** (Anthropic) | MCP server config | [`../docs/integration/claude-code.md`](../docs/integration/claude-code.md) |
| **Cursor** | MCP server config | [`../docs/integration/cursor.md`](../docs/integration/cursor.md) |
| **CrewAI** | Agent Tool | [`crewai/`](crewai/) |

## Adding a New Integration

Each integration lives in its own directory with:

```
integrations/<framework>/
├── mimir_<framework>/
│   └── __init__.py     # Main adapter code
├── pyproject.toml       # Package metadata
└── README.md            # Usage guide
```

The adapter pattern:
1. **MCP subprocess call** — Uses Mimir's stdio MCP transport
2. **Framework interface mapping** — Maps the framework's memory API to Mimir tools
3. **Drop-in compatibility** — Works as a replacement for the framework's default memory

## Requirements

All integrations require Mimir v1.0.0+ installed:

```bash
curl -sSL https://raw.githubusercontent.com/tcconnally/mimir/main/scripts/bootstrap.sh | bash
```

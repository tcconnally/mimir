# Mimir LangGraph Integration

Drop-in persistent long-term memory for LangGraph agents via Mimir.

## Install

```bash
pip install langgraph
pip install mimir-langgraph
```

Or from source:

```bash
pip install -e integrations/langgraph/
```

## Quick Start

```python
from mimir_langgraph import MimirStore

# Create a Mimir-backed store
store = MimirStore(
    binary="mimir",  # or /usr/local/bin/mimir
    db_path="~/.mimir/data/mimir.db",
)

# Use as a drop-in BaseStore replacement
store.put(("users", "123"), "preferences", {"theme": "dark", "language": "en"})

item = store.get(("users", "123"), "preferences")
print(item.value)  # {"theme": "dark", "language": "en"}

# Search across namespaces
results = store.search(("users",), query="preferences theme")
for r in results:
    print(r.key, r.value, r.score)
```

## Integration with LangGraph Agents

```python
from langgraph.graph import StateGraph
from langgraph.store.base import BaseStore
from mimir_langgraph import MimirStore

# Use MimirStore as your long-term memory
store = MimirStore()

# Build your graph with store
graph = (
    StateGraph(AgentState)
    .add_node("agent", agent_node)
    .compile(store=store)
)
```

The store persists across sessions. Agents can retrieve context
from previous interactions using `store.search()`.

## Configuration

| Parameter | Default | Description |
|---|---|---|
| `binary` | `"mimir"` | Path to the mimir binary |
| `db_path` | `"~/.mimir/data/mimir.db"` | Path to the SQLite database |
| `timeout` | `30.0` | Tool call timeout in seconds |
| `encryption_key` | `None` | Path to AES-256-GCM key file |
| `ollama_url` | `None` | Ollama endpoint for hybrid search |
| `embedding_model` | `None` | Embedding model name (requires ollama_url) |

## How It Works

LangGraph's BaseStore interface maps cleanly onto Mimir's entity model:

| LangGraph | Mimir |
|---|---|
| `namespace: tuple[str, ...]` | `category: str` (joined with `/`) |
| `key: str` | `key: str` |
| `value: dict` | `body_json: str` (JSON) |
| `search()` | `mimir_recall` (FTS5) |
| `put()` | `mimir_remember` |
| `delete()` | `mimir_forget` |

## Requirements

- Mimir v1.0.0+ installed (`curl -sSL https://raw.githubusercontent.com/tcconnally/mimir/main/scripts/bootstrap.sh | bash`)
- LangGraph >= 0.2.0
- Python 3.10+

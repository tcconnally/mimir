# Mimir for AutoGen

Persistent long-term memory for [AutoGen](https://github.com/microsoft/autogen)
(AG2 / `autogen-core` v0.4+) agents, backed by [Mimir](https://github.com/Perseus-Computing-LLC/mneme).

`MimirMemory` implements the `autogen_core.memory.Memory` protocol, so it drops
straight into an `AssistantAgent(memory=[...])`. Stored knowledge is injected
into the model context before each inference, giving your agents memory that
survives across sessions, processes, and crews.

## Install

```bash
# Install Mimir (the binary)
curl -sSL https://raw.githubusercontent.com/Perseus-Computing-LLC/mneme/main/scripts/bootstrap.sh | bash

# Install the adapter
pip install -e integrations/autogen
```

## Usage

```python
import asyncio
from autogen_agentchat.agents import AssistantAgent
from autogen_ext.models.openai import OpenAIChatCompletionClient
from mimir_autogen import MimirMemory


async def main():
    memory = MimirMemory(db_path="~/.mimir/data/agent.db")

    # Seed a fact
    from autogen_core.memory import MemoryContent, MemoryMimeType
    await memory.add(MemoryContent(
        content="The user prefers TypeScript over JavaScript.",
        mime_type=MemoryMimeType.TEXT,
        metadata={"category": "preferences", "key": "language"},
    ))

    agent = AssistantAgent(
        name="assistant",
        model_client=OpenAIChatCompletionClient(model="gpt-4o"),
        memory=[memory],
    )

    result = await agent.run(task="What language should I use for this project?")
    print(result.messages[-1].content)

    await memory.close()


asyncio.run(main())
```

## How it maps to Mimir

| AutoGen `Memory` method | Mimir tool | Behavior |
|---|---|---|
| `add(MemoryContent)` | `mimir_remember` | Content → `body_json`; `metadata.category`/`metadata.key` route the entity |
| `query(text)` | `mimir_recall` | FTS5 keyword search → list of `MemoryContent` |
| `update_context(ctx)` | `mimir_context` | Prepends the rendered memory block as a `SystemMessage` |
| `clear()` | `mimir_prune` | Soft-deletes (archives) this memory's category |
| `close()` | — | Shuts down the persistent Mimir stdio process |

## Configuration

```python
MimirMemory(
    binary="mimir",                       # or absolute path: /usr/local/bin/mimir
    db_path="~/.mimir/data/mimir.db",
    category="autogen",                   # default category for add()
    context_limit=10,                     # entities injected by update_context()
    encryption_key="~/.mimir/secret.key", # optional AES-256-GCM at rest
    llm_endpoint="http://localhost:11434/api/generate",  # optional, for hybrid search
    llm_model="nomic-embed-text",         # optional embedding/RAG model
)
```

## Notes

- The adapter keeps a **persistent** Mimir stdio session — the process is
  spawned once and reused across all calls (no per-call cold start). Call
  `await memory.close()` when done, or let `__del__` reap it.
- `add()` accepts `metadata={"category": ..., "key": ...}` to control where the
  entity lands. Without an explicit key, a timestamped key is generated so
  repeated `add()` calls never collide.
- Use a **project-specific** `db_path` to isolate memories per agent or per
  workspace.

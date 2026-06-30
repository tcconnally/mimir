# Mimir CrewAI Integration

Persistent memory for CrewAI agents via Mimir.

## Install

Install from source (not yet published to PyPI):

```bash
pip install crewai
pip install -e integrations/crewai/
```

## Quick Start

```python
from crewai import Agent, Task, Crew
from mimir_crewai import MimirMemoryTool

# Create the memory tool
memory = MimirMemoryTool(
    db_path="~/.mimir/data/crew.db"
)

# Give it to your agents
researcher = Agent(
    role="Senior Researcher",
    goal="Find and analyze information",
    backstory="Expert at gathering and synthesizing data",
    tools=[memory],
    verbose=True,
)

# Agents use it naturally
task = Task(
    description=(
        "Research the competitor's pricing strategy. "
        "Use Mimir Memory to recall any previous findings on this topic, "
        "then remember your new conclusions."
    ),
    agent=researcher,
    expected_output="A report with pricing analysis",
)

crew = Crew(agents=[researcher], tasks=[task])
result = crew.kickoff()
```

## Available Actions

| Action | Description | Parameters |
|---|---|---|
| `remember` | Store a fact or decision | `category`, `key`, `content`, `entity_type?` |
| `recall` | Search stored memories | `query`, `category?`, `limit?` |
| `journal` | Record a significant event | `event_type`, `description`, `context?` |
| `context` | Get session context summary | (none) |

## How It Works

The `MimirMemoryTool` wraps Mimir's MCP tools as a CrewAI tool:

- `remember` → `mimir_remember`
- `recall` → `mimir_recall`
- `journal` → `mimir_journal`
- `context` → `mimir_context`

All memories persist across sessions and crews. Agents can build up
a shared knowledge base over time.

## Requirements

- Mimir v1.0.0+ (`curl -sSL https://raw.githubusercontent.com/Perseus-Computing-LLC/mneme/main/scripts/bootstrap.sh | bash`)
- CrewAI >= 0.30.0
- Python 3.10+

# Mimir Integration Examples

## Quickstart
`quickstart.py` — 60-second demo: remember, recall, forget, vault export.

## CrewAI Integration
`crewai_integration.py` — Use Mimir as persistent memory for CrewAI crews.
Stores conversation history and user preferences, recalls context across crew kickoffs.

## Google ADK Integration
`adk_integration.py` — Implements `BaseMemoryService` for Google Agent Development Kit.
Local-first, encrypted, zero cloud dependencies — complementing ADK's InMemory and Vertex AI backends.

## Running

```bash
pip install mimir crewai google-adk  # as needed
python examples/quickstart.py
```

"""
Mimir Quickstart — 60 seconds to persistent agent memory

pip install mimir
wget https://github.com/tcconnally/mimir/releases/latest/download/mimir-linux-x86_64
chmod +x mimir-linux-x86_64
"""
from mimir import MimirClient

# Connect (single binary, no setup)
client = MimirClient("./memory.db")

# Remember something
client.remember(
    content="The API key for production is sk-abc123. Rotate monthly.",
    category="credential",
    metadata={"env": "production", "service": "openai"},
)

# Recall it later — even in a different session
results = client.recall("API key production", limit=3)
for r in results:
    print(f"[{r.category}] score={r.score:.2f}: {r.content[:80]}...")

# Forget things that are no longer relevant
client.forget(entity_id=results[0].id)

# Export memories to share or back up
vault = client.vault_export()
print(f"Exported {len(vault)} memories to vault")

# Search with hybrid BM25 + embeddings
from mimir import search_memories
hits = search_memories("production credentials", db_path="./memory.db", limit=5)
for h in hits:
    print(f"  {h.entity_id}: {h.content[:60]}...")

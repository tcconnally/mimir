"""
Mimir + Google ADK Integration Example

Shows Mimir as a BaseMemoryService backend for Google ADK.
Requires: pip install google-adk mimir
"""
import asyncio
import json
from google.adk import Agent
from google.adk.memory.base_memory_service import BaseMemoryService, SearchMemoryResponse
from google.adk.memory.memory_entry import MemoryEntry
from google.adk.sessions.session import Session

class MimirMemoryService(BaseMemoryService):
    """Mimir-backed memory for Google ADK agents."""

    def __init__(self, db_path: str = "./adk_memory.db"):
        from mimir import MimirClient
        self.client = MimirClient(db_path)

    async def add_session_to_memory(self, session: Session) -> None:
        """Store session events as Mimir memories."""
        for event in session.events:
            self.client.remember(
                content=json.dumps({
                    "author": event.author,
                    "content": str(event.content) if event.content else "",
                }),
                category="adk-session",
                metadata={
                    "session_id": session.id,
                    "app_name": session.app_name,
                    "user_id": session.user_id,
                },
            )

    async def search_memory(
        self,
        app_name: str,
        user_id: str,
        query: str = "",
    ) -> SearchMemoryResponse:
        """Search memories and return matching entries."""
        results = self.client.recall(
            query=query,
            limit=10,
        )
        memories = [
            MemoryEntry(
                content=r.content,
                metadata=r.metadata or {},
                score=r.score or 0.0,
            )
            for r in results
        ]
        return SearchMemoryResponse(memories=memories)


async def main():
    # Create ADK agent with Mimir memory
    memory_service = MimirMemoryService("./adk_memory.db")

    agent = Agent(
        name="mimir_agent",
        model="gemini-2.5-flash",
        instruction="You have persistent memory across sessions.",
    )

    # In production, wire this via the ADK session service
    # session = await session_service.create_session(
    #     app_name="my-app",
    #     user_id="alice",
    # )
    # await memory_service.add_session_to_memory(session)

    print("MimirMemoryService ready for ADK integration")
    print("Memory backend: SQLite + FTS5 + encrypted at rest")


if __name__ == "__main__":
    asyncio.run(main())

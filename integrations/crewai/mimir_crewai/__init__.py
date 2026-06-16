"""
CrewAI Mimir Memory Tool — provides persistent memory for CrewAI agents.

Usage:
    from crewai import Agent, Task, Crew
    from mimir_crewai import MimirMemoryTool

    memory = MimirMemoryTool(db_path="~/.mimir/data/crew.db")

    agent = Agent(
        role="Researcher",
        goal="Find information",
        tools=[memory],
    )
"""

import json
import subprocess
import time
import select
from pathlib import Path
from typing import Optional, Any
from crewai.tools import BaseTool


class MimirMemoryTool(BaseTool):
    """CrewAI tool that provides persistent memory via Mimir.

    Agents can remember facts, recall past decisions, and search
    the knowledge base — all persisted across sessions and crews.

    Available actions:
        remember  — Store a fact or decision
        recall    — Search stored memories
        journal   — Record a significant event
        context   — Get the current session context summary
    """

    name: str = "Mimir Memory"
    description: str = (
        "Persistent memory tool for storing and retrieving information "
        "across sessions. Use this to remember facts, recall past "
        "decisions, and maintain context between agent interactions.\n"
        "Actions: remember(category, key, content), "
        "recall(query, category?), "
        "journal(event_type, description, context?), "
        "context() — get session summary"
    )

    def __init__(
        self,
        binary: str = "mimir",
        db_path: str = "~/.mimir/data/crew.db",
        timeout: float = 30.0,
        encryption_key: Optional[str] = None,
    ):
        super().__init__()
        self.binary = binary
        self.db_path = str(Path(db_path).expanduser())
        self.timeout = timeout
        self.encryption_key = encryption_key

    def _call_mimir(self, method: str, params: dict) -> dict:
        """Call a Mimir MCP tool via stdio."""
        args = [self.binary, "--db", self.db_path]
        if self.encryption_key:
            args.extend(["--encryption-key", self.encryption_key])

        proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        try:
            init_req = json.dumps({
                "jsonrpc": "2.0", "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "crewai-mimir", "version": "1.0.0"},
                },
            }) + "\n"
            proc.stdin.write(init_req)
            proc.stdin.flush()
            ready, _, _ = select.select([proc.stdout], [], [], 10.0)
            if ready:
                proc.stdout.readline()
            time.sleep(0.05)

            call_req = json.dumps({
                "jsonrpc": "2.0", "id": 2,
                "method": "tools/call",
                "params": {"name": method, "arguments": params},
            }) + "\n"
            proc.stdin.write(call_req)
            proc.stdin.flush()

            ready, _, _ = select.select([proc.stdout], [], [], self.timeout)
            if not ready:
                return {"error": "timeout"}

            response = json.loads(proc.stdout.readline())
            if "error" in response:
                return {"error": response["error"]}
            return response.get("result", {})

        finally:
            try:
                proc.stdin.close()
            except Exception:
                pass
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    def _run(self, action: str, **kwargs) -> str:
        """Execute a memory action.

        Args:
            action: One of 'remember', 'recall', 'journal', 'context'
            **kwargs: Action-specific parameters
        """
        if action == "remember":
            return self._remember(**kwargs)
        elif action == "recall":
            return self._recall(**kwargs)
        elif action == "journal":
            return self._journal(**kwargs)
        elif action == "context":
            return self._context()
        else:
            return f"Unknown action: {action}. Use: remember, recall, journal, context"

    def _remember(
        self,
        category: str = "crewai",
        key: str = "",
        content: str = "",
        entity_type: str = "fact",
    ) -> str:
        """Store a fact, decision, or piece of knowledge."""
        result = self._call_mimir("mimir_remember", {
            "category": category,
            "key": key or f"auto-{int(time.time())}",
            "body_json": json.dumps({"content": content}),
            "entity_type": entity_type,
        })
        if "error" in result:
            return f"Failed to remember: {result['error']}"
        return f"Remembered: [{category}] {key or 'auto'}: {content[:100]}"

    def _recall(
        self,
        query: str = "",
        category: str = "",
        limit: int = 5,
    ) -> str:
        """Search stored memories."""
        params = {"query": query, "limit": limit}
        if category:
            params["category"] = category

        result = self._call_mimir("mimir_recall", params)
        items = result.get("items", [])

        if not items:
            return f"No memories found for '{query}'"

        lines = [f"Found {len(items)} memor{'y' if len(items)==1 else 'ies'}:"]
        for item in items:
            body = item.get("body_json", "{}")
            try:
                content = json.loads(body).get("content", body)
            except (json.JSONDecodeError, TypeError):
                content = body
            lines.append(f"  [{item.get('category', '?')}] {item.get('key', '?')}: {content[:200]}")
        return "\n".join(lines)

    def _journal(
        self,
        event_type: str = "observation",
        description: str = "",
        context: str = "",
    ) -> str:
        """Record a significant event in the journal."""
        result = self._call_mimir("mimir_journal", {
            "event_type": event_type,
            "category": "crewai",
            "key": f"event-{int(time.time())}",
            "evaluated": {"description": description, "context": context},
        })
        if "error" in result:
            return f"Failed to journal: {result['error']}"
        return f"Journaled {event_type}: {description[:100]}"

    def _context(self) -> str:
        """Get a summary of recent memories for session context."""
        result = self._call_mimir("mimir_context", {})
        if "error" in result:
            return f"Failed to get context: {result['error']}"
        context_text = result.get("context", "")
        if not context_text:
            return "No stored context. Use 'remember' to store information first."
        # Return first 1000 chars to avoid overwhelming the agent
        return context_text[:1000] + ("..." if len(context_text) > 1000 else "")

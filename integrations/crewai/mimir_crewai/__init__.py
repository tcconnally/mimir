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
import threading
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

    The tool keeps a persistent Mimir stdio session — the process is
    spawned once and reused across all calls.  This avoids the per-call
    cold-start overhead (process spawn + DB open + init handshake).
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

        # Persistent session — spawned lazily on first call
        self._proc: Optional[subprocess.Popen] = None
        self._req_id: int = 0
        self._lock = threading.Lock()

    # ── session management ──────────────────────────────────────────

    def _ensure_session(self):
        """Spawn a persistent mimir process if one isn't already running."""
        if self._proc is not None and self._proc.poll() is None:
            return  # already alive

        args = [self.binary, "--db", self.db_path]
        if self.encryption_key:
            args.extend(["--encryption-key", self.encryption_key])

        self._proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self._req_id = 0

        # Send initialize request
        init_req = json.dumps({
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "crewai-mimir", "version": "1.0.0"},
            },
        })
        try:
            self._proc.stdin.write(init_req + "\n")
            self._proc.stdin.flush()
        except (BrokenPipeError, OSError):
            self._proc = None
            raise RuntimeError("Failed to initialize mimir process")

        # Read the initialize response (ignore — just consume it)
        self._read_response(self._req_id - 1)

    def _next_id(self) -> int:
        self._req_id += 1
        return self._req_id

    def _read_response(self, expect_id: int) -> Optional[dict]:
        """Read newline-delimited JSON from stdout until we find a response
        whose ``id`` matches *expect_id*.  Returns the parsed message or
        ``None`` if the process died or timed out."""
        assert self._proc is not None

        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            # Check if process died
            if self._proc.poll() is not None:
                return None

            line = self._proc.stdout.readline()
            if not line:
                time.sleep(0.01)
                continue

            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(msg, dict) and msg.get("id") == expect_id:
                return msg
        return None

    def _close_session(self):
        """Shut down the persistent mimir process."""
        if self._proc is None:
            return
        try:
            self._proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        try:
            self._proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self._proc.kill()
            self._proc.wait()
        self._proc = None

    def __del__(self):
        self._close_session()

    # ── MCP call ────────────────────────────────────────────────────

    @staticmethod
    def _unwrap_result(result: dict) -> dict:
        """Unwrap an MCP ``tools/call`` result into Mimir's payload dict.

        Mimir returns the standard MCP envelope::

            {"content": [{"type": "text", "text": "<json>"}],
             "structuredContent": {...parsed json...}}

        The payload (``items``, ``context`` ...) lives in ``structuredContent``
        (preferred) or the JSON text of the first content block. Reading
        ``result["items"]`` directly always yields nothing.
        """
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        content = result.get("content")
        if isinstance(content, list) and content:
            text = content[0].get("text", "") if isinstance(content[0], dict) else ""
            try:
                parsed = json.loads(text)
            except (json.JSONDecodeError, TypeError):
                return {}
            if isinstance(parsed, dict):
                return parsed
        return {}

    def _call_mimir(self, method: str, params: dict) -> dict:
        """Call a Mimir MCP tool via the persistent stdio session."""
        with self._lock:
            try:
                self._ensure_session()
            except RuntimeError as e:
                return {"error": str(e)}

            req_id = self._next_id()
            call_req = json.dumps({
                "jsonrpc": "2.0",
                "id": req_id,
                "method": "tools/call",
                "params": {"name": method, "arguments": params},
            })

            try:
                self._proc.stdin.write(call_req + "\n")
                self._proc.stdin.flush()
            except (BrokenPipeError, OSError):
                self._proc = None
                return {"error": "mimir process died — call re-spawns"}

            response = self._read_response(req_id)
            if response is None:
                # Process likely died — reset so next call re-spawns
                self._close_session()
                return {"error": "no response from mimir (process may have exited)"}
            if response.get("error"):
                return {"error": response["error"]}
            return self._unwrap_result(response.get("result", {}))

    # ── CrewAI tool interface ───────────────────────────────────────

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
            "type": entity_type,
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
        return context_text[:1000] + ("..." if len(context_text) > 1000 else "")

"""
MimirMemory — AutoGen ``Memory`` implementation backed by Mimir.

Drop-in persistent long-term memory for AutoGen (AG2 / autogen-core v0.4+)
agents. Implements the ``autogen_core.memory.Memory`` protocol so a Mimir
database can be attached to any ``AssistantAgent`` and its stored knowledge is
injected into the model context before each inference.

Usage:
    from autogen_agentchat.agents import AssistantAgent
    from autogen_ext.models.openai import OpenAIChatCompletionClient
    from mimir_autogen import MimirMemory

    memory = MimirMemory(db_path="~/.mimir/data/agent.db")

    agent = AssistantAgent(
        name="assistant",
        model_client=OpenAIChatCompletionClient(model="gpt-4o"),
        memory=[memory],
    )

The adapter maps AutoGen's ``MemoryContent`` model onto Mimir's entity model:

    MemoryContent.content   → Mimir body_json {"content": ...}
    MemoryContent.metadata  → merged into body_json (category/key extracted)
    query(text)             → Mimir FTS5 recall
    update_context()        → prepends a Mimir context block as a SystemMessage

It keeps a persistent Mimir stdio session — the process is spawned once and
reused across all calls, avoiding per-call cold-start overhead (process spawn +
DB open + init handshake).
"""

from __future__ import annotations

import json
import subprocess
import time
import threading
import logging
from pathlib import Path
from typing import Any, Optional

from autogen_core import CancellationToken
from autogen_core.memory import (
    Memory,
    MemoryContent,
    MemoryMimeType,
    MemoryQueryResult,
    UpdateContextResult,
)
from autogen_core.model_context import ChatCompletionContext
from autogen_core.models import SystemMessage

logger = logging.getLogger(__name__)


class MimirMemory(Memory):
    """AutoGen ``Memory`` backed by a local Mimir MCP server.

    Mimir is a local-first persistent memory engine with structured entities,
    journal events, and state management. This adapter implements the four
    ``Memory`` protocol methods (``add``, ``query``, ``update_context``,
    ``clear``) plus ``close`` so it can be passed directly to an
    ``AssistantAgent(memory=[...])``.
    """

    def __init__(
        self,
        binary: str = "mimir",
        db_path: str = "~/.mimir/data/mimir.db",
        timeout: float = 30.0,
        category: str = "autogen",
        context_limit: int = 10,
        encryption_key: Optional[str] = None,
        llm_endpoint: Optional[str] = None,
        llm_model: Optional[str] = None,
    ):
        """Initialize the Mimir-backed memory.

        Args:
            binary: Path to the mimir binary (default: finds on PATH)
            db_path: Path to the Mimir SQLite database
            timeout: Command timeout in seconds
            category: Default Mimir category for stored memories
            context_limit: Max entities to inject in ``update_context``
            encryption_key: Optional path to AES-256-GCM key file
            llm_endpoint: Optional LLM endpoint (e.g. Ollama
                ``http://localhost:11434/api/generate``) for hybrid search
            llm_model: Optional model name used for embeddings / ``mimir_ask``
        """
        self.binary = binary
        self.db_path = str(Path(db_path).expanduser())
        self.timeout = timeout
        self.category = category
        self.context_limit = context_limit
        self.encryption_key = encryption_key
        self.llm_endpoint = llm_endpoint
        self.llm_model = llm_model

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
        if self.llm_endpoint:
            args.extend(["--llm-endpoint", self.llm_endpoint])
        if self.llm_model:
            args.extend(["--llm-model", self.llm_model])

        self._proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self._req_id = 0

        init_id = self._next_id()
        init_req = json.dumps({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "autogen-mimir", "version": "1.0.0"},
            },
        })
        try:
            self._proc.stdin.write(init_req + "\n")
            self._proc.stdin.flush()
        except (BrokenPipeError, OSError):
            self._proc = None
            raise RuntimeError("Failed to initialize mimir process")

        # Read the initialize response (ignore — just consume it)
        self._read_response(init_id)

    def _next_id(self) -> int:
        self._req_id += 1
        return self._req_id

    def _read_response(self, expect_id: int) -> Optional[dict]:
        """Read newline-delimited JSON from stdout until we find a response
        whose ``id`` matches *expect_id*. Returns the parsed message or
        ``None`` if the process died or timed out."""
        assert self._proc is not None

        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
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
                self._close_session()
                return {"error": "no response from mimir (process may have exited)"}
            if response.get("error"):
                return {"error": response["error"]}
            return self._unwrap_result(response.get("result", {}))

    # ── AutoGen Memory protocol ─────────────────────────────────────

    async def add(
        self,
        content: MemoryContent,
        cancellation_token: CancellationToken | None = None,
    ) -> None:
        """Store a ``MemoryContent`` entry in Mimir.

        Maps to ``mimir_remember``. The content text becomes ``body_json``;
        ``metadata`` may carry an explicit ``category``/``key``, otherwise an
        auto key is generated.
        """
        metadata = content.metadata or {}
        category = metadata.get("category", self.category)
        key = metadata.get("key") or f"autogen-{int(time.time() * 1000)}"

        text = self._content_to_text(content)
        body = {"content": text, "mime_type": str(content.mime_type)}
        # Preserve any extra metadata keys (besides routing ones) in the body.
        for k, v in metadata.items():
            if k not in ("category", "key"):
                body[k] = v

        result = self._call_mimir("mimir_remember", {
            "category": category,
            "key": key,
            "body_json": json.dumps(body),
            "type": metadata.get("type", "autogen_memory"),
        })
        if "error" in result:
            logger.warning("MimirMemory.add failed: %s", result["error"])

    async def query(
        self,
        query: str | MemoryContent,
        cancellation_token: CancellationToken | None = None,
        **kwargs: Any,
    ) -> MemoryQueryResult:
        """Search stored memories via Mimir FTS5 recall.

        Returns a ``MemoryQueryResult`` whose ``results`` is a list of
        ``MemoryContent`` reconstructed from Mimir entities.
        """
        query_text = query if isinstance(query, str) else self._content_to_text(query)
        limit = int(kwargs.get("limit", self.context_limit))

        params: dict[str, Any] = {"query": query_text, "limit": limit}
        category = kwargs.get("category")
        if category:
            params["category"] = category

        result = self._call_mimir("mimir_recall", params)
        items = result.get("items", []) if "error" not in result else []

        results: list[MemoryContent] = []
        for item in items:
            body = item.get("body_json", "{}")
            try:
                parsed = json.loads(body)
                text = parsed.get("content", body)
            except (json.JSONDecodeError, TypeError):
                text = body
            results.append(MemoryContent(
                content=text,
                mime_type=MemoryMimeType.TEXT,
                metadata={
                    "category": item.get("category", ""),
                    "key": item.get("key", ""),
                    "score": item.get("decay_score"),
                },
            ))

        return MemoryQueryResult(results=results)

    async def update_context(
        self,
        model_context: ChatCompletionContext,
    ) -> UpdateContextResult:
        """Inject Mimir's context block into the model context.

        Calls ``mimir_context`` and prepends the rendered markdown block as a
        ``SystemMessage`` so the agent starts each turn with its persistent
        memory loaded. Returns the memories used for transparency/telemetry.
        """
        result = self._call_mimir("mimir_context", {"limit": self.context_limit})
        if "error" in result:
            logger.warning("MimirMemory.update_context failed: %s", result["error"])
            return UpdateContextResult(memories=MemoryQueryResult(results=[]))

        context_text = result.get("context", "")
        if not context_text:
            return UpdateContextResult(memories=MemoryQueryResult(results=[]))

        await model_context.add_message(
            SystemMessage(content=f"Relevant memory context from Mimir:\n{context_text}")
        )

        memory = MemoryContent(content=context_text, mime_type=MemoryMimeType.TEXT)
        return UpdateContextResult(memories=MemoryQueryResult(results=[memory]))

    async def clear(self) -> None:
        """Clear stored memories for this memory's category.

        Maps to ``mimir_prune`` scoped to the configured category. This is a
        soft-delete (archived=1) — entities are recoverable, not destroyed.
        """
        result = self._call_mimir("mimir_prune", {"category": self.category})
        if "error" in result:
            logger.warning("MimirMemory.clear failed: %s", result["error"])

    async def close(self) -> None:
        """Shut down the persistent Mimir process."""
        self._close_session()

    # ── helpers ─────────────────────────────────────────────────────

    @staticmethod
    def _content_to_text(content: MemoryContent) -> str:
        """Coerce a ``MemoryContent.content`` (str | bytes | dict | ...) to text."""
        c = content.content
        if isinstance(c, str):
            return c
        if isinstance(c, bytes):
            try:
                return c.decode("utf-8", errors="replace")
            except Exception:
                return str(c)
        try:
            return json.dumps(c)
        except (TypeError, ValueError):
            return str(c)


# Convenience helper
def create_mimir_memory(
    db_path: str = "~/.mimir/data/mimir.db",
    **kwargs,
) -> MimirMemory:
    """Create a MimirMemory with sensible defaults.

    Args:
        db_path: Path to the Mimir database
        **kwargs: Additional MimirMemory arguments
    """
    return MimirMemory(db_path=db_path, **kwargs)

"""
MimirStore — LangGraph BaseStore implementation backed by Mimir.

Drop-in persistent long-term memory for LangGraph agents.
Maps LangGraph's namespace/key/value model to Mimir's entity model.

Usage:
    from mimir_langgraph import MimirStore
    from langgraph.store.memory import InMemoryStore

    store = MimirStore()  # connects to local Mimir via MCP stdio
    # Or with explicit config:
    store = MimirStore(
        binary="/usr/local/bin/mimir",
        db_path="~/.mimir/data/mimir.db"
    )

    # Use as any BaseStore
    store.put(("users", "123"), "prefs", {"theme": "dark"})
    item = store.get(("users", "123"), "prefs")
    results = store.search(("users",), query="preferences")
"""

from __future__ import annotations

import json
import subprocess
import time
import logging
from collections.abc import Iterable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal, Optional

from langgraph.store.base import BaseStore, Item, SearchItem, Op, Result

# The "no TTL given" sentinel was renamed NOT_GIVEN -> NOT_PROVIDED in
# LangGraph 1.0. Support both so the adapter imports across versions.
try:
    from langgraph.store.base import NOT_PROVIDED as _NOT_GIVEN
except ImportError:  # langgraph < 1.0
    from langgraph.store.base import NOT_GIVEN as _NOT_GIVEN

logger = logging.getLogger(__name__)


class MimirStore(BaseStore):
    """LangGraph BaseStore backed by a local Mimir MCP server.

    Mimir is a local-first persistent memory engine with structured entities,
    journal events, and state management. This adapter maps LangGraph's
    namespace/key/value model onto Mimir's entity model.

    Mapping:
        namespace tuple  → Mimir category (joined with '/')
        key              → Mimir key
        value dict       → Mimir body_json
        search query     → Mimir FTS5 recall
    """

    def __init__(
        self,
        binary: str = "mimir",
        db_path: str = "~/.mimir/data/mimir.db",
        timeout: float = 30.0,
        connect_timeout: float = 10.0,
        encryption_key: Optional[str] = None,
        ollama_url: Optional[str] = None,
        embedding_model: Optional[str] = None,
    ):
        """Initialize the Mimir-backed store.

        Args:
            binary: Path to the mimir binary (default: finds on PATH)
            db_path: Path to the Mimir SQLite database
            timeout: Command timeout in seconds
            connect_timeout: MCP handshake timeout in seconds
            encryption_key: Optional path to AES-256-GCM key file
            ollama_url: Optional Ollama endpoint for hybrid search
            embedding_model: Optional embedding model name (requires ollama_url)
        """
        self.binary = binary
        self.db_path = str(Path(db_path).expanduser())
        self.timeout = timeout
        self.connect_timeout = connect_timeout
        self.encryption_key = encryption_key
        self.ollama_url = ollama_url
        self.embedding_model = embedding_model
        self._proc: Optional[subprocess.Popen] = None

    def _namespace_to_category(self, namespace: tuple[str, ...]) -> str:
        """Convert LangGraph namespace tuple to Mimir category string."""
        return "/".join(namespace) if namespace else "default"

    def _category_to_namespace(self, category: str) -> tuple[str, ...]:
        """Convert Mimir category string back to namespace tuple."""
        return tuple(category.split("/")) if category != "default" else ()

    @staticmethod
    def _unwrap_result(result: dict) -> dict:
        """Unwrap an MCP ``tools/call`` result into Mimir's payload dict.

        Mimir returns the standard MCP envelope::

            {"content": [{"type": "text", "text": "<json>"}],
             "structuredContent": {...parsed json...}}

        The actual payload (``items``, ``by_category``, ``context`` ...) lives
        in ``structuredContent`` (preferred) or, failing that, in the JSON text
        of the first content block. Reading ``result["items"]`` directly always
        yields nothing, so callers must go through this helper.
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
        """Call a Mimir MCP tool via a stdio subprocess.

        Sends ``initialize`` and ``tools/call`` in one write and reads all
        output via :meth:`subprocess.Popen.communicate`, which is portable
        across platforms (the previous ``select``-based reader did not work on
        Windows, where ``select`` cannot poll pipes). Returns the unwrapped
        Mimir payload dict.
        """
        args = [self.binary, "--db", self.db_path]
        if self.encryption_key:
            args.extend(["--encryption-key", self.encryption_key])
        if self.ollama_url:
            args.extend(["--ollama-url", self.ollama_url])
        if self.embedding_model:
            args.extend(["--embedding-model", self.embedding_model])

        init_req = json.dumps({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "langgraph-mimir", "version": "1.0.0"},
            },
        })
        call_req = json.dumps({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": method, "arguments": params},
        })

        proc = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            stdout, _ = proc.communicate(
                input=init_req + "\n" + call_req + "\n", timeout=self.timeout
            )
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate()
            raise TimeoutError(f"Mimir {method} timed out after {self.timeout}s")

        # Find the JSON-RPC response to our tools/call (id == 2); other lines
        # are the initialize response and any log noise.
        response: dict | None = None
        for line in stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(msg, dict) and msg.get("id") == 2:
                response = msg
                break

        if response is None:
            raise RuntimeError(f"No response from Mimir for {method}")
        if response.get("error"):
            raise RuntimeError(f"Mimir error: {response['error']}")
        return self._unwrap_result(response.get("result", {}))

    @staticmethod
    def _ms_to_dt(ms: Any) -> datetime:
        """Convert a Mimir ``*_unix_ms`` timestamp to a UTC ``datetime``.

        ``Item.created_at`` / ``updated_at`` are typed ``datetime``; the epoch
        is used as a fallback when a record carries no usable timestamp.
        """
        epoch = datetime.fromtimestamp(0, tz=timezone.utc)
        if not ms:
            return epoch
        try:
            return datetime.fromtimestamp(int(ms) / 1000, tz=timezone.utc)
        except (ValueError, TypeError, OSError):
            return epoch

    def put(
        self,
        namespace: tuple[str, ...],
        key: str,
        value: dict[str, Any],
        index: list[str] | Literal[False] | None = None,  # type: ignore[name-defined]
        *,
        ttl: float | None | Any = _NOT_GIVEN,
    ) -> None:
        """Store a value in Mimir.

        Maps to mimir_remember with category=namespace, key=key.
        The value dict becomes body_json. TTL is stored as a state entry.
        """
        category = self._namespace_to_category(namespace)

        result = self._call_mimir("mimir_remember", {
            "category": category,
            "key": key,
            "body_json": json.dumps(value),
            "type": "langgraph_item",
        })

        # Handle TTL via Mimir state
        if ttl is not _NOT_GIVEN and ttl is not None:
            self._call_mimir("mimir_state_set", {
                "key": f"{category}/{key}__ttl",
                "value": str(time.time() + float(ttl)),
                "ttl": float(ttl),
            })

    async def aput(self, *args, **kwargs) -> None:
        """Async variant — delegates to sync put."""
        self.put(*args, **kwargs)

    def get(
        self,
        namespace: tuple[str, ...],
        key: str,
        *,
        refresh_ttl: bool | None = None,
    ) -> Item | None:
        """Retrieve a value from Mimir.

        Maps to mimir_recall filtered by category + key.
        """
        category = self._namespace_to_category(namespace)

        result = self._call_mimir("mimir_recall", {
            "query": key,
            "category": category,
            "limit": 5,
        })

        items = result.get("items", [])
        for item in items:
            if item.get("key") == key:
                try:
                    value = json.loads(item.get("body_json", "{}"))
                except (json.JSONDecodeError, TypeError):
                    value = {}

                return Item(
                    namespace=namespace,
                    key=key,
                    value=value,
                    created_at=self._ms_to_dt(item.get("created_at_unix_ms")),
                    updated_at=self._ms_to_dt(
                        item.get("last_accessed_unix_ms")
                        or item.get("created_at_unix_ms")
                    ),
                )

        return None

    async def aget(self, *args, **kwargs) -> Item | None:
        """Async variant — delegates to sync get."""
        return self.get(*args, **kwargs)

    def search(
        self,
        namespace_prefix: tuple[str, ...],
        /,
        *,
        query: str | None = None,
        filter: dict[str, Any] | None = None,
        limit: int = 10,
        offset: int = 0,
        refresh_ttl: bool | None = None,
    ) -> list[SearchItem]:
        """Search for items in Mimir.

        Uses Mimir's FTS5 keyword search. The namespace_prefix becomes
        a category filter.
        """
        category = self._namespace_to_category(namespace_prefix)
        search_query = query or ""

        params = {
            "query": search_query,
            "limit": limit,
            "offset": offset,
        }
        if category and category != "default":
            params["category"] = category

        result = self._call_mimir("mimir_recall", params)
        items = result.get("items", [])

        results = []
        for item in items:
            try:
                value = json.loads(item.get("body_json", "{}"))
            except (json.JSONDecodeError, TypeError):
                value = {}

            results.append(SearchItem(
                namespace=namespace_prefix,
                key=item.get("key", ""),
                value=value,
                created_at=self._ms_to_dt(item.get("created_at_unix_ms")),
                updated_at=self._ms_to_dt(
                    item.get("last_accessed_unix_ms")
                    or item.get("created_at_unix_ms")
                ),
                score=item.get("decay_score"),
            ))

        return results

    async def asearch(self, *args, **kwargs) -> list[SearchItem]:
        """Async variant — delegates to sync search."""
        return self.search(*args, **kwargs)

    def delete(self, namespace: tuple[str, ...], key: str) -> None:
        """Delete an item from Mimir.

        Maps to mimir_forget (soft-delete with archived=1).
        """
        category = self._namespace_to_category(namespace)
        self._call_mimir("mimir_forget", {
            "category": category,
            "key": key,
            "reason": "LangGraph delete",
        })

    async def adelete(self, *args, **kwargs) -> None:
        """Async variant — delegates to sync delete."""
        self.delete(*args, **kwargs)

    def list_namespaces(
        self,
        *,
        prefix: Any | None = None,
        suffix: Any | None = None,
        max_depth: int | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[tuple[str, ...]]:
        """List all namespaces (categories) in Mimir."""
        result = self._call_mimir("mimir_stats", {})
        # mimir_stats returns category counts under "by_category" (a mapping of
        # category name -> count), not a "categories" list.
        by_category = result.get("by_category", {})

        namespaces = []
        for cat in by_category:
            ns = self._category_to_namespace(cat)
            namespaces.append(ns)

        return namespaces[offset:offset + limit]

    async def alist_namespaces(self, *args, **kwargs) -> list[tuple[str, ...]]:
        """Async variant — delegates to sync list_namespaces."""
        return self.list_namespaces(*args, **kwargs)

    def batch(self, ops: Iterable[Op]) -> list[Result]:  # type: ignore[name-defined]
        """Execute a batch of operations."""
        results = []
        for op in ops:
            try:
                if op[0] == "put":
                    self.put(*op[1], **op[2] if len(op) > 2 else {})
                    results.append(None)
                elif op[0] == "delete":
                    self.delete(*op[1])
                    results.append(None)
                elif op[0] == "get":
                    results.append(self.get(*op[1], **op[2] if len(op) > 2 else {}))
                elif op[0] == "search":
                    results.append(self.search(*op[1], **op[2] if len(op) > 2 else {}))
                else:
                    results.append(None)
            except Exception as e:
                logger.error(f"Batch op {op[0]} failed: {e}")
                results.append(None)
        return results

    async def abatch(self, ops: Iterable[Op]) -> list[Result]:  # type: ignore[name-defined]
        """Async variant — delegates to sync batch."""
        return self.batch(ops)


# Convenience helper
def create_mimir_store(
    db_path: str = "~/.mimir/data/mimir.db",
    **kwargs,
) -> MimirStore:
    """Create a MimirStore with sensible defaults.

    Args:
        db_path: Path to the Mimir database
        **kwargs: Additional MimirStore arguments
    """
    return MimirStore(db_path=db_path, **kwargs)

"""Tests for the AutoGen MimirMemory adapter.

autogen-core (and its dependency tree) need not be installed to exercise the
Mimir wiring: we stub the ``autogen_core`` modules the adapter imports with
minimal stand-ins, then drive the memory against Mimir's real MCP JSON-RPC
envelope via a fake persistent-stdio subprocess.
"""

from __future__ import annotations

import asyncio
import json
import sys
import types

import pytest


# ── stub autogen_core before importing the adapter ──────────────────

@pytest.fixture(scope="module")
def MimirMemory():
    if "autogen_core" not in sys.modules:
        core = types.ModuleType("autogen_core")
        memory_mod = types.ModuleType("autogen_core.memory")
        model_ctx_mod = types.ModuleType("autogen_core.model_context")
        models_mod = types.ModuleType("autogen_core.models")

        class CancellationToken:  # noqa: D401
            pass

        class MemoryMimeType:
            TEXT = "text/plain"

            def __str__(self):
                return "text/plain"

        class MemoryContent:
            def __init__(self, content=None, mime_type=None, metadata=None):
                self.content = content
                self.mime_type = mime_type
                self.metadata = metadata or {}

        class MemoryQueryResult:
            def __init__(self, results=None):
                self.results = results or []

        class UpdateContextResult:
            def __init__(self, memories=None):
                self.memories = memories

        class Memory:
            pass

        class ChatCompletionContext:
            def __init__(self):
                self.messages = []

            async def add_message(self, message):
                self.messages.append(message)

        class SystemMessage:
            def __init__(self, content=""):
                self.content = content

        core.CancellationToken = CancellationToken
        memory_mod.Memory = Memory
        memory_mod.MemoryContent = MemoryContent
        memory_mod.MemoryMimeType = MemoryMimeType
        memory_mod.MemoryQueryResult = MemoryQueryResult
        memory_mod.UpdateContextResult = UpdateContextResult
        model_ctx_mod.ChatCompletionContext = ChatCompletionContext
        models_mod.SystemMessage = SystemMessage

        sys.modules["autogen_core"] = core
        sys.modules["autogen_core.memory"] = memory_mod
        sys.modules["autogen_core.model_context"] = model_ctx_mod
        sys.modules["autogen_core.models"] = models_mod

    from mimir_autogen import MimirMemory as cls
    return cls


# ── fake persistent-stdio Popen ─────────────────────────────────────

def _fake_popen(routes):
    """Build a FakePopen whose stdout replays JSON-RPC responses.

    ``routes`` maps a tool name → callable(args) -> payload dict. The fake
    answers the initialize handshake with ``{}`` and each ``tools/call`` with
    the MCP envelope wrapping the routed payload.
    """
    class FakeStdout:
        def __init__(self):
            self._lines = []

        def push(self, line):
            self._lines.append(line)

        def readline(self):
            if self._lines:
                return self._lines.pop(0)
            return ""

    class FakePopen:
        def __init__(self, *args, **kwargs):
            self._stdout = FakeStdout()
            self.stderr = None

            class _Stdin:
                def __init__(self, outer):
                    self.outer = outer

                def write(self, data):
                    for line in data.splitlines():
                        line = line.strip()
                        if not line:
                            continue
                        msg = json.loads(line)
                        mid = msg.get("id")
                        if msg.get("method") == "initialize":
                            self.outer._stdout.push(
                                json.dumps({"jsonrpc": "2.0", "id": mid, "result": {}}) + "\n"
                            )
                        elif msg.get("method") == "tools/call":
                            name = msg["params"]["name"]
                            payload = routes[name](msg["params"]["arguments"])
                            env = {
                                "jsonrpc": "2.0",
                                "id": mid,
                                "result": {
                                    "content": [{"type": "text", "text": json.dumps(payload)}],
                                    "structuredContent": payload,
                                },
                            }
                            self.outer._stdout.push(json.dumps(env) + "\n")

                def flush(self):
                    pass

                def close(self):
                    pass

            self.stdin = _Stdin(self)

        @property
        def stdout(self):
            return self._stdout

        def poll(self):
            return None

        def wait(self, timeout=None):
            return 0

        def kill(self):
            pass

    return FakePopen


def _run(coro):
    return asyncio.run(coro)


# ── tests ───────────────────────────────────────────────────────────

def test_add_sends_remember_with_routing(monkeypatch, MimirMemory):
    from autogen_core.memory import MemoryContent, MemoryMimeType

    captured = {}

    def remember(args):
        captured.update(args)
        return {"id": "mem-1", "status": "ok"}

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_remember": remember})
    )
    mem = MimirMemory()
    content = MemoryContent(
        content="user prefers dark mode",
        mime_type=MemoryMimeType.TEXT,
        metadata={"category": "prefs", "key": "theme"},
    )
    _run(mem.add(content))

    assert captured["category"] == "prefs"
    assert captured["key"] == "theme"
    assert json.loads(captured["body_json"])["content"] == "user prefers dark mode"
    assert captured["type"] == "autogen_memory"


def test_add_auto_key_when_missing(monkeypatch, MimirMemory):
    from autogen_core.memory import MemoryContent, MemoryMimeType

    captured = {}

    def remember(args):
        captured.update(args)
        return {"status": "ok"}

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_remember": remember})
    )
    mem = MimirMemory(category="autogen")
    _run(mem.add(MemoryContent(content="x", mime_type=MemoryMimeType.TEXT)))

    assert captured["category"] == "autogen"
    assert captured["key"].startswith("autogen-")


def test_query_parses_structured_items(monkeypatch, MimirMemory):
    def recall(args):
        return {
            "items": [
                {
                    "category": "prefs",
                    "key": "theme",
                    "body_json": json.dumps({"content": "dark mode"}),
                    "decay_score": 0.9,
                }
            ],
            "total": 1,
        }

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_recall": recall})
    )
    mem = MimirMemory()
    result = _run(mem.query("theme"))

    assert len(result.results) == 1
    item = result.results[0]
    assert item.content == "dark mode"
    assert item.metadata["category"] == "prefs"
    assert item.metadata["key"] == "theme"


def test_update_context_injects_system_message(monkeypatch, MimirMemory):
    from autogen_core.model_context import ChatCompletionContext

    def context(args):
        return {"context": "## Memory\n- user prefers dark mode"}

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_context": context})
    )
    mem = MimirMemory()
    ctx = ChatCompletionContext()
    result = _run(mem.update_context(ctx))

    assert len(ctx.messages) == 1
    assert "dark mode" in ctx.messages[0].content
    assert len(result.memories.results) == 1


def test_update_context_empty_is_noop(monkeypatch, MimirMemory):
    from autogen_core.model_context import ChatCompletionContext

    def context(args):
        return {"context": ""}

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_context": context})
    )
    mem = MimirMemory()
    ctx = ChatCompletionContext()
    result = _run(mem.update_context(ctx))

    assert ctx.messages == []
    assert result.memories.results == []


def test_clear_prunes_category(monkeypatch, MimirMemory):
    captured = {}

    def prune(args):
        captured.update(args)
        return {"archived": 3}

    monkeypatch.setattr(
        "mimir_autogen.subprocess.Popen", _fake_popen({"mimir_prune": prune})
    )
    mem = MimirMemory(category="autogen")
    _run(mem.clear())
    assert captured["category"] == "autogen"


def test_unwrap_handles_text_only_envelope(MimirMemory):
    assert MimirMemory._unwrap_result(
        {"content": [{"type": "text", "text": json.dumps({"items": [1, 2]})}]}
    ) == {"items": [1, 2]}

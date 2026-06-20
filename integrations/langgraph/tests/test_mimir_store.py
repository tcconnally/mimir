"""Tests for MimirStore — the LangGraph BaseStore backed by Mimir.

These mock the ``mimir`` subprocess with the *real* MCP JSON-RPC envelope
Mimir emits (``result.structuredContent`` / ``result.content[0].text``), so
they pin the response-parsing contract without needing the Rust binary.
"""

from __future__ import annotations

import json
from datetime import datetime

import pytest

from mimir_langgraph import MimirStore


def _make_fake_popen(routes):
    """Build a fake ``subprocess.Popen`` driven by ``routes``.

    ``routes`` maps a Mimir tool name to a callable(arguments) -> payload dict.
    The fake drives the persistent stdio session the store uses (write/readline,
    not communicate): each ``initialize`` write gets an empty result and each
    ``tools/call`` write gets the routed payload in Mimir's real MCP envelope.
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
        last_input: str | None = None

        def __init__(self, *args, **kwargs):
            self.args = args[0] if args else kwargs.get("args")
            self._stdout = FakeStdout()
            self.stderr = None

            class _Stdin:
                def __init__(self, outer):
                    self.outer = outer

                def write(self, data):
                    FakePopen.last_input = data
                    for line in data.splitlines():
                        line = line.strip()
                        if not line:
                            continue
                        msg = json.loads(line)
                        mid = msg.get("id")
                        if msg.get("method") == "initialize":
                            self.outer._stdout.push(json.dumps({
                                "jsonrpc": "2.0", "id": mid,
                                "result": {"protocolVersion": "2024-11-05"},
                            }) + "\n")
                        elif msg.get("method") == "tools/call":
                            name = msg["params"]["name"]
                            payload = routes[name](msg["params"]["arguments"])
                            self.outer._stdout.push(json.dumps({
                                "jsonrpc": "2.0", "id": mid,
                                "result": {
                                    "content": [{"type": "text", "text": json.dumps(payload)}],
                                    "structuredContent": payload,
                                },
                            }) + "\n")

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


def _patch(monkeypatch, routes):
    monkeypatch.setattr("mimir_langgraph.subprocess.Popen", _make_fake_popen(routes))


def test_get_parses_structured_content(monkeypatch):
    routes = {
        "mimir_recall": lambda a: {
            "items": [
                {
                    "key": "prefs",
                    "category": "users/123",
                    "body_json": json.dumps({"theme": "dark"}),
                    "created_at_unix_ms": 1700000000000,
                    "last_accessed_unix_ms": 1700000005000,
                    "decay_score": 0.9,
                }
            ],
            "total": 1,
        }
    }
    _patch(monkeypatch, routes)
    store = MimirStore()

    item = store.get(("users", "123"), "prefs")
    assert item is not None
    assert item.value == {"theme": "dark"}
    # Timestamps come back as real datetimes (Item.created_at is typed datetime).
    assert isinstance(item.created_at, datetime)
    assert item.created_at.year == 2023


def test_get_returns_none_when_no_match(monkeypatch):
    _patch(monkeypatch, {"mimir_recall": lambda a: {"items": [], "total": 0}})
    store = MimirStore()
    assert store.get(("users", "123"), "missing") is None


def test_search_maps_items_and_score(monkeypatch):
    routes = {
        "mimir_recall": lambda a: {
            "items": [
                {
                    "key": "n1",
                    "body_json": json.dumps({"text": "hello"}),
                    "created_at_unix_ms": 1700000000000,
                    "decay_score": 0.42,
                }
            ],
            "total": 1,
        }
    }
    _patch(monkeypatch, routes)
    store = MimirStore()

    results = store.search(("notes",), query="hello")
    assert len(results) == 1
    assert results[0].key == "n1"
    assert results[0].value == {"text": "hello"}
    assert results[0].score == 0.42


def test_put_sends_type_not_entity_type(monkeypatch):
    captured = {}

    def remember(args):
        captured.update(args)
        return {"id": "mem-1", "status": "ok"}

    _patch(monkeypatch, {"mimir_remember": remember})
    store = MimirStore()

    store.put(("users", "123"), "prefs", {"theme": "dark"})
    assert captured["category"] == "users/123"
    assert captured["key"] == "prefs"
    assert json.loads(captured["body_json"]) == {"theme": "dark"}
    # Regression: Mimir's param is ``type``; ``entity_type`` was silently dropped.
    assert captured.get("type") == "langgraph_item"
    assert "entity_type" not in captured


def test_list_namespaces_reads_by_category(monkeypatch):
    routes = {
        "mimir_stats": lambda a: {
            "by_category": {"users/123": 3, "notes": 5, "default": 1}
        }
    }
    _patch(monkeypatch, routes)
    store = MimirStore()

    namespaces = store.list_namespaces()
    assert ("users", "123") in namespaces
    assert ("notes",) in namespaces


def test_unwrap_prefers_structured_then_text():
    # structuredContent wins when present.
    assert MimirStore._unwrap_result(
        {"structuredContent": {"items": [1]}, "content": [{"text": "{}"}]}
    ) == {"items": [1]}
    # Falls back to parsing content[0].text JSON.
    assert MimirStore._unwrap_result(
        {"content": [{"type": "text", "text": json.dumps({"items": [2]})}]}
    ) == {"items": [2]}
    # Garbage text yields an empty dict rather than blowing up.
    assert MimirStore._unwrap_result({"content": [{"text": "not json"}]}) == {}

"""Tests for the CrewAI MimirMemoryTool.

CrewAI (and its heavy dependency tree) need not be installed to exercise the
Mimir wiring: we stub ``crewai.tools.BaseTool`` with a minimal base class, then
drive the tool against Mimir's real MCP JSON-RPC envelope via a fake subprocess.
"""

from __future__ import annotations

import json
import sys
import types

import pytest


@pytest.fixture(scope="module")
def MimirMemoryTool():
    """Import MimirMemoryTool with a stubbed ``crewai.tools.BaseTool``."""
    if "crewai" not in sys.modules:
        crewai = types.ModuleType("crewai")
        tools = types.ModuleType("crewai.tools")

        class BaseTool:
            name: str = ""
            description: str = ""

            def __init__(self, *args, **kwargs):
                pass

        tools.BaseTool = BaseTool
        crewai.tools = tools
        sys.modules["crewai"] = crewai
        sys.modules["crewai.tools"] = tools

    from mimir_crewai import MimirMemoryTool as tool_cls

    return tool_cls


def _fake_popen(routes):
    class FakePopen:
        last_input = None

        def __init__(self, *args, **kwargs):
            pass

        def communicate(self, input=None, timeout=None):  # noqa: A002
            FakePopen.last_input = input
            call = next(
                m
                for m in (json.loads(line) for line in input.splitlines() if line)
                if m.get("id") == 2
            )
            payload = routes[call["params"]["name"]](call["params"]["arguments"])
            init_resp = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {}})
            tool_resp = json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "content": [{"type": "text", "text": json.dumps(payload)}],
                        "structuredContent": payload,
                    },
                }
            )
            return (init_resp + "\n" + tool_resp + "\n", "")

        def kill(self):
            pass

    return FakePopen


def test_remember_sends_type(monkeypatch, MimirMemoryTool):
    captured = {}

    def remember(args):
        captured.update(args)
        return {"id": "mem-1", "status": "ok"}

    monkeypatch.setattr(
        "mimir_crewai.subprocess.Popen", _fake_popen({"mimir_remember": remember})
    )
    tool = MimirMemoryTool()
    out = tool._remember(category="crewai", key="k1", content="hello world")

    assert captured.get("type") == "fact"  # regression: was the dropped "entity_type"
    assert "entity_type" not in captured
    assert json.loads(captured["body_json"]) == {"content": "hello world"}
    assert "Remembered" in out


def test_recall_parses_structured_items(monkeypatch, MimirMemoryTool):
    def recall(args):
        return {
            "items": [
                {
                    "category": "crewai",
                    "key": "k1",
                    "body_json": json.dumps({"content": "the answer is 42"}),
                }
            ],
            "total": 1,
        }

    monkeypatch.setattr(
        "mimir_crewai.subprocess.Popen", _fake_popen({"mimir_recall": recall})
    )
    tool = MimirMemoryTool()
    out = tool._recall(query="answer")

    # Before the envelope-unwrap fix this returned "No memories found".
    assert "Found 1 memory" in out
    assert "the answer is 42" in out


def test_unwrap_handles_text_only_envelope(MimirMemoryTool):
    assert MimirMemoryTool._unwrap_result(
        {"content": [{"type": "text", "text": json.dumps({"items": [1, 2]})}]}
    ) == {"items": [1, 2]}

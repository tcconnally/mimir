# Remote Transport: SSE & Streamable HTTP

Mimir is an MCP **stdio** server by default. For remote access — Claude Desktop,
the Anthropic MCP Connector API, or any HTTP MCP client — it also ships a full
**SSE** and **Streamable HTTP** transport, with optional Bearer-token auth.

## Transport modes

| Flag | Endpoints | Use case |
|---|---|---|
| *(default)* | stdio | Local clients (Hermes, Claude Code, Cursor) |
| `--transport sse` | `GET /sse` + `POST /message` | Claude Desktop, MCP Connector API |
| `--transport http` | `POST /message` only | Stateless Streamable HTTP |

## Quick start

```bash
mimir --db /path/to/mimir.db \
  --transport sse \
  --web-bind 0.0.0.0 \
  --port 8765
```

Output:

```
mimir: MCP over sse transport on http://0.0.0.0:8765
mimir: POST http://0.0.0.0:8765/message
mimir: GET  http://0.0.0.0:8765/sse
```

> ⚠️ `--web-bind 0.0.0.0` exposes the server on all interfaces. Only do this
> behind a reverse proxy/tunnel **and** with `--mcp-token` set (see below).
> The default bind is `127.0.0.1` (loopback only).

## Authentication (`--mcp-token`)

When `--mcp-token` is set, **every** transport route requires a matching
`Authorization: Bearer <token>` header. Requests without it — or with the wrong
token — get `401 Unauthorized` with a `WWW-Authenticate: Bearer` header. Has no
effect on stdio transport.

```bash
mimir --db /path/to/mimir.db \
  --transport http \
  --web-bind 0.0.0.0 \
  --port 8765 \
  --mcp-token "$(openssl rand -hex 32)"
```

Client request:

```bash
curl -s -X POST http://localhost:8765/message \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

| Auth state | Response |
|---|---|
| No `Authorization` header | `401 Unauthorized` |
| Wrong token | `401 Unauthorized` |
| Correct `Bearer <token>` | `200 OK` (request processed) |

When `--mcp-token` is **not** set, auth is skipped entirely (backward
compatible) — appropriate only for loopback (`127.0.0.1`) deployments.

## Verify the endpoint

```python
import json, urllib.request

TOKEN = "YOUR_TOKEN"  # omit the header entirely if running without --mcp-token

def jsonrpc(method, params=None, id=1):
    body = {"jsonrpc": "2.0", "id": id, "method": method, "params": params or {}}
    req = urllib.request.Request(
        "http://localhost:8765/message",
        data=json.dumps(body).encode(),
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {TOKEN}",
        },
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.loads(resp.read().decode())

init = jsonrpc("initialize", {
    "protocolVersion": "2024-11-05",
    "capabilities": {},
    "clientInfo": {"name": "test", "version": "1.0"},
})
print("Server:", init["result"]["serverInfo"])

tools = jsonrpc("tools/list", id=2)
print("Tools:", len(tools["result"]["tools"]))
```

## Connecting from the Anthropic MCP Connector API

The SSE endpoint must be publicly reachable (e.g. behind a Cloudflare tunnel).
Pass the Bearer token via the connector's `authorization_token` field:

```python
client.beta.messages.create(
    model="claude-opus-4-8",
    mcp_servers=[{
        "type": "url",
        "url": "https://mimir-mcp.example.com/sse",
        "name": "mimir",
        "authorization_token": "YOUR_TOKEN",  # matches --mcp-token
    }],
    tools=[{"type": "mcp_toolset", "mcp_server_name": "mimir"}],
    betas=["mcp-client-2025-11-20"],
)
```

## Docker

```bash
docker run -p 8765:8765 \
  -v ~/.mimir/data:/data \
  ghcr.io/perseus-computing-llc/mimir:latest \
  --db /data/mimir.db \
  --transport sse \
  --web-bind 0.0.0.0 \
  --port 8765 \
  --mcp-token YOUR_TOKEN
```

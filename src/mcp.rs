use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

use crate::db::Database;
use crate::tools;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

struct MCPState {
    initialized: bool,
}

impl MCPState {
    fn new() -> Self {
        MCPState { initialized: false }
    }
}

/// Run the MCP server loop: read JSON-RPC from stdin, write responses to stdout.
pub fn run_server(db: Database) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut state = MCPState::new();

    eprintln!("mimir: MCP server ready");

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mimir: stdin read error: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("mimir: JSON parse error: {} in line: {}", e, line);
                let error_response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("Parse error: {}", e)}
                });
                let _ = writeln!(stdout, "{}", error_response);
                let _ = stdout.flush();
                continue;
            }
        };

        let response = handle_request(&request, &mut state, &db);

        if let Some(resp) = response {
            let resp_str = serde_json::to_string(&resp).unwrap_or_else(|_| {
                json!({
                    "jsonrpc": "2.0",
                    "id": request.id,
                    "error": {"code": -32603, "message": "Internal error: serialization failed"}
                })
                .to_string()
            });
            let _ = writeln!(stdout, "{}", resp_str);
            let _ = stdout.flush();
        }
    }
}

fn handle_request(
    req: &JsonRpcRequest,
    state: &mut MCPState,
    db: &Database,
) -> Option<JsonRpcResponse> {
    let id = req.id.clone();

    if req.jsonrpc != "2.0" {
        return Some(error_response(
            id,
            -32600,
            "Invalid Request: jsonrpc must be \"2.0\"",
        ));
    }

    match req.method.as_str() {
        "initialize" => {
            let response = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "protocolVersion": "2025-06-18",
                    "serverInfo": {
                        "name": "mimir",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    }
                })),
                error: None,
            };
            state.initialized = true;
            Some(response)
        }

        "notifications/initialized" => {
            // Notification — no response
            None
        }

        "tools/list" => {
            if !state.initialized {
                return Some(error_response(id, -32002, "Not initialized"));
            }
            Some(list_tools(id))
        }

        "tools/call" => {
            if !state.initialized {
                return Some(error_response(id, -32002, "Not initialized"));
            }

            let params = match &req.params {
                Some(p) => p,
                None => return Some(error_response(id, -32602, "Missing params")),
            };

            let tool_name = match params.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => return Some(error_response(id, -32602, "Missing tool name")),
            };

            let tool_args = params.get("arguments").cloned().unwrap_or(json!({}));

            let result_text = call_tool(tool_name, db, tool_args, id.clone());

            match result_text {
                Ok(text) => Some(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: Some(json!({
                        "content": [{
                            "type": "text",
                            "text": text
                        }]
                    })),
                    error: None,
                }),
                Err(error_response) => Some(error_response),
            }
        }

        _ => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {}", req.method),
        )),
    }
}

/// Build the tools/list response with all 15 tools.
fn list_tools(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!({
            "tools": [
                {
                    "name": "mimir_remember",
                    "description": "Store or update an entity. Idempotent by (category, key) — call as many times as you want.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "category": {"type": "string", "description": "Entity category (e.g. 'decision', 'architecture', 'project')"},
                            "key": {"type": "string", "description": "Unique key within the category (e.g. 'use-postgres-16')"},
                            "body_json": {"type": "string", "description": "JSON object with entity body — flexible schema"},
                            "status": {"type": "string", "default": "active", "description": "Entity status"},
                            "type": {"type": "string", "default": "insight", "description": "Entity type: insight, architecture, decision, reference"},
                            "tags": {"type": "array", "items": {"type": "string"}, "description": "Tags for categorization"},
                            "importance": {"type": "number", "default": 0.5, "description": "0.0–1.0 importance (used as initial decay score)"},
                            "topic_path": {"type": "string", "default": "", "description": "Hierarchical topic path (e.g. 'architecture/database')"}
                        },
                        "required": ["category", "key", "body_json"]
                    }
                },
                {
                    "name": "mimir_recall",
                    "description": "Search entities with keyword search (FTS5) plus optional category/type/topic filters.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Search query — words are OR'd together"},
                            "category": {"type": "string", "description": "Filter by category (e.g. 'decision')"},
                            "type": {"type": "string", "description": "Filter by entity type (e.g. 'architecture')"},
                            "limit": {"type": "integer", "default": 10, "description": "Maximum results"},
                            "min_decay": {"type": "number", "default": 0.0, "description": "Minimum decay score threshold (0.0–1.0)"},
                            "topic_path": {"type": "string", "description": "Filter by topic path prefix"},
                            "include_archived": {"type": "boolean", "default": false, "description": "Include archived (soft-deleted) entities"}
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "mimir_forget",
                    "description": "Soft-delete an entity (sets archived=1). Recoverable.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "category": {"type": "string", "description": "Entity category"},
                            "key": {"type": "string", "description": "Entity key"},
                            "reason": {"type": "string", "default": "", "description": "Reason for archiving"}
                        },
                        "required": ["category", "key"]
                    }
                },
                {
                    "name": "mimir_link",
                    "description": "Create a relationship link from one entity to another.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "from_category": {"type": "string", "description": "Source entity category"},
                            "from_key": {"type": "string", "description": "Source entity key"},
                            "to_id": {"type": "string", "description": "Target entity ID"},
                            "relationship": {"type": "string", "default": "related", "description": "Relationship type (e.g. 'depends_on', 'implements')"}
                        },
                        "required": ["from_category", "from_key", "to_id"]
                    }
                },
                {
                    "name": "mimir_unlink",
                    "description": "Remove a link from one entity to another.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "from_category": {"type": "string", "description": "Source entity category"},
                            "from_key": {"type": "string", "description": "Source entity key"},
                            "to_id": {"type": "string", "description": "Target entity ID to unlink"}
                        },
                        "required": ["from_category", "from_key", "to_id"]
                    }
                },
                {
                    "name": "mimir_journal",
                    "description": "Append a journal event — structured decision/observation log with evaluated/acted/forward fields.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "event_type": {"type": "string", "default": "decision", "description": "Event type: decision, observation, action, error"},
                            "evaluated": {"type": "object", "description": "What was evaluated (options, context)"},
                            "acted": {"type": "object", "description": "What action was taken and why"},
                            "forward": {"type": "object", "description": "What the plan is going forward"},
                            "category": {"type": "string", "description": "Related entity category"},
                            "key": {"type": "string", "description": "Related entity key"},
                            "entity_id": {"type": "string", "description": "Related entity ID"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_timeline",
                    "description": "Query journal events by time range and optional filters.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "from_ms": {"type": "integer", "description": "Start time (unix ms)"},
                            "to_ms": {"type": "integer", "description": "End time (unix ms)"},
                            "event_type": {"type": "string", "description": "Filter by event type"},
                            "category": {"type": "string", "description": "Filter by related category"},
                            "entity_id": {"type": "string", "description": "Filter by related entity ID"},
                            "limit": {"type": "integer", "default": 50, "description": "Maximum results"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_state_set",
                    "description": "Set a key-value state entry with optional TTL (auto-expires).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "key": {"type": "string", "description": "State key"},
                            "value_json": {"type": "string", "description": "JSON value"},
                            "ttl_seconds": {"type": "integer", "description": "Time-to-live in seconds (optional)"}
                        },
                        "required": ["key", "value_json"]
                    }
                },
                {
                    "name": "mimir_state_get",
                    "description": "Get a state value by key. Returns null if expired or missing.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "key": {"type": "string", "description": "State key to retrieve"}
                        },
                        "required": ["key"]
                    }
                },
                {
                    "name": "mimir_state_delete",
                    "description": "Delete a state entry by key.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "key": {"type": "string", "description": "State key to delete"}
                        },
                        "required": ["key"]
                    }
                },
                {
                    "name": "mimir_state_list",
                    "description": "List state keys, optionally filtered by prefix.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "prefix": {"type": "string", "default": "", "description": "Key prefix filter"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_health",
                    "description": "Check server and database health.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "mimir_stats",
                    "description": "Database statistics: entity counts by category/type/layer, journal count, state count, DB file size.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "mimir_compact",
                    "description": "Archive entities below a decay threshold. Supports dry-run mode.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "min_decay": {"type": "number", "default": 0.1, "description": "Decay threshold — entities below this are archived"},
                            "dry_run": {"type": "boolean", "default": false, "description": "If true, report what would happen without making changes"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_migrate",
                    "description": "Migrate a v0.1.x Mimir database to v0.2.0 schema.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "from_path": {"type": "string", "description": "Path to the v0.1.x database file"}
                        },
                        "required": ["from_path"]
                    }
                },
                {
                    "name": "mimir_context",
                    "description": "Return a pre-formatted markdown context block of top entities for session injection.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "categories": {"type": "array", "items": {"type": "string"}, "description": "Categories to include (empty = all)"},
                            "limit": {"type": "integer", "default": 10, "description": "Maximum entities"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_vault_export",
                    "description": "Export all non-archived entities to .md files with YAML frontmatter in a vault directory. Human-readable, git-trackable, Obsidian-compatible.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "vault_dir": {"type": "string", "default": "~/.mimir/vault", "description": "Directory to write .md files"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_vault_import",
                    "description": "Import .md files from a vault directory into the database. Reads YAML frontmatter + body.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "vault_dir": {"type": "string", "default": "~/.mimir/vault", "description": "Directory to read .md files from"}
                        },
                        "required": []
                    }
                },
                {
                    "name": "mimir_decay",
                    "description": "Recalculate Ebbinghaus decay scores for all entities and auto-archive fully decayed ones (decay < 0.05).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "mimir_workspace_list",
                    "description": "List all distinct entity categories in the database.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                }
            ]
        })),
        error: None,
    }
}

/// Route a tools/call request to the appropriate handler.
fn call_tool(
    name: &str,
    db: &Database,
    args: Value,
    id: Option<Value>,
) -> Result<String, JsonRpcResponse> {
    match name {
        "mimir_remember" => tools::handle_remember(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_recall" => tools::handle_recall(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_forget" => tools::handle_forget(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_link" => tools::handle_link(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_unlink" => tools::handle_unlink(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_journal" => tools::handle_journal(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_timeline" => tools::handle_timeline(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_state_set" => tools::handle_state_set(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_state_get" => tools::handle_state_get(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_state_delete" => tools::handle_state_delete(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_state_list" => tools::handle_state_list(db, args)
            .map_err(|e| error_response(id, -32603, &e)),

        "mimir_health" => Ok(tools::handle_health(db)),

        "mimir_stats" => Ok(tools::handle_stats(db)),

        "mimir_compact" => Ok(tools::handle_compact(db, args)),

        "mimir_migrate" => Ok(tools::handle_migrate(db, args)),

        "mimir_context" => Ok(tools::handle_context(db, args)),

        "mimir_vault_export" => Ok(tools::handle_vault_export(db, args)),
        "mimir_vault_import" => Ok(tools::handle_vault_import(db, args)),
        "mimir_decay" => Ok(tools::handle_decay(db, args)),
        "mimir_workspace_list" => Ok(tools::handle_workspace_list(db)),

        _ => Err(error_response(
            id,
            -32601,
            &format!("Unknown tool: {}", name),
        )),
    }
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_non_json_rpc_2_requests() {
        let db_path = std::env::temp_dir().join(format!(
            "mimir-jsonrpc-version-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db =
            Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");
        let req = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: None,
        };
        let mut state = MCPState::new();

        let resp = handle_request(&req, &mut state, &db).expect("error response");
        assert_eq!(resp.error.expect("json-rpc error").code, -32600);
        assert!(!state.initialized);

        let _ = fs::remove_file(db_path);
    }
}

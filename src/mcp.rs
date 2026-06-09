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

/// Run the MCP server loop: read JSON-RPC from stdin, write responses to stdout
pub fn run_server(db: Database) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let mut state = MCPState::new();

    eprintln!("mneme: MCP server ready");

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mneme: stdin read error: {}", e);
                break;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("mneme: JSON parse error: {} in line: {}", e, line);
                // Try to send a parse error if we can extract an id
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
                        "name": "mneme",
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
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "tools": [
                        {
                            "name": "mneme_recall",
                            "description": "Search memories with hybrid keyword+vector search",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "query": {
                                        "type": "string",
                                        "description": "Natural language search query"
                                    },
                                    "memory_types": {
                                        "type": "array",
                                        "items": { "type": "string" },
                                        "description": "Filter: insight, architecture, decision"
                                    },
                                    "max_results": {
                                        "type": "integer",
                                        "default": 10,
                                        "description": "Maximum number of results"
                                    },
                                    "workspace_hash": {
                                        "type": "string",
                                        "description": "Scope to specific workspace"
                                    },
                                    "include_federation": {
                                        "type": "boolean",
                                        "default": false,
                                        "description": "Cross-workspace search"
                                    },
                                    "filters": {
                                        "type": "object",
                                        "description": "Additional key-value filters"
                                    },
                                    "min_decay_score": {
                                        "type": "number",
                                        "default": 0.0,
                                        "description": "Ebbinghaus threshold (0.0-1.0)"
                                    },
                                    "topic_path": {
                                        "type": "string",
                                        "description": "e.g. architecture/database/choice"
                                    }
                                },
                                "required": ["query"]
                            }
                        },
                        {
                            "name": "mneme_store",
                            "description": "Store a new memory",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "content": {
                                        "type": "string",
                                        "description": "Full memory text"
                                    },
                                    "memory_type": {
                                        "type": "string",
                                        "default": "insight",
                                        "enum": ["insight", "architecture", "decision"]
                                    },
                                    "workspace_hash": {
                                        "type": "string",
                                        "description": "Originating workspace"
                                    },
                                    "tags": {
                                        "type": "object",
                                        "description": "Key-value tags"
                                    },
                                    "links": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "target_id": { "type": "string" },
                                                "relationship": { "type": "string" },
                                                "weight": { "type": "number" }
                                            }
                                        }
                                    },
                                    "importance": {
                                        "type": "number",
                                        "default": 0.5,
                                        "description": "0.0-1.0 importance"
                                    },
                                    "topic_path": {
                                        "type": "string",
                                        "description": "Optional topic path"
                                    }
                                },
                                "required": ["content"]
                            }
                        },
                        {
                            "name": "mneme_health",
                            "description": "Check server and database health",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        }
                    ]
                })),
                error: None,
            })
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

            let result_text = match tool_name {
                "mneme_recall" => match tools::handle_recall(db, tool_args) {
                    Ok(text) => text,
                    Err(e) => return Some(error_response(id, -32603, &e)),
                },
                "mneme_store" => match tools::handle_store(db, tool_args) {
                    Ok(text) => text,
                    Err(e) => return Some(error_response(id, -32603, &e)),
                },
                "mneme_health" => tools::handle_health(db),
                _ => {
                    return Some(error_response(
                        id,
                        -32601,
                        &format!("Unknown tool: {}", tool_name),
                    ))
                }
            };

            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(json!({
                    "content": [{
                        "type": "text",
                        "text": result_text
                    }]
                })),
                error: None,
            })
        }

        _ => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {}", req.method),
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
        let db_path =
            std::env::temp_dir().join(format!("mneme-jsonrpc-version-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");
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

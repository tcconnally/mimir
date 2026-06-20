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

pub struct MCPState {
    pub initialized: bool,
}

impl MCPState {
    pub fn new() -> Self {
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

pub fn handle_request(
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

            // Try to parse the result as JSON for structuredContent
            let structured: Option<serde_json::Value> = serde_json::from_str(&result_text).ok();
            let mut result = json!({
                "content": [{
                    "type": "text",
                    "text": result_text
                }]
            });
            // Copy isError through from the tool handler's result if present
            if let Some(parsed) = &structured {
                result["structuredContent"] = parsed.clone();
                if let Some(is_err) = parsed.get("isError") {
                    result["isError"] = is_err.clone();
                }
            }
            Some(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(result),
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

/// Build the tools/list response with all 30 tools including outputSchema and annotations.
fn list_tools(id: Option<Value>) -> JsonRpcResponse {
    // Tools are defined inline as serde_json::Value for maximum flexibility.
    // The json!() macro would require the exact structure at compile time,
    // but since we have 30 tools with nested outputSchema, we parse from a string.
    let tools_json: serde_json::Value = serde_json::from_str(
        r###"[
  {
    "name": "mimir_remember",
    "description": "Store or update an entity by (category, key). Idempotent \u2014 call as often as you want, same key returns an update. Optional always_on=true injects entity into every mimir_context. Optional certainty (0.0-1.0) is used by mimir_conflicts for typed-entity conflict detection. Use this for saving facts, decisions, architecture notes, and conventions. When encryption is enabled, body_json is encrypted at rest with AES-256-GCM.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category: 'decision', 'architecture', 'convention', 'insight', or custom"
        },
        "key": {
          "type": "string",
          "description": "Unique key within the category, e.g. 'use-postgres-16' or 'deployment-strategy'"
        },
        "body_json": {
          "type": "string",
          "description": "JSON object with the entity body \u2014 store content, summary, and any custom fields here"
        },
        "status": {
          "type": "string",
          "default": "active",
          "description": "Entity status: 'active', 'draft', 'deprecated'"
        },
        "type": {
          "type": "string",
          "default": "insight",
          "description": "Entity type: 'insight', 'architecture', 'decision', 'reference', 'convention'"
        },
        "tags": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Tags for categorization and cross-referencing"
        },
        "importance": {
          "type": "number",
          "default": 0.5,
          "description": "Initial importance 0.0\u20131.0 \u2014 sets the starting decay score"
        },
        "topic_path": {
          "type": "string",
          "default": "",
          "description": "Hierarchical topic path, e.g. 'architecture/database/postgres'"
        },
        "workspace_hash": {
          "type": "string",
          "default": "",
          "description": "Workspace scope identifier (v1.2.0). Empty = global. Entities with a workspace_hash are invisible to recall queries scoped to a different workspace."
        },
        "agent_id": {
          "type": "string",
          "default": "",
          "description": "Agent identity (v1.2.0). Tracks which agent wrote this entity. Used for agent attribution and context filtering."
        }
      },
      "required": [
        "category",
        "key",
        "body_json"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "id": {
          "type": "string",
          "description": "Entity ID, e.g. 'mem-a1b2c3d4e5f6'"
        },
        "action": {
          "type": "string",
          "description": "'created' for new entities, 'updated' for existing ones"
        },
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_recall",
    "description": "Search entities with FTS5 keyword search. Words are OR'd together. Returns entities sorted by relevance with expanded content/summary fields at top level. Use this to find previously stored facts, decisions, or architecture notes. When encryption is enabled, body_json is decrypted transparently.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Search query \u2014 words are OR'd together for broad recall"
        },
        "category": {
          "type": "string",
          "description": "Filter by category, e.g. 'decision' or 'architecture'"
        },
        "type": {
          "type": "string",
          "description": "Filter by entity type, e.g. 'insight' or 'reference'"
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of results to return (max 1000)"
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "description": "Number of results to skip for pagination"
        },
        "min_decay": {
          "type": "number",
          "default": 0.0,
          "description": "Minimum decay score threshold 0.0\u20131.0 \u2014 higher values return fresher results"
        },
        "topic_path": {
          "type": "string",
          "description": "Filter by topic path prefix, e.g. 'architecture/'"
        },
        "mode": {
          "type": "string",
          "default": "fts5",
          "description": "Search mode: 'fts5' (keyword), 'dense' (vector), or 'hybrid' (fused via RRF)",
          "enum": ["fts5", "dense", "hybrid"]
        },
        "include_archived": {
          "type": "boolean",
          "default": false,
          "description": "Include archived (soft-deleted) entities in results"
        },
        "expansion": {
          "type": "object",
          "properties": {
            "enabled": {
              "type": "boolean",
              "default": false,
              "description": "Enable stemming-based query expansion"
            },
            "n_variants": {
              "type": "integer",
              "default": 1,
              "description": "Number of stemmed token variants to generate"
            }
          },
          "description": "Configuration for FTS5 query expansion using Porter stemming"
        },
        "preview_cap": {
          "type": "integer",
          "description": "If set, truncate body_json at N chars and append drill-down footer. Use mimir_get_entity to read full body."
        },
        "content_weight": {
          "type": "number",
          "minimum": 0,
          "maximum": 1,
          "default": 0,
          "description": "Additive boost for content witness — rewards entities whose body text literally contains query terms. Damped by body length. Never penalizes."
        },
        "diversity_halving": {
          "type": "number",
          "minimum": 0,
          "maximum": 1,
          "default": 1,
          "description": "Per-keyword diversity quota factor (1.0=disabled). Each distinct matched keyword gets ceil(N x halving^n) slots — first keyword N, second N/2, etc."
        },
        "workspace_hash": {
          "type": "string",
          "description": "Workspace scope filter (v1.2.0). When set, only entities with a matching workspace_hash are returned. Omit for no workspace filtering."
        },
        "agent_id": {
          "type": "string",
          "description": "Agent identity filter (v1.2.0). When set, only entities with a matching agent_id are returned. Omit for no agent filtering."
        }
      },
      "required": [
        "query"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "items": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "Matching entities with expanded body_json fields at top level"
        },
        "total": {
          "type": "integer",
          "description": "Number of results returned"
        },
        "variants": {
          "type": "integer",
          "description": "Number of query variants used when expansion is enabled"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_ask",
    "description": "Ask a natural language question and get a grounded answer from stored memories via RAG. Internally recalls top-k entities, assembles context, and queries the configured LLM (Ollama) for an answer with cited sources. Requires --llm-endpoint to be set.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Natural language question to answer from stored memories"
        },
        "top_k": {
          "type": "integer",
          "default": 5,
          "description": "Number of top entities to use as context (max 20)"
        }
      },
      "required": [
        "query"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "answer": {
          "type": "string",
          "description": "Grounded answer with cited sources"
        },
        "sources": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "key": { "type": "string" },
              "category": { "type": "string" },
              "score": { "type": "number" },
              "snippet": { "type": "string" }
            }
          },
          "description": "Cited source entities used in the answer"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true,
      "destructiveHint": false
    }
  },
  {
    "name": "mimir_get_entity",
    "description": "Get an entity by ID with its full body_json content. Use after mimir_recall with preview_cap to read the complete body of a truncated result. The drill-down footer embedded in preview-capped results references this tool with the entity ID to use.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "id": {
          "type": "string",
          "description": "Entity ID to retrieve (from recall result id field or preview cap footer)"
        }
      },
      "required": [
        "id"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "id": { "type": "string" },
        "category": { "type": "string" },
        "key": { "type": "string" },
        "body_json": { "type": "string", "description": "Full entity body content" },
        "status": { "type": "string" },
        "entity_type": { "type": "string" },
        "decay_score": { "type": "number" },
        "retrieval_count": { "type": "integer" },
        "layer": { "type": "string" },
        "always_on": { "type": "boolean" },
        "certainty": { "type": "number" }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_forget",
    "description": "Soft-delete an entity by setting archived=1. The entity is hidden from queries but recoverable. Use this to clean up stale or incorrect facts without permanent data loss.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category to archive"
        },
        "key": {
          "type": "string",
          "description": "Entity key to archive"
        },
        "reason": {
          "type": "string",
          "default": "",
          "description": "Reason for archiving, logged for audit trail"
        }
      },
      "required": [
        "category",
        "key"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "found": {
          "type": "boolean",
          "description": "Whether the entity was found and archived"
        },
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_ingest",
    "description": "Sync external data connectors (GitHub issues, file watcher) into Mimir. Call with no arguments to run all enabled connectors, or specify a connector name to run only that one. Use dry_run=true to preview without storing.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "connector": {
          "type": "string",
          "description": "Specific connector to run (omit for all enabled)"
        },
        "dry_run": {
          "type": "boolean",
          "default": false,
          "description": "Preview documents without storing them"
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "ingested": {
          "type": "integer",
          "description": "Number of documents ingested (or would be ingested in dry run)"
        },
        "dry_run": {
          "type": "boolean",
          "description": "Whether this was a dry run"
        },
        "errors": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Error messages from connectors that failed"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_embed",
    "description": "Generate and store dense vector embeddings for entities via Ollama /api/embed. Supports single entity (category+key) or batch mode (batch_category). Requires --llm-endpoint to be set.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "text": { "type": "string", "description": "Text to embed (omit to use entity body_json)" },
        "category": { "type": "string", "description": "Entity category for single mode" },
        "key": { "type": "string", "description": "Entity key for single mode" },
        "batch_category": { "type": "string", "description": "Embed all entities in this category lacking embeddings" },
        "batch_limit": { "type": "integer", "default": 100, "description": "Max entities in batch mode" }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "embedded": { "type": "integer", "description": "Number of entities embedded" },
        "dimensions": { "type": "integer", "description": "Vector dimensions" }
      }
    },
    "annotations": { "destructiveHint": true }
  },
  {
    "name": "mimir_prune",
    "description": "Bulk archive entities by category, decay threshold, or age. Use dry_run=true to preview without archiving. Useful for cleaning stale or low-quality memories.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": { "type": "string", "description": "Archive entities in this category" },
        "min_decay": { "type": "number", "description": "Archive entities with decay_score below this threshold" },
        "older_than_days": { "type": "integer", "description": "Archive entities older than this many days" },
        "limit": { "type": "integer", "default": 100, "description": "Max entities to prune (0 = unlimited)" },
        "dry_run": { "type": "boolean", "default": false, "description": "Preview without archiving" }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "archived": { "type": "integer" },
        "examined": { "type": "integer" },
        "dry_run": { "type": "boolean" },
        "reason": { "type": "string" }
      }
    },
    "annotations": { "destructiveHint": true }
  },
  {
    "name": "mimir_link",
    "description": "Create a relationship link from one entity to another. Builds a knowledge graph that mimir_traverse can walk. Use 'depends_on', 'implements', 'extends', 'references', or custom relationships.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_category": {
          "type": "string",
          "description": "Source entity category"
        },
        "from_key": {
          "type": "string",
          "description": "Source entity key"
        },
        "to_id": {
          "type": "string",
          "description": "Target entity ID (from mimir_remember return value)"
        },
        "relationship": {
          "type": "string",
          "default": "related",
          "description": "Relationship type: 'depends_on', 'implements', 'extends', 'references', or custom"
        }
      },
      "required": [
        "from_category",
        "from_key",
        "to_id"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "success": {
          "type": "boolean"
        },
        "from": {
          "type": "string",
          "description": "Source as 'category/key'"
        },
        "to": {
          "type": "string",
          "description": "Target entity ID"
        },
        "relationship": {
          "type": "string",
          "description": "Relationship type set"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_unlink",
    "description": "Remove a relationship link from one entity to another. Use this to correct outdated or incorrect links in the knowledge graph.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_category": {
          "type": "string",
          "description": "Source entity category"
        },
        "from_key": {
          "type": "string",
          "description": "Source entity key"
        },
        "to_id": {
          "type": "string",
          "description": "Target entity ID to unlink"
        }
      },
      "required": [
        "from_category",
        "from_key",
        "to_id"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "success": {
          "type": "boolean"
        },
        "from": {
          "type": "string",
          "description": "Source as 'category/key'"
        },
        "to": {
          "type": "string",
          "description": "Target entity ID"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_journal",
    "description": "Append a structured decision/observation log entry. Uses evaluated/acted/forward pattern: what was considered, what was done, and what happens next. Essential for audit trails and timeline reconstruction.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "event_type": {
          "type": "string",
          "default": "decision",
          "description": "Event type: 'decision', 'observation', 'action', 'error'"
        },
        "evaluated": {
          "type": "object",
          "description": "What was evaluated: options considered, context, constraints"
        },
        "acted": {
          "type": "object",
          "description": "What action was taken and why"
        },
        "forward": {
          "type": "object",
          "description": "What the plan is going forward"
        },
        "category": {
          "type": "string",
          "description": "Related entity category for linking"
        },
        "key": {
          "type": "string",
          "description": "Related entity key for linking"
        },
        "entity_id": {
          "type": "string",
          "description": "Related entity ID for linking"
        },
        "agent_id": {
          "type": "string",
          "default": "",
          "description": "Agent identity (v1.2.0). Records which agent created this journal event."
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "id": {
          "type": "string",
          "description": "Journal event ID"
        },
        "event_type": {
          "type": "string",
          "description": "Event type recorded"
        },
        "created_at_unix_ms": {
          "type": "integer",
          "description": "Creation timestamp in unix milliseconds"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_timeline",
    "description": "Query journal events by time range with optional filters for event type, category, or entity. Use this to reconstruct the decision history and understand what happened when.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_ms": {
          "type": "integer",
          "description": "Start time boundary in unix milliseconds"
        },
        "to_ms": {
          "type": "integer",
          "description": "End time boundary in unix milliseconds"
        },
        "event_type": {
          "type": "string",
          "description": "Filter by event type: 'decision', 'observation', 'action', 'error'"
        },
        "category": {
          "type": "string",
          "description": "Filter by related entity category"
        },
        "entity_id": {
          "type": "string",
          "description": "Filter by related entity ID"
        },
        "limit": {
          "type": "integer",
          "default": 50,
          "description": "Maximum number of events to return (max 1000)"
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "description": "Number of events to skip for pagination"
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "items": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "Journal events matching the query"
        },
        "total": {
          "type": "integer",
          "description": "Number of events returned"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_state_set",
    "description": "Set a key-value state entry with optional TTL for auto-expiration. Use this for session state, temporary flags, or configuration values that should expire after a set time.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "State key \u2014 unique identifier for this state entry"
        },
        "value_json": {
          "type": "string",
          "description": "JSON value to store"
        },
        "ttl_seconds": {
          "type": "integer",
          "description": "Time-to-live in seconds. Entry auto-expires and returns null after this duration. Omit for permanent state."
        }
      },
      "required": [
        "key",
        "value_json"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "State key set"
        },
        "ttl_seconds": {
          "type": "integer",
          "description": "TTL that was set, if any"
        },
        "expires_at_unix_ms": {
          "type": "integer",
          "description": "Expiration timestamp in unix milliseconds, if TTL was set"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_state_get",
    "description": "Get a state value by key. Returns null if the key has expired or doesn't exist. Use this instead of mimir_recall for transient session state that doesn't need FTS5 search.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "State key to retrieve"
        }
      },
      "required": [
        "key"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "found": {
          "type": "boolean",
          "description": "Whether the key exists and hasn't expired"
        },
        "key": {
          "type": "string",
          "description": "State key requested"
        },
        "value": {
          "type": "string",
          "description": "JSON value if found"
        },
        "expires_at_unix_ms": {
          "type": "integer",
          "description": "Expiration timestamp if TTL was set"
        },
        "created_at_unix_ms": {
          "type": "integer",
          "description": "Creation timestamp"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_state_delete",
    "description": "Delete a state entry by key. Permanent removal \u2014 unlike mimir_forget which is a soft-delete. Use this to clean up expired or unused state entries.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "State key to permanently delete"
        }
      },
      "required": [
        "key"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "found": {
          "type": "boolean",
          "description": "Whether the key existed and was deleted"
        },
        "key": {
          "type": "string",
          "description": "Key that was deleted"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_state_list",
    "description": "List all state keys, optionally filtered by a key prefix. Use this to discover what state entries exist without knowing exact keys ahead of time.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "prefix": {
          "type": "string",
          "default": "",
          "description": "Only return keys that start with this prefix"
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "keys": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Matching state keys"
        },
        "total": {
          "type": "integer",
          "description": "Number of keys returned"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_health",
    "description": "Check whether the Mimir server and its SQLite database are healthy. Returns a simple healthy/unhealthy status. Use this for health checks and monitoring, not for detailed stats (use mimir_stats).",
    "inputSchema": {
      "type": "object",
      "properties": {}
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "status": {
          "type": "string",
          "enum": [
            "healthy",
            "unhealthy"
          ],
          "description": "Server health status"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_stats",
    "description": "Return comprehensive database statistics: entity counts by category, type, and decay layer; journal event count; state entry count; database file size; and date range of stored data.",
    "inputSchema": {
      "type": "object",
      "properties": {}
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "total_entities": {
          "type": "integer",
          "description": "Total entities in the database"
        },
        "by_category": {
          "type": "object",
          "description": "Entity counts grouped by category"
        },
        "by_type": {
          "type": "object",
          "description": "Entity counts grouped by type"
        },
        "by_layer": {
          "type": "object",
          "description": "Entity counts grouped by decay layer (buffer/working/core)"
        },
        "total_journal_events": {
          "type": "integer",
          "description": "Total journal events recorded"
        },
        "total_state_entries": {
          "type": "integer",
          "description": "Total state entries (including expired)"
        },
        "db_file_size_bytes": {
          "type": "integer",
          "description": "Database file size on disk in bytes"
        },
        "oldest_unix_ms": {
          "type": "integer",
          "description": "Oldest entity creation timestamp"
        },
        "newest_unix_ms": {
          "type": "integer",
          "description": "Newest entity creation timestamp"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_compact",
    "description": "Archive entities whose decay score has fallen below a threshold. Supports dry-run mode to preview without making changes. Run periodically or threshold-triggered to keep the database focused on active, high-value memories.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "min_decay": {
          "type": "number",
          "default": 0.1,
          "description": "Decay threshold \u2014 entities with decay score below this are archived"
        },
        "dry_run": {
          "type": "boolean",
          "default": false,
          "description": "If true, report what would be archived without making changes"
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entities_archived": {
          "type": "integer",
          "description": "Number of entities actually archived (0 in dry-run mode)"
        },
        "entities_examined": {
          "type": "integer",
          "description": "Number of entities checked"
        },
        "dry_run": {
          "type": "boolean",
          "description": "Whether this was a dry run"
        },
        "completed_at_unix_ms": {
          "type": "integer",
          "description": "Completion timestamp"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_migrate",
    "description": "Migrate a v0.1.x Mimir database to the current v0.5.0 schema. Reads the old database, converts memories to the entity model, and merges into the current database. Use this once per legacy database during upgrade.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_path": {
          "type": "string",
          "description": "Absolute path to the v0.1.x SQLite database file to migrate"
        }
      },
      "required": [
        "from_path"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "total_old_memories": {
          "type": "integer",
          "description": "Number of memories found in the old database"
        },
        "entities_created": {
          "type": "integer",
          "description": "New entities created from old memories"
        },
        "entities_updated": {
          "type": "integer",
          "description": "Existing entities updated during merge"
        },
        "errors": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Any errors encountered during migration"
        },
        "completed_at_unix_ms": {
          "type": "integer",
          "description": "Completion timestamp"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_context",
    "description": "Return a pre-formatted markdown context block of the most important entities for session injection. The downstream system (Perseus) uses this to pre-load AI agent context with relevant memories before work begins.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "categories": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Categories to include. Empty array = all categories."
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of entities to include in the context block"
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "markdown": {
          "type": "string",
          "description": "Markdown-formatted context block with entity details"
        },
        "total_chars": {
          "type": "integer",
          "description": "Character count of the markdown content"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_traverse",
    "description": "Walk the entity link graph starting from a given entity up to a configurable depth. Returns a chain of linked entities \u2014 useful for exploring dependencies, decision trees, and relationship graphs built via mimir_link.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Starting entity category"
        },
        "key": {
          "type": "string",
          "description": "Starting entity key"
        },
        "max_depth": {
          "type": "integer",
          "default": 3,
          "description": "Maximum traversal depth from the starting entity"
        },
        "max_nodes": {
          "type": "integer",
          "default": 100,
          "description": "Maximum total nodes to traverse before stopping"
        }
      },
      "required": [
        "category",
        "key"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entity": {
          "type": "object",
          "description": "Root entity with its links"
        },
        "traversed": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "Linked entities traversed from root"
        }
      },
      "required": ["entity", "traversed"]
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_score",
    "description": "Assign a quality score (0.0\u20131.0) to an entity. Verified entities with high scores resist decay and rank higher in recall results. Use this to mark entities as accurate, verified, or deprecated.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category to score"
        },
        "key": {
          "type": "string",
          "description": "Entity key to score"
        },
        "score": {
          "type": "number",
          "description": "Quality score 0.0\u20131.0. 1.0 = verified, 0.5 = neutral, 0.0 = low quality"
        }
      },
      "required": [
        "category",
        "key",
        "score"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "found": {
          "type": "boolean",
          "description": "Whether the entity was found"
        },
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key"
        },
        "score": {
          "type": "number",
          "description": "Quality score assigned"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_conflicts",
    "description": "Detect conflicting entities in the same category \u2014 pairs with low trigram similarity in their body_json. Flags potential contradictions, duplicate-but-divergent entries, and stale-overwritten facts that need human review.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "default": "general",
          "description": "Category to scan for conflicts"
        },
        "threshold": {
          "type": "number",
          "default": 0.4,
          "description": "Similarity threshold \u2014 pairs below this are flagged as conflicts"
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of conflicts to return"
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "description": "Number of entities to skip for pagination"
        }
      },
      "required": [
        "category"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "conflicts": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "Conflict pairs with similarity scores"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_vault_export",
    "description": "Export all non-archived entities to .md files with YAML frontmatter in a vault directory. Files are human-readable, git-trackable, and Obsidian-compatible. Use this for backup, transfer between workspaces, or offline review.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "vault_dir": {
          "type": "string",
          "default": "~/.mimir/vault",
          "description": "Directory path to write .md files. Created if it doesn't exist. Use ~ for home directory."
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "files_created": {
          "type": "integer",
          "description": "Number of new .md files created"
        },
        "files_updated": {
          "type": "integer",
          "description": "Number of existing .md files updated"
        },
        "errors": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Any errors encountered during export"
        },
        "vault_dir": {
          "type": "string",
          "description": "Absolute path to the vault directory"
        },
        "completed_at_unix_ms": {
          "type": "integer",
          "description": "Completion timestamp"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_vault_import",
    "description": "Import .md files from a vault directory into the database. Reads YAML frontmatter for metadata and markdown body for content. Idempotent \u2014 re-running on the same vault won't duplicate entities. Pair with mimir_vault_export for transfer.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "vault_dir": {
          "type": "string",
          "default": "~/.mimir/vault",
          "description": "Directory path to read .md files from. Use ~ for home directory."
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "files_created": {
          "type": "integer",
          "description": "Number of new entities created from files"
        },
        "files_updated": {
          "type": "integer",
          "description": "Number of existing entities updated"
        },
        "errors": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Any errors encountered during import"
        },
        "vault_dir": {
          "type": "string",
          "description": "Absolute path of the vault directory read"
        },
        "completed_at_unix_ms": {
          "type": "integer",
          "description": "Completion timestamp"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_decay",
    "description": "Recalculate Ebbinghaus decay scores for all entities based on time since last access. Auto-archives entities that have fully decayed (score < 0.05). Run periodically to keep memory fresh \u2014 decayed entities surface less often in recall results.",
    "inputSchema": {
      "type": "object",
      "properties": {}
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entities_checked": {
          "type": "integer",
          "description": "Total entities evaluated"
        },
        "entities_updated": {
          "type": "integer",
          "description": "Entities whose decay score changed"
        },
        "auto_archived": {
          "type": "integer",
          "description": "Entities auto-archived because decay fell below 0.05"
        },
        "completed_at_unix_ms": {
          "type": "integer",
          "description": "Completion timestamp"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_reindex",
    "description": "Rebuild the FTS5 search index from the entities table. Repairs index drift — e.g. after a direct SQLite write, an interrupted archive, or a legacy database written before the atomic prune/forget fixes — so archived entities stop surfacing in recall/search. Returns the number of entities reindexed.",
    "inputSchema": {
      "type": "object",
      "properties": {}
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "reindexed": {
          "type": "integer",
          "description": "Number of non-archived entities indexed into FTS5"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  },
  {
    "name": "mimir_workspace_list",
    "description": "List all distinct entity categories present in the database. Use this to discover what knowledge domains exist before querying with mimir_recall or mimir_context.",
    "inputSchema": {
      "type": "object",
      "properties": {}
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "categories": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "All distinct categories in the database"
        },
        "total": {
          "type": "integer",
          "description": "Number of categories"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_recall_when",
    "description": "Search entities whose recall_when triggers match a given context. Use this for proactive just-in-time memory injection \u2014 before writing code, before plans, at session start. Pass the current task description as context and get back memories that declared they should be recalled in similar situations.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "context": {
          "type": "string",
          "description": "The current task or context description to match against recall_when triggers"
        },
        "limit": {
          "type": "integer",
          "description": "Maximum entities to return (default 10, max 100)",
          "default": 10
        }
      },
      "required": ["context"]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "items": {"type": "array", "items": {"type": "object"}},
        "total": {"type": "integer"},
        "context": {"type": "string"}
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_cohere",
    "description": "Run an autonomous coherence grooming pass over the memory. Promotes buffer entities to working layer, applies decay, auto-links related entities, and archives stale ones below the decay threshold. Use dry_run=true to preview without making changes.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "dry_run": {
          "type": "boolean",
          "description": "If true, count what would be done without making changes",
          "default": false
        },
        "max_links": {
          "type": "integer",
          "description": "Maximum auto-links to create (default 20, max 100)",
          "default": 20
        },
        "promote_threshold": {
          "type": "integer",
          "description": "Retrieval count threshold for buffer to working promotion (default 3)",
          "default": 3
        },
        "archive_threshold": {
          "type": "number",
          "description": "Decay score below which entities are auto-archived (default 0.05)",
          "default": 0.05
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "promoted": {"type": "integer", "description": "Number of entities promoted from buffer to working"},
        "decayed": {"type": "integer", "description": "Number of entities whose decay score was reduced"},
        "linked": {"type": "integer", "description": "Number of auto-links created"},
        "archived": {"type": "integer", "description": "Number of entities archived due to low decay"},
        "entities_examined": {"type": "integer", "description": "Total non-archived entities examined"},
        "dry_run": {"type": "boolean"},
        "completed_at_unix_ms": {"type": "integer"}
      }
    },
    "annotations": {
      "destructiveHint": true
    }
  }
]"###
    ).expect("tools JSON must be valid");

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!({
            "tools": tools_json
        })),
        error: None,
    }
}
fn call_tool(name: &str, db: &Database, args: Value, _id: Option<Value>) -> String {
    let handler_result: Result<String, String> = match name {
        "mimir_remember" => tools::handle_remember(db, args).map_err(|e| e.to_string()),

        "mimir_recall" => tools::handle_recall(db, args).map_err(|e| e.to_string()),

        "mimir_ask" => tools::handle_ask(db, args).map_err(|e| e.to_string()),

        "mimir_get_entity" => tools::handle_get_entity(db, args).map_err(|e| e.to_string()),
        "mimir_forget" => tools::handle_forget(db, args).map_err(|e| e.to_string()),

        "mimir_ingest" => tools::handle_ingest(db, args).map_err(|e| e.to_string()),

        "mimir_embed" => tools::handle_embed(db, args).map_err(|e| e.to_string()),

        "mimir_prune" => tools::handle_prune(db, args).map_err(|e| e.to_string()),

        "mimir_link" => tools::handle_link(db, args).map_err(|e| e.to_string()),

        "mimir_unlink" => tools::handle_unlink(db, args).map_err(|e| e.to_string()),

        "mimir_journal" => tools::handle_journal(db, args).map_err(|e| e.to_string()),

        "mimir_timeline" => tools::handle_timeline(db, args).map_err(|e| e.to_string()),

        "mimir_state_set" => tools::handle_state_set(db, args).map_err(|e| e.to_string()),

        "mimir_state_get" => tools::handle_state_get(db, args).map_err(|e| e.to_string()),

        "mimir_state_delete" => tools::handle_state_delete(db, args).map_err(|e| e.to_string()),

        "mimir_state_list" => tools::handle_state_list(db, args).map_err(|e| e.to_string()),

        "mimir_health" => Ok(tools::handle_health(db)),

        "mimir_stats" => Ok(tools::handle_stats(db)),

        "mimir_compact" => Ok(tools::handle_compact(db, args)),

        "mimir_migrate" => Ok(tools::handle_migrate(db, args)),

        "mimir_context" => Ok(tools::handle_context(db, args)),

        "mimir_traverse" => Ok(tools::handle_traverse(db, args)),
        "mimir_score" => Ok(tools::handle_score(db, args)),
        "mimir_conflicts" => Ok(tools::handle_conflicts(db, args)),
        "mimir_vault_export" => Ok(tools::handle_vault_export(db, args)),
        "mimir_vault_import" => Ok(tools::handle_vault_import(db, args)),
        "mimir_decay" => Ok(tools::handle_decay(db, args)),
        "mimir_reindex" => Ok(tools::handle_reindex(db, args)),
        "mimir_workspace_list" => Ok(tools::handle_workspace_list(db)),
        "mimir_recall_when" => tools::handle_recall_when(db, args).map_err(|e| e.to_string()),
        "mimir_cohere" => tools::handle_cohere(db, args).map_err(|e| e.to_string()),

        _ => Err(format!("Unknown tool: {}", name)),
    };

    // MCP spec §3.3: tool failures must return isError:true in the result,
    // NOT a JSON-RPC protocol error (which is reserved for transport/protocol faults).
    match handler_result {
        Ok(text) => text,
        Err(err_msg) => serde_json::to_string(&json!({
            "content": [{"type": "text", "text": err_msg}],
            "isError": true
        }))
        .unwrap_or_else(|_| {
            format!(
                r#"{{"content":[{{"type":"text","text":"{}"}}],"isError":true}}"#,
                err_msg
            )
        }),
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
            std::env::temp_dir().join(format!("mimir-jsonrpc-version-{}.db", uuid::Uuid::new_v4()));
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

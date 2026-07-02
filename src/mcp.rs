use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::sync::OnceLock;

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
    // #210: AtomicBool so the HTTP/SSE transport can share &MCPState across
    // concurrent requests without a Mutex (which would re-serialize them now
    // that the DB pool removed the other lock). handle_request takes &MCPState.
    pub initialized: std::sync::atomic::AtomicBool,
}

impl MCPState {
    pub fn new() -> Self {
        MCPState {
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// Run the MCP server loop: read JSON-RPC from stdin, write responses to stdout.
pub fn run_server(db: Database) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = BufReader::new(stdin.lock());
    let state = MCPState::new();

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

        let response = handle_request(&request, &state, &db);

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
    state: &MCPState,
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
                        // Tracks Cargo.toml's package name automatically, so a
                        // future rename doesn't leave this handshake reporting
                        // stale branding like it did across Mimir -> Mneme ->
                        // Perseus Vault (this was hardcoded to "mimir" the
                        // whole time).
                        "name": env!("CARGO_PKG_NAME"),
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
            state.initialized.store(true, std::sync::atomic::Ordering::Relaxed);
            Some(response)
        }

        "notifications/initialized" => {
            // Notification — no response
            None
        }

        "tools/list" => {
            if !state.initialized.load(std::sync::atomic::Ordering::Relaxed) {
                return Some(error_response(id, -32002, "Not initialized"));
            }
            Some(list_tools(id))
        }

        "tools/call" => {
            if !state.initialized.load(std::sync::atomic::Ordering::Relaxed) {
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
            // Copy isError through, then move the parsed value into
            // structuredContent rather than deep-cloning the whole result (#208).
            if let Some(parsed) = structured {
                if let Some(is_err) = parsed.get("isError") {
                    result["isError"] = is_err.clone();
                }
                result["structuredContent"] = parsed;
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

/// Given a `mimir_*` tool definition from the static registry, return a clone
/// advertised under the equivalent `mneme_*` name (Mneme rename, transition
/// release — both names dispatch to the same handler via `call_tool`).
/// Returns `None` for entries that, unexpectedly, aren't named `mimir_*`.
fn mneme_alias_tool(tool: &serde_json::Value) -> Option<serde_json::Value> {
    let name = tool.get("name")?.as_str()?;
    let suffix = name.strip_prefix("mimir_")?;
    let mut alias = tool.clone();
    alias["name"] = serde_json::Value::String(format!("mneme_{}", suffix));
    Some(alias)
}

/// Given a `mimir_*` tool definition from the static registry, return a clone
/// advertised under the equivalent `perseus_vault_*` name (Perseus Vault
/// rename, transition release — all three names dispatch to the same handler
/// via `call_tool`). Returns `None` for entries that aren't named `mimir_*`.
fn perseus_vault_alias_tool(tool: &serde_json::Value) -> Option<serde_json::Value> {
    let name = tool.get("name")?.as_str()?;
    let suffix = name.strip_prefix("mimir_")?;
    let mut alias = tool.clone();
    alias["name"] = serde_json::Value::String(format!("perseus_vault_{}", suffix));
    Some(alias)
}

/// Build the tools/list response with all 44 tools including outputSchema and annotations.
fn list_tools(id: Option<Value>) -> JsonRpcResponse {
    // The tool registry is a compile-time constant. Parse it exactly once per
    // process and reuse the cached Value instead of re-parsing ~1.8k lines of
    // JSON on every tools/list request (perf review #208).
    //
    // Mneme rename (transition release): the canonical registry below still
    // declares every tool under its original "mimir_*" name. We additionally
    // synthesize a "mneme_*" alias entry for each one so clients that have
    // already moved to the new product name see matching tools/list output.
    // Both names dispatch to the same handler in `call_tool` below.
    static TOOLS: OnceLock<serde_json::Value> = OnceLock::new();
    let tools_json = TOOLS.get_or_init(|| {
        let base = serde_json::from_str::<serde_json::Value>(
        r###"[
  {
    "name": "mimir_remember",
    "description": "Store or update an entity by (category, key). Idempotent — call as often as you want, same key returns an update. Optional always_on=true injects entity into every mimir_context. Optional certainty (0.0-1.0) is used by mimir_conflicts for typed-entity conflict detection. Use this for saving facts, decisions, architecture notes, and conventions. When encryption is enabled, body_json is encrypted at rest with AES-256-GCM.",
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
          "description": "JSON object with the entity body — store content, summary, and any custom fields here"
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
          "description": "Initial importance 0.0–1.0 — sets the starting decay score"
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
    },
    "title": "Remember Entity"
  },
  {
    "name": "mimir_recall",
    "description": "Search entities with FTS5 keyword search. Words are OR'd together. Returns entities sorted by relevance with expanded content/summary fields at top level. Use this to find previously stored facts, decisions, or architecture notes. When encryption is enabled, body_json is decrypted transparently.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Search query — words are OR'd together for broad recall"
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
          "description": "Minimum decay score threshold 0.0–1.0 — higher values return fresher results"
        },
        "topic_path": {
          "type": "string",
          "description": "Filter by topic path prefix, e.g. 'architecture/'"
        },
        "mode": {
          "type": "string",
          "default": "fts5",
          "description": "Search mode: 'fts5' (keyword), 'dense' (vector), or 'hybrid' (fused via RRF)",
          "enum": [
            "fts5",
            "dense",
            "hybrid"
          ]
        },
        "include_archived": {
          "type": "boolean",
          "default": false,
          "description": "Include archived (soft-deleted) entities in results"
        },
        "include_confidence": {
          "type": "boolean",
          "default": false,
          "description": "Add a normalized confidence score (0.0-1.0) to each result, rolled up from rank, trust (verified/certainty), and decay. Presentation-only; does not change ranking."
        },
        "reinforce": {
          "type": "boolean",
          "default": false,
          "description": "Opt-in reinforcement for mode='dense'/'hybrid': bump retrieval_count/last_accessed/decay on the returned hits so semantically-used memories resist decay and promote through layers. Default false keeps semantic recall side-effect-free and byte-deterministic over a frozen DB. No effect on mode='fts5', which already reinforces."
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
        "trust_weight": {
          "type": "number",
          "minimum": 0,
          "maximum": 1,
          "default": 0.15,
          "description": "Additive boost for provenance/trust (default 0.15, on by default) — verified sources rank above unverified AI drafts on the same topic. Verified entities get the full boost; unverified ones are scaled by certainty. Set 0 to disable. Never penalizes."
        },
        "diversity_halving": {
          "type": "number",
          "minimum": 0,
          "maximum": 1,
          "default": 1,
          "description": "Per-keyword diversity quota factor (1.0=disabled). Each distinct matched keyword gets ceil(N x halving^n) slots — first keyword N, second N/2, etc."
        },
        "recency_half_life_secs": {
          "type": "number",
          "minimum": 0,
          "description": "Time-aware ranking for mode='hybrid' (default off). When set, each fused result's score is multiplied by 0.5^(age / this), where age is seconds since the memory was created — so a memory this many seconds old keeps half its weight and recent context outranks older but similar hits. Omit for relevance-only ranking."
        },
        "workspace_hash": {
          "type": "string",
          "description": "Workspace scope filter (v1.2.0). When set, only entities with a matching workspace_hash are returned. Omit for no workspace filtering."
        },
        "agent_id": {
          "type": "string",
          "description": "Agent identity filter (v1.2.0). When set, only entities with a matching agent_id are returned. Omit for no agent filtering."
        },
        "layer": {
            "type": "string",
            "description": "Filter by memory layer (world, episodic, semantic)."
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
    },
    "title": "Recall Entities"
  },
  {
    "name": "mimir_recall_layer",
    "description": "Recall entities from a specific biomimetic memory layer (world, episodic, semantic).",
    "inputSchema": {
      "type": "object",
      "properties": {
        "layer": {
          "type": "string",
          "description": "The memory layer to recall from.",
          "enum": ["world", "episodic", "semantic"]
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of results to return (max 1000)."
        }
      },
      "required": ["layer"]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "items": {
          "type": "array",
          "items": { "type": "object" },
          "description": "Matching entities with expanded body_json fields at top level."
        },
        "total": {
          "type": "integer",
          "description": "Number of results returned."
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    }
  },
  {
    "name": "mimir_semantic_search",
    "description": "Dense-only semantic search: find entities by meaning, ranked purely by embedding similarity (no keyword fallback). On by default via the bundled in-process ONNX model — zero config, zero network. A one-tool shortcut for 'find things like this'. For fused keyword+vector results use mimir_recall.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "query": {
          "type": "string",
          "description": "Natural-language text to semantically match against stored memories"
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of results to return"
        },
        "category": {
          "type": "string",
          "description": "Filter by category, e.g. 'decision' or 'architecture'"
        },
        "workspace_hash": {
          "type": "string",
          "description": "Workspace scope filter. When set, only entities with a matching workspace_hash are returned."
        },
        "agent_id": {
          "type": "string",
          "description": "Agent identity filter. When set, only entities with a matching agent_id are returned."
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
          "description": "Matching entities ranked by dense embedding similarity, with expanded body_json fields at top level"
        },
        "total": {
          "type": "integer",
          "description": "Number of results returned"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Semantic Search Entities"
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
              "key": {
                "type": "string"
              },
              "category": {
                "type": "string"
              },
              "score": {
                "type": "number"
              },
              "snippet": {
                "type": "string"
              }
            }
          },
          "description": "Cited source entities used in the answer"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true,
      "destructiveHint": false
    },
    "title": "Ask Question from Memories"
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
        "id": {
          "type": "string"
        },
        "category": {
          "type": "string"
        },
        "key": {
          "type": "string"
        },
        "body_json": {
          "type": "string",
          "description": "Full entity body content"
        },
        "status": {
          "type": "string"
        },
        "entity_type": {
          "type": "string"
        },
        "decay_score": {
          "type": "number"
        },
        "retrieval_count": {
          "type": "integer"
        },
        "layer": {
          "type": "string"
        },
        "always_on": {
          "type": "boolean"
        },
        "certainty": {
          "type": "number"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Get Entity by ID"
  },
  {
    "name": "mimir_history",
    "description": "List every superseded (historical) version of a fact (category + key), newest first. Each entry was the live fact for an interval before it was overwritten. The companion to mimir_as_of: as_of returns the single version live at one instant; history returns the full version trail. Returns an empty list if the fact has never been overwritten (its only version is the current live one in recall).",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key within the category"
        }
      },
      "required": [
        "category",
        "key"
      ]
    }
  },
  {
    "name": "mimir_as_of",
    "description": "Bi-temporal time-travel: return the version of a fact (category + key) that Mneme believed at a given past instant. When a fact is overwritten, the prior version is kept in history; this returns whichever version was live at as_of_unix_ms. Use to answer 'what did we believe about X back then?' or to audit how a fact changed. Returns found=false if the fact had not been recorded yet at that time.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key within the category"
        },
        "as_of_unix_ms": {
          "type": "integer",
          "description": "Transaction-time instant (unix ms) to travel to"
        }
      },
      "required": [
        "category",
        "key",
        "as_of_unix_ms"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "found": {
          "type": "boolean",
          "description": "False if the fact had not been recorded by as_of_unix_ms"
        },
        "id": {
          "type": "string"
        },
        "category": {
          "type": "string"
        },
        "key": {
          "type": "string"
        },
        "body_json": {
          "type": "string",
          "description": "The fact's content as it was at as_of_unix_ms"
        },
        "status": {
          "type": "string"
        },
        "entity_type": {
          "type": "string"
        },
        "as_of_unix_ms": {
          "type": "integer"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Time-Travel Entity Lookup"
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
    },
    "title": "Forget Entity (Soft-Delete)"
  },
  {
    "name": "mimir_ingest",
    "description": "Sync external data connectors (GitHub issues, file watcher) into Mneme. Call with no arguments to run all enabled connectors, or specify a connector name to run only that one. Use dry_run=true to preview without storing.",
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
          "items": {
            "type": "string"
          },
          "description": "Error messages from connectors that failed"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Ingest External Data"
  },
  {
    "name": "mimir_ingest_file",
    "description": "Ingest a document file into memory by extracting its text LOCALLY (no cloud, no network). Plaintext/markdown/structured-text work in any build; DOCX and PDF require a binary built with --features multimodal (otherwise a clear error is returned). The extracted text is stored as a normal entity (recallable via mimir_recall). category defaults to 'document', key defaults to the file name.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "path": {
          "type": "string",
          "description": "Path to the document file to ingest"
        },
        "category": {
          "type": "string",
          "description": "Entity category (default 'document')"
        },
        "key": {
          "type": "string",
          "description": "Entity key (default: the file name)"
        },
        "tags": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Optional tags"
        }
      },
      "required": [
        "path"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "id": {
          "type": "string",
          "description": "Stored entity id"
        },
        "action": {
          "type": "string",
          "description": "created or updated"
        },
        "category": {
          "type": "string"
        },
        "key": {
          "type": "string"
        },
        "chars": {
          "type": "integer",
          "description": "Characters of text extracted"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Ingest Document File"
  },
  {
    "name": "mimir_embed",
    "description": "Generate and store dense vector embeddings for entities via Ollama /api/embed. Supports single entity (category+key) or batch mode (batch_category). Requires --llm-endpoint to be set.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "text": {
          "type": "string",
          "description": "Text to embed (omit to use entity body_json)"
        },
        "category": {
          "type": "string",
          "description": "Entity category for single mode"
        },
        "key": {
          "type": "string",
          "description": "Entity key for single mode"
        },
        "batch_category": {
          "type": "string",
          "description": "Embed all entities in this category lacking embeddings"
        },
        "batch_limit": {
          "type": "integer",
          "default": 100,
          "description": "Max entities in batch mode"
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "embedded": {
          "type": "integer",
          "description": "Number of entities embedded"
        },
        "dimensions": {
          "type": "integer",
          "description": "Vector dimensions"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Generate Entity Embeddings"
  },
  {
    "name": "mimir_prune",
    "description": "Bulk archive entities by category, decay threshold, or age. Use dry_run=true to preview without archiving. Useful for cleaning stale or low-quality memories.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Archive entities in this category"
        },
        "min_decay": {
          "type": "number",
          "description": "Archive entities with decay_score below this threshold"
        },
        "older_than_days": {
          "type": "integer",
          "description": "Archive entities older than this many days"
        },
        "limit": {
          "type": "integer",
          "default": 100,
          "description": "Max entities to prune (0 = unlimited)"
        },
        "dry_run": {
          "type": "boolean",
          "default": false,
          "description": "Preview without archiving"
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "archived": {
          "type": "integer"
        },
        "examined": {
          "type": "integer"
        },
        "dry_run": {
          "type": "boolean"
        },
        "reason": {
          "type": "string"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Prune Stale Entities"
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
    },
    "title": "Link Entities"
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
    },
    "title": "Unlink Entities"
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
    },
    "title": "Append Journal Entry"
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
    },
    "title": "Query Journal Timeline"
  },
  {
    "name": "mimir_state_set",
    "description": "Set a key-value state entry with optional TTL for auto-expiration. Use this for session state, temporary flags, or configuration values that should expire after a set time.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "key": {
          "type": "string",
          "description": "State key — unique identifier for this state entry"
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
    },
    "title": "Set State Entry"
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
    },
    "title": "Get State Entry"
  },
  {
    "name": "mimir_state_delete",
    "description": "Delete a state entry by key. Permanent removal — unlike mimir_forget which is a soft-delete. Use this to clean up expired or unused state entries.",
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
    },
    "title": "Delete State Entry"
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
    },
    "title": "List State Entries"
  },
  {
    "name": "mimir_health",
    "description": "Check whether the Mneme server and its SQLite database are healthy. Returns a simple healthy/unhealthy status. Use this for health checks and monitoring, not for detailed stats (use mimir_stats).",
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
    },
    "title": "Check Health"
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
    },
    "title": "Get Database Statistics"
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
          "description": "Decay threshold — entities with decay score below this are archived"
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
    },
    "title": "Compact Low-Decay Entities"
  },
  {
    "name": "mimir_purge",
    "description": "Permanently delete all archived entities and run VACUUM to reclaim disk space. This is the only operation that actually removes entities — prune/forget only soft-archive. Archived entities are DELETED and NOT RECOVERABLE. Supports dry_run=true to preview first.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "dry_run": {
          "type": "boolean",
          "default": false,
          "description": "If true, report what would be deleted without making changes"
        }
      },
      "required": []
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entities_deleted": {
          "type": "integer",
          "description": "Number of archived entities permanently deleted"
        },
        "bytes_freed": {
          "type": "integer",
          "description": "Bytes reclaimed after VACUUM (0 in dry-run mode)"
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
    },
    "title": "Purge Archived Entities"
  },
  {
    "name": "mimir_memories",
    "description": "Anthropic memory-tool compatible file interface over the vault: view / create / str_replace / insert / delete / rename on paths under /memories. Files are stored as vault entities (category 'memories', FTS-indexed, encrypted at rest, edits versioned via history), so clients built against Claude's native memory directory convention can use the vault unchanged. Use command='view' with path='/memories' to list files.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "command": {
          "type": "string",
          "enum": ["view", "create", "str_replace", "insert", "delete", "rename"],
          "description": "The operation to perform"
        },
        "path": {
          "type": "string",
          "description": "Path under /memories (e.g. '/memories/notes.md'). For view, '/memories' lists the directory."
        },
        "file_text": {
          "type": "string",
          "description": "create: full file content to write (overwrites an existing file)"
        },
        "old_str": {
          "type": "string",
          "description": "str_replace: exact text to replace — must occur exactly once in the file"
        },
        "new_str": {
          "type": "string",
          "description": "str_replace: replacement text"
        },
        "insert_line": {
          "type": "integer",
          "description": "insert: line number to insert AT (0 = beginning of file)"
        },
        "insert_text": {
          "type": "string",
          "description": "insert: the line to insert"
        },
        "old_path": {
          "type": "string",
          "description": "rename: current path"
        },
        "new_path": {
          "type": "string",
          "description": "rename: destination path (must not exist)"
        }
      },
      "required": [
        "command"
      ]
    },
    "title": "Memories Directory (Anthropic convention)"
  },
  {
    "name": "mimir_migrate",
    "description": "Migrate a v0.1.x Mneme database to the current v0.5.0 schema. Reads the old database, converts memories to the entity model, and merges into the current database. Use this once per legacy database during upgrade.",
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
    },
    "title": "Migrate Legacy Database"
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
        },
        "workspace_hash": {
          "type": "string",
          "description": "Workspace scope filter (v1.2.0). When set, only entities with a matching workspace_hash are included. Omit for no workspace filtering — in a federated vault that leaks every workspace's memory into the block."
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
    },
    "title": "Get Context Block"
  },
  {
    "name": "mimir_extract",
    "description": "Extract structured knowledge — facts, preferences, temporal events, episodes — from raw text or a stored entity, using a fully local, deterministic rule-based extractor (no cloud LLM, no embedding/API call, no network). Read-only: never writes to the store. Provide `text`, or `category` + `key` to extract from a stored entity.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "text": {
          "type": "string",
          "description": "Raw text to extract from. If omitted, category + key of a stored entity are used."
        },
        "category": {
          "type": "string",
          "description": "Category of a stored entity to extract from (requires key)."
        },
        "key": {
          "type": "string",
          "description": "Key of a stored entity to extract from (requires category)."
        },
        "strategy": {
          "type": "string",
          "default": "rule_based",
          "enum": [
            "rule_based",
            "none"
          ],
          "description": "Extractor strategy: 'rule_based' (local heuristics) or 'none' (no-op)."
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
          "description": "Extracted items, each an object with `kind` and `text`."
        },
        "total": {
          "type": "integer",
          "description": "Number of items extracted"
        },
        "strategy": {
          "type": "string",
          "description": "Extractor strategy used"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Extract Structured Knowledge"
  },
  {
    "name": "mimir_traverse",
    "description": "Walk the entity link graph starting from a given entity up to a configurable depth. Returns a chain of linked entities — useful for exploring dependencies, decision trees, and relationship graphs built via mimir_link.",
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
      "required": [
        "entity",
        "traversed"
      ]
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Traverse Entity Graph"
  },
  {
    "name": "mimir_score",
    "description": "Assign a quality score (0.0–1.0) to an entity. The score persists as an importance floor: decay_tick/cohere never recompute decay_score below it, so an explicitly scored memory survives idle time indefinitely (fidelity beats recency). Scores >= 0.7 also mark the entity verified. Re-score with 0.0 to clear the floor. Use this to mark entities as accurate, verified, or deprecated.",
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
          "description": "Quality score 0.0–1.0. 1.0 = verified, 0.5 = neutral, 0.0 = low quality"
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
    },
    "title": "Score Entity Quality"
  },
  {
    "name": "mimir_follow",
    "description": "Record whether an entity (typically a convention/insight/lesson) was actually FOLLOWED or MISSED by the agent — the honest follow-rate signal. Unlike retrieval_count (how often a memory is recalled), this tracks whether recall changed behavior. After enough attempts, efficacy_status flips to 'useful' or 'dead' and feeds into decay scoring so ignored rules decay out of recall while followed ones resist decay.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category"
        },
        "key": {
          "type": "string",
          "description": "Entity key"
        },
        "followed": {
          "type": "boolean",
          "description": "true if the agent's action followed/honored this entity's guidance, false if it was ignored/missed"
        },
        "context": {
          "type": "string",
          "description": "Optional description of the action/context this observation relates to"
        }
      },
      "required": [
        "category",
        "key",
        "followed"
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
          "type": "string"
        },
        "key": {
          "type": "string"
        },
        "follow_count": {
          "type": "integer"
        },
        "miss_count": {
          "type": "integer"
        },
        "follow_rate": {
          "type": "number"
        },
        "efficacy_status": {
          "type": "string",
          "description": "'unverified' | 'useful' | 'dead'"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Record Follow/Miss Efficacy Signal"
  },
  {
    "name": "mimir_conflicts",
    "description": "Detect conflicting entities in the same category — pairs with low trigram similarity in their body_json. Flags potential contradictions, duplicate-but-divergent entries, and stale-overwritten facts. Read-only by default. Opt in with resolve=true to actively invalidate the lower-certainty side of clear conflicts (superseding it into history, reversible + time-travelable via mimir_as_of); that path defaults to dry_run=true so you preview first, and never resolves pairs whose certainties are within certainty_margin.",
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
          "description": "Similarity threshold — pairs below this are flagged as conflicts"
        },
        "limit": {
          "type": "integer",
          "default": 10,
          "description": "Maximum number of conflicts to return / resolve"
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "description": "Number of entities to skip for pagination"
        },
        "resolve": {
          "type": "boolean",
          "default": false,
          "description": "Opt-in: invalidate the lower-certainty side of clear conflicts instead of only reporting them"
        },
        "dry_run": {
          "type": "boolean",
          "default": true,
          "description": "When resolve=true, only report what would be invalidated unless set false"
        },
        "certainty_margin": {
          "type": "number",
          "default": 0.2,
          "description": "Minimum certainty gap to auto-resolve; closer pairs are skipped as ambiguous"
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
          "description": "Conflict pairs with similarity scores (detection mode)"
        },
        "invalidations": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "Winner/loser pairs invalidated or previewed (resolve mode)"
        }
      }
    },
    "annotations": {
      "readOnlyHint": false
    },
    "title": "Detect Conflicting Entities"
  },
  {
    "name": "mimir_consolidate",
    "description": "Merge overlapping/duplicative entities in the same category into durable, evidence-tracked 'observations' — the mirror image of mimir_conflicts, which flags dissimilar (contradictory) pairs. Groups entities whose pairwise trigram similarity meets similarity_threshold, then creates one new entity per group (category='observation') whose body carries a summary (the highest-certainty source's content), the full list of source entity ids as evidence, and a proof_count. Source entities are NOT deleted or archived — they remain independently accessible, and the new observation links back to each of them (relationship='evidence_for') for full audit. Read-only preview with dry_run=true.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Category to scan for overlapping/duplicative entities to consolidate"
        },
        "similarity_threshold": {
          "type": "number",
          "default": 0.6,
          "description": "Trigram similarity threshold at or above which two entities are considered overlapping enough to merge"
        },
        "limit": {
          "type": "integer",
          "default": 50,
          "description": "Maximum number of observations to create"
        },
        "offset": {
          "type": "integer",
          "default": 0,
          "description": "Number of entities to skip for pagination"
        },
        "dry_run": {
          "type": "boolean",
          "default": false,
          "description": "Preview which observations would be created without writing anything"
        }
      },
      "required": [
        "category"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string"
        },
        "entities_examined": {
          "type": "integer",
          "description": "Number of entities scanned in this category"
        },
        "observations_created": {
          "type": "integer",
          "description": "Number of new observation entities created (or would be, in dry-run)"
        },
        "source_entities_merged": {
          "type": "integer",
          "description": "Total count of source entities folded into the created observations"
        },
        "dry_run": {
          "type": "boolean"
        },
        "observations": {
          "type": "array",
          "items": {
            "type": "object"
          },
          "description": "The observations created (or previewed), each with entity_id, key, summary, source_ids, proof_count, certainty"
        }
      }
    },
    "annotations": {
      "readOnlyHint": false
    },
    "title": "Consolidate Overlapping Facts into Observations"
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
    },
    "title": "Export Vault to Files"
  },
  {
    "name": "mimir_vault_import",
    "description": "Import .md files from a vault directory into the database. Reads YAML frontmatter for metadata and markdown body for content. Idempotent — re-running on the same vault won't duplicate entities. Pair with mimir_vault_export for transfer.",
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
    },
    "title": "Import Vault from Files"
  },
  {
    "name": "mimir_decay",
    "description": "Recalculate Ebbinghaus decay scores for all entities based on time since last access. Auto-archives entities that have fully decayed (score < 0.05). Run periodically to keep memory fresh — decayed entities surface less often in recall results.",
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
    },
    "title": "Recalculate Decay Scores"
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
    },
    "title": "Rebuild Search Index"
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
    },
    "title": "List Workspace Categories"
  },
  {
    "name": "mimir_recall_when",
    "description": "Search entities whose recall_when triggers match a given context. Use this for proactive just-in-time memory injection — before writing code, before plans, at session start. Pass the current task description as context and get back memories that declared they should be recalled in similar situations.",
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
        },
        "workspace_hash": {
          "type": "string",
          "description": "Workspace scope filter (v1.2.0). When set, only entities with a matching workspace_hash can fire. Omit for no workspace filtering — in a federated vault that lets one workspace's triggers inject into another's turns."
        }
      },
      "required": [
        "context"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "items": {
          "type": "array",
          "items": {
            "type": "object"
          }
        },
        "total": {
          "type": "integer"
        },
        "context": {
          "type": "string"
        }
      }
    },
    "annotations": {
      "readOnlyHint": true
    },
    "title": "Proactive Recall by Context"
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
        "promoted": {
          "type": "integer",
          "description": "Number of entities promoted from buffer to working"
        },
        "decayed": {
          "type": "integer",
          "description": "Number of entities whose decay score was reduced"
        },
        "linked": {
          "type": "integer",
          "description": "Number of auto-links created"
        },
        "archived": {
          "type": "integer",
          "description": "Number of entities archived due to low decay"
        },
        "entities_examined": {
          "type": "integer",
          "description": "Total non-archived entities examined"
        },
        "dry_run": {
          "type": "boolean"
        },
        "completed_at_unix_ms": {
          "type": "integer"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Run Coherence Grooming"
  },
  {
    "name": "mimir_share",
    "description": "Share an entity to another workspace. Copies the entity (by category + key) from its current workspace into the target workspace, preserving content and metadata while generating a new ID. The original entity is unchanged. Use this for controlled cross-workspace knowledge transfer.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "category": {
          "type": "string",
          "description": "Entity category to share"
        },
        "key": {
          "type": "string",
          "description": "Entity key to share"
        },
        "to_workspace": {
          "type": "string",
          "description": "Target workspace hash to copy the entity into"
        }
      },
      "required": [
        "category",
        "key",
        "to_workspace"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "shared_id": {
          "type": "string",
          "description": "ID of the new shared copy"
        },
        "action": {
          "type": "string",
          "description": "'created' or 'updated'"
        },
        "from_workspace": {
          "type": "string",
          "description": "Source workspace the entity was copied from"
        },
        "to_workspace": {
          "type": "string",
          "description": "Target workspace the entity was copied to"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Share Entity to Workspace"
  },
  {
    "name": "mimir_federate",
    "description": "Federate entities from one workspace to another. Exports entities scoped to from_workspace, remaps their workspace_hash to to_workspace, and imports them — effectively copying or moving knowledge between workspaces. Use this for cross-agent or cross-project knowledge sharing without manual file transfer.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_workspace": {
          "type": "string",
          "description": "Source workspace hash to export entities from"
        },
        "to_workspace": {
          "type": "string",
          "description": "Target workspace hash to import entities into"
        },
        "vault_dir": {
          "type": "string",
          "default": "/tmp/mimir-federate",
          "description": "Temporary vault directory for the intermediate .md export files"
        }
      },
      "required": [
        "from_workspace",
        "to_workspace"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "exported": {
          "type": "integer",
          "description": "Number of entities exported from the source workspace"
        },
        "remapped": {
          "type": "integer",
          "description": "Number of entities whose workspace_hash was remapped"
        },
        "imported": {
          "type": "integer",
          "description": "Number of entities imported into the target workspace"
        },
        "import_errors": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Any errors encountered during import"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Federate Entities Between Workspaces"
  },
  {
    "name": "mimir_correct",
    "description": "Capture a user correction to the agent. Stores what went wrong, what the user said, and the lesson learned — as both a 'correction' entity and a journal entry. Use this every time the user corrects your approach. Enables the self-improving feedback loop: the agent learns from mistakes across sessions.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "wrong_approach": {
          "type": "string",
          "description": "What the agent did that was wrong (the mistaken approach)"
        },
        "user_correction": {
          "type": "string",
          "description": "What the user said to correct the agent (the right way)"
        },
        "task_context": {
          "type": "string",
          "description": "What task was being attempted when the correction occurred"
        },
        "session_id": {
          "type": "string",
          "default": "",
          "description": "Session identifier for traceability"
        },
        "tags": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Tags for categorization"
        },
        "category": {
          "type": "string",
          "default": "correction",
          "description": "Entity category (default: 'correction')"
        },
        "visibility": {
          "type": "string",
          "default": "workspace",
          "description": "Visibility: 'private', 'workspace', or 'public'"
        }
      },
      "required": [
        "wrong_approach",
        "user_correction",
        "task_context"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entity_id": {
          "type": "string",
          "description": "Created correction entity ID"
        },
        "journal_id": {
          "type": "string",
          "description": "Created journal entry ID"
        },
        "category": {
          "type": "string"
        },
        "key": {
          "type": "string"
        },
        "created_at_unix_ms": {
          "type": "integer"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Capture Agent Correction"
  },
  {
    "name": "mimir_synthesize",
    "description": "LLM-driven session synthesis. Reviews a session transcript and extracts structured lessons: what worked (success), what failed (failure), what was corrected (correction), what was abandoned (dead_end), and key decisions made (decision). Each lesson becomes an entity linked to a synthesis journal entry. Requires --llm-endpoint to be configured. This is the Perplexity-Brain-style overnight synthesis loop for agent self-improvement.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "session_content": {
          "type": "string",
          "description": "Full session transcript to synthesize lessons from"
        },
        "session_id": {
          "type": "string",
          "default": "",
          "description": "Session identifier for traceability"
        },
        "tags": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Tags applied to all synthesized entities"
        },
        "visibility": {
          "type": "string",
          "default": "workspace",
          "description": "Visibility for synthesized entities"
        }
      },
      "required": [
        "session_content"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "lessons": {
          "type": "array",
          "items": {
            "type": "object",
            "properties": {
              "lesson_type": {
                "type": "string"
              },
              "summary": {
                "type": "string"
              },
              "evidence": {
                "type": "string"
              },
              "confidence": {
                "type": "number"
              }
            }
          },
          "description": "Extracted lessons with type, summary, evidence, and confidence"
        },
        "entities_created": {
          "type": "integer",
          "description": "Number of lesson entities created"
        },
        "journal_id": {
          "type": "string"
        },
        "dry_run": {
          "type": "boolean"
        },
        "completed_at_unix_ms": {
          "type": "integer"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Synthesize Session Lessons"
  },
  {
    "name": "mimir_bench",
    "description": "Record a performance benchmark data point. Tracks task metrics (turns taken, tokens used, success) alongside whether memory recall was used — enabling measurement of Mneme's impact on agent performance. Aggregate with mimir_recall to analyze trends.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "task_description": {
          "type": "string",
          "description": "Description of the task being measured"
        },
        "turns_taken": {
          "type": "integer",
          "description": "Number of conversation turns the task took"
        },
        "tokens_used": {
          "type": "integer",
          "description": "Total tokens consumed by the task"
        },
        "memory_recall_used": {
          "type": "boolean",
          "description": "Whether memory recall (mimir_recall) was used during this task"
        },
        "recall_count": {
          "type": "integer",
          "default": 0,
          "description": "How many times memory was recalled during this task"
        },
        "task_success": {
          "type": "boolean",
          "default": false,
          "description": "Whether the task completed successfully"
        },
        "session_id": {
          "type": "string",
          "default": "",
          "description": "Session identifier for traceability"
        },
        "tags": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Tags for categorization"
        }
      },
      "required": [
        "task_description",
        "turns_taken",
        "tokens_used",
        "memory_recall_used"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "entity_id": {
          "type": "string",
          "description": "Created benchmark entity ID"
        },
        "created_at_unix_ms": {
          "type": "integer"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Record Benchmark"
  },
  {
    "name": "mimir_autocohere",
    "description": "Run a full atomic grooming pass: cohere (promote, link, archive), then decay (recalculate Ebbinghaus decay), then compact (archive below threshold). Returns a summary report. Use dry_run=true to preview without changes.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "dry_run": {
          "type": "boolean",
          "description": "If true, preview changes without writing",
          "default": false
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "promoted_entities": {
          "type": "integer",
          "description": "Entities promoted during cohere"
        },
        "links_created": {
          "type": "integer",
          "description": "Auto-links created during cohere"
        },
        "archived_entities": {
          "type": "integer",
          "description": "Entities archived (cohere + compact)"
        },
        "decay_updates": {
          "type": "integer",
          "description": "Entities whose decay score was updated"
        },
        "compact_archived_count": {
          "type": "integer",
          "description": "Entities archived during compact step"
        },
        "db_size_delta_bytes": {
          "type": "integer",
          "description": "Change in SQLite file size in bytes"
        },
        "dry_run": {
          "type": "boolean"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Atomic Coherence Pass"
  },
  {
    "name": "mimir_supersede",
    "description": "Create a 'supersedes' relationship from a new fact to an old one, setting the old entity's status to 'deprecated'. Use this when a newer entity makes an older one obsolete.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "from_category": {
          "type": "string",
          "description": "Category of the OLD entity being superseded"
        },
        "from_key": {
          "type": "string",
          "description": "Key of the OLD entity being superseded"
        },
        "to_category": {
          "type": "string",
          "description": "Category of the NEW entity that supersedes"
        },
        "to_key": {
          "type": "string",
          "description": "Key of the NEW entity that supersedes"
        },
        "reason": {
          "type": "string",
          "description": "Reason for superseding (recorded in archive_reason)",
          "default": ""
        },
        "relationship": {
          "type": "string",
          "description": "Link relationship type (default: 'supersedes')",
          "default": "supersedes"
        }
      },
      "required": [
        "from_category",
        "from_key",
        "to_category",
        "to_key"
      ]
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "from_entity_id": {
          "type": "string",
          "description": "ID of the old (superseded) entity"
        },
        "from_entity_category": {
          "type": "string"
        },
        "from_entity_key": {
          "type": "string"
        },
        "to_entity_id": {
          "type": "string",
          "description": "ID of the new (superseding) entity"
        },
        "to_entity_category": {
          "type": "string"
        },
        "to_entity_key": {
          "type": "string"
        },
        "relationship": {
          "type": "string"
        },
        "status_updated": {
          "type": "string",
          "description": "New status of the old entity (always 'deprecated')"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Supersede Entity"
  },
  {
    "name": "mimir_maintenance",
    "description": "Database maintenance operations: deduplicate entities with identical (category, key), detect orphan journal entries and links, vacuum (reclaim disk space), reindex FTS5. Set dry_run=true to preview. Use 'all' to run everything.",
    "inputSchema": {
      "type": "object",
      "properties": {
        "dedup": {
          "type": "boolean",
          "description": "Find duplicate (category, key) entities and archive the oldest",
          "default": false
        },
        "orphans": {
          "type": "boolean",
          "description": "Detect journal entries and links pointing to non-existent entities",
          "default": false
        },
        "vacuum": {
          "type": "boolean",
          "description": "Run SQLite VACUUM to reclaim disk space",
          "default": false
        },
        "reindex": {
          "type": "boolean",
          "description": "Rebuild the FTS5 search index from entities table",
          "default": false
        },
        "all": {
          "type": "boolean",
          "description": "Run all maintenance operations (dedup, orphans, vacuum, reindex)",
          "default": false
        },
        "dry_run": {
          "type": "boolean",
          "description": "If true, preview changes without writing",
          "default": false
        }
      }
    },
    "outputSchema": {
      "type": "object",
      "properties": {
        "dedup_archived": {
          "type": "integer",
          "description": "Number of duplicate entities archived"
        },
        "orphan_journal_entries_found": {
          "type": "integer",
          "description": "Orphan journal entries detected"
        },
        "orphan_links_found": {
          "type": "integer",
          "description": "Orphan links detected"
        },
        "vacuum_reclaimed_bytes": {
          "type": "integer",
          "description": "Disk space reclaimed by VACUUM"
        },
        "reindex_rows_affected": {
          "type": "integer",
          "description": "Rows reindexed into FTS5"
        },
        "dry_run": {
          "type": "boolean"
        },
        "errors": {
          "type": "array",
          "items": {
            "type": "string"
          },
          "description": "Errors encountered during maintenance"
        }
      }
    },
    "annotations": {
      "destructiveHint": true
    },
    "title": "Run Database Maintenance"
  }
]"###
        ).expect("tools JSON must be valid");

        let base_array = base.as_array().expect("tools registry must be a JSON array");
        let mut aliased: Vec<serde_json::Value> = Vec::with_capacity(base_array.len() * 3);
        for tool in base_array {
            aliased.push(tool.clone());
            if let Some(mneme_alias) = mneme_alias_tool(tool) {
                aliased.push(mneme_alias);
            }
            if let Some(vault_alias) = perseus_vault_alias_tool(tool) {
                aliased.push(vault_alias);
            }
        }
        serde_json::Value::Array(aliased)
    });

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!({
            "tools": tools_json.clone()
        })),
        error: None,
    }
}
fn call_tool(name: &str, db: &Database, args: Value, _id: Option<Value>) -> String {
    // Keep the caller's original (un-normalized) name for error messages —
    // a "mneme_bogus"/"perseus_vault_bogus" call should say so, not report
    // back the normalized "mimir_bogus" it was rewritten to below.
    let original_name = name;
    // Mneme/Perseus Vault rename (transition release): "mneme_*" and
    // "perseus_vault_*" are back-compat aliases for "mimir_*" — normalize
    // whichever prefix is present once here so every match arm below keeps
    // dispatching on the original name without needing its own alias arm.
    let owned_name = name
        .strip_prefix("perseus_vault_")
        .or_else(|| name.strip_prefix("mneme_"))
        .map(|suffix| format!("mimir_{}", suffix));
    let name: &str = owned_name.as_deref().unwrap_or(name);

    let handler_result: Result<String, String> = match name {
        "mimir_remember" => tools::handle_remember(db, args).map_err(|e| e.to_string()),

        "mimir_recall" => tools::handle_recall(db, args).map_err(|e| e.to_string()),

        "mimir_recall_layer" => tools::handle_recall_layer(db, args).map_err(|e| e.to_string()),

        "mimir_semantic_search" => {
            tools::handle_semantic_search(db, args).map_err(|e| e.to_string())
        }

        "mimir_ask" => tools::handle_ask(db, args).map_err(|e| e.to_string()),

        "mimir_get_entity" => tools::handle_get_entity(db, args).map_err(|e| e.to_string()),
        "mimir_history" => tools::handle_history(db, args).map_err(|e| e.to_string()),
        "mimir_as_of" => tools::handle_as_of(db, args).map_err(|e| e.to_string()),
        "mimir_forget" => tools::handle_forget(db, args).map_err(|e| e.to_string()),

        "mimir_ingest" => tools::handle_ingest(db, args).map_err(|e| e.to_string()),

        "mimir_ingest_file" => tools::handle_ingest_file(db, args).map_err(|e| e.to_string()),

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

        "mimir_purge" => tools::handle_purge(db, args).map_err(|e| e.to_string()),
        "mimir_memories" => tools::handle_memories(db, args).map_err(|e| e.to_string()),

        "mimir_migrate" => Ok(tools::handle_migrate(db, args)),

        "mimir_context" => Ok(tools::handle_context(db, args)),

        "mimir_extract" => tools::handle_extract(db, args).map_err(|e| e.to_string()),

        "mimir_traverse" => Ok(tools::handle_traverse(db, args)),
        "mimir_score" => Ok(tools::handle_score(db, args)),
        "mimir_follow" => tools::handle_follow(db, args).map_err(|e| e.to_string()),
        "mimir_conflicts" => Ok(tools::handle_conflicts(db, args)),
        "mimir_consolidate" => Ok(tools::handle_consolidate(db, args)),
        "mimir_vault_export" => Ok(tools::handle_vault_export(db, args)),
        "mimir_vault_import" => Ok(tools::handle_vault_import(db, args)),
        "mimir_decay" => Ok(tools::handle_decay(db, args)),
        "mimir_reindex" => Ok(tools::handle_reindex(db, args)),
        "mimir_share" => tools::handle_share(db, args).map_err(|e| e.to_string()),
        "mimir_federate" => tools::handle_federate(db, args).map_err(|e| e.to_string()),
        "mimir_workspace_list" => Ok(tools::handle_workspace_list(db)),
        "mimir_recall_when" => tools::handle_recall_when(db, args).map_err(|e| e.to_string()),
        "mimir_cohere" => tools::handle_cohere(db, args).map_err(|e| e.to_string()),
        "mimir_correct" => tools::handle_correct(db, args).map_err(|e| e.to_string()),
        "mimir_synthesize" => tools::handle_synthesize(db, args).map_err(|e| e.to_string()),
        "mimir_bench" => tools::handle_bench(db, args).map_err(|e| e.to_string()),

        "mimir_autocohere" => tools::handle_autocohere(db, args).map_err(|e| e.to_string()),
        "mimir_supersede" => tools::handle_supersede(db, args).map_err(|e| e.to_string()),
        "mimir_maintenance" => tools::handle_maintenance(db, args).map_err(|e| e.to_string()),

        _ => Err(format!("Unknown tool: {}", original_name)),
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
    fn memories_adapter_full_lifecycle_roundtrip() {
        // The Anthropic /memories directory convention over vault entities:
        // create, list, view (numbered), str_replace (unique-match), insert,
        // rename, delete, and recreate-after-delete (revival must also
        // restore the FTS row so the file is searchable again).
        let db_path = std::env::temp_dir()
            .join(format!("mimir-memories-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");
        let call = |args: Value| -> String {
            call_tool("mimir_memories", &db, args, None)
        };

        // create
        let r = call(json!({"command": "create", "path": "/memories/notes.md",
                            "file_text": "alpha\nbeta\ngamma"}));
        assert!(r.contains("created"), "create failed: {r}");

        // view directory
        let r = call(json!({"command": "view", "path": "/memories"}));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["files"], json!(["notes.md"]), "dir listing: {r}");

        // view file — numbered content
        let r = call(json!({"command": "view", "path": "/memories/notes.md"}));
        assert!(r.contains("beta"), "view content missing: {r}");
        let v: Value = serde_json::from_str(&r).unwrap();
        assert!(
            v["content"].as_str().unwrap().contains("     2\tbeta"),
            "expected cat -n numbering: {r}"
        );

        // str_replace — must reject ambiguous and missing matches
        let r = call(json!({"command": "str_replace", "path": "/memories/notes.md",
                            "old_str": "beta", "new_str": "BETA"}));
        assert!(r.contains("replaced"), "str_replace failed: {r}");
        let r = call(json!({"command": "str_replace", "path": "/memories/notes.md",
                            "old_str": "missing", "new_str": "x"}));
        assert!(r.contains("not found"), "missing old_str must error: {r}");

        // insert at line 0
        let r = call(json!({"command": "insert", "path": "/memories/notes.md",
                            "insert_line": 0, "insert_text": "header"}));
        assert!(r.contains("inserted"), "insert failed: {r}");
        let r = call(json!({"command": "view", "path": "/memories/notes.md"}));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert!(
            v["content"].as_str().unwrap().starts_with("     1\theader"),
            "insert at 0 must lead the file: {r}"
        );

        // rename
        let r = call(json!({"command": "rename", "old_path": "/memories/notes.md",
                            "new_path": "/memories/archive/notes.md"}));
        assert!(r.contains("renamed"), "rename failed: {r}");
        let r = call(json!({"command": "view", "path": "/memories"}));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["files"], json!(["archive/notes.md"]), "post-rename listing: {r}");

        // path traversal is rejected
        let r = call(json!({"command": "view", "path": "/memories/../etc/passwd"}));
        assert!(r.contains("invalid path") || r.contains("error"), "traversal must be rejected: {r}");

        // delete, then recreate: revival must restore searchability (the FTS
        // row is deleted by forget; the remember update path must re-insert it).
        let r = call(json!({"command": "delete", "path": "/memories/archive/notes.md"}));
        assert!(r.contains("deleted"), "delete failed: {r}");
        let r = call(json!({"command": "create", "path": "/memories/archive/notes.md",
                            "file_text": "reborn searchable zanzibar"}));
        assert!(r.contains("created"), "recreate failed: {r}");
        let hits = db
            .recall(&crate::models::RecallParams {
                query: "zanzibar".to_string(),
                skip_side_effects: true,
                ..crate::models::RecallParams::default()
            })
            .unwrap();
        assert!(
            hits.iter().any(|e| e.key == "archive/notes.md"),
            "revived file must be FTS-searchable again"
        );

        drop(db);
        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn unknown_tool_error_reports_original_unnormalized_name() {
        let db_path = std::env::temp_dir()
            .join(format!("mimir-unknown-tool-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");

        // A caller using either back-compat prefix should see ITS OWN name in
        // the error, not the "mimir_*" name it gets normalized to internally.
        let mneme_result = call_tool("mneme_bogus", &db, json!({}), None);
        assert!(mneme_result.contains("Unknown tool: mneme_bogus"), "got: {mneme_result}");
        assert!(!mneme_result.contains("mimir_bogus"), "got: {mneme_result}");

        let vault_result = call_tool("perseus_vault_bogus", &db, json!({}), None);
        assert!(
            vault_result.contains("Unknown tool: perseus_vault_bogus"),
            "got: {vault_result}"
        );
        assert!(!vault_result.contains("mimir_bogus"), "got: {vault_result}");

        let _ = fs::remove_file(&db_path);
    }

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
        let state = MCPState::new();

        let resp = handle_request(&req, &state, &db).expect("error response");
        assert_eq!(resp.error.expect("json-rpc error").code, -32600);
        assert!(!state.initialized.load(std::sync::atomic::Ordering::Relaxed));

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn initialize_reports_the_current_crate_name_not_a_hardcoded_one() {
        // Regression: serverInfo.name was a hardcoded "mimir" literal,
        // reporting stale branding through the Mimir -> Mneme -> Perseus
        // Vault renames. It must track Cargo.toml's package name instead.
        let db_path = std::env::temp_dir()
            .join(format!("mimir-initialize-name-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "initialize".to_string(),
            params: None,
        };
        let state = MCPState::new();

        let resp = handle_request(&req, &state, &db).expect("initialize response");
        let result = resp.result.expect("initialize result");
        assert_eq!(
            result["serverInfo"]["name"],
            json!(env!("CARGO_PKG_NAME")),
        );

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn recall_confidence_is_opt_in_and_normalized() {
        let db_path =
            std::env::temp_dir().join(format!("mimir-confidence-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");

        tools::handle_remember(
            &db,
            json!({"category": "demo", "key": "k1", "body_json": "{\"content\":\"alpha bravo\"}"}),
        )
        .expect("remember");

        // Default: confidence is absent (opt-in, non-breaking).
        let plain = tools::handle_recall(&db, json!({"query": "alpha"})).expect("recall");
        let plain_v: Value = serde_json::from_str(&plain).unwrap();
        assert!(
            plain_v["items"][0].get("confidence").is_none(),
            "confidence must be opt-in"
        );

        // Opt-in: confidence present and normalized to [0,1].
        let withc =
            tools::handle_recall(&db, json!({"query": "alpha", "include_confidence": true}))
                .expect("recall");
        let withc_v: Value = serde_json::from_str(&withc).unwrap();
        let c = withc_v["items"][0]["confidence"]
            .as_f64()
            .expect("confidence number");
        assert!((0.0..=1.0).contains(&c), "confidence {} out of range", c);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn history_tool_lists_superseded_versions() {
        let db_path =
            std::env::temp_dir().join(format!("mimir-history-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");

        tools::handle_remember(
            &db,
            json!({"category":"facts","key":"color","body_json":"{\"content\":\"blue\"}"}),
        )
        .expect("v1");
        // A content change snapshots the prior version into history.
        tools::handle_remember(
            &db,
            json!({"category":"facts","key":"color","body_json":"{\"content\":\"green\"}"}),
        )
        .expect("v2");

        let resp =
            tools::handle_history(&db, json!({"category":"facts","key":"color"})).expect("history");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["total"].as_i64().unwrap(), 1, "one superseded version: {}", resp);
        let body = v["versions"][0]["content"]
            .as_str()
            .or_else(|| v["versions"][0]["body_json"].as_str())
            .unwrap_or("");
        assert!(body.contains("blue"), "history should hold the old 'blue' value: {}", resp);

        // Unknown key -> empty trail.
        let empty =
            tools::handle_history(&db, json!({"category":"facts","key":"nope"})).expect("history");
        let ev: Value = serde_json::from_str(&empty).unwrap();
        assert_eq!(ev["total"].as_i64().unwrap(), 0);

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn recall_layer_filter_scopes_by_canonical_and_alias() {
        let db_path =
            std::env::temp_dir().join(format!("mimir-layerfilter-{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(db_path.to_str().expect("temp db path")).expect("open temp db");

        tools::handle_remember(
            &db,
            json!({"category":"demo","key":"a","body_json":"{\"content\":\"alpha core fact\"}","layer":"core"}),
        )
        .expect("remember a");
        tools::handle_remember(
            &db,
            json!({"category":"demo","key":"b","body_json":"{\"content\":\"alpha working fact\"}","layer":"working"}),
        )
        .expect("remember b");

        let keys = |resp: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(resp).unwrap();
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["key"].as_str().unwrap().to_string())
                .collect()
        };

        // Canonical "core" -> only entity a.
        let core =
            tools::handle_recall(&db, json!({"query":"alpha","layer":"core"})).expect("recall");
        let ck = keys(&core);
        assert!(
            ck.contains(&"a".to_string()) && !ck.contains(&"b".to_string()),
            "core filter returned {:?}",
            ck
        );

        // Alias "semantic" -> "working" -> only entity b.
        let sem =
            tools::handle_recall(&db, json!({"query":"alpha","layer":"semantic"})).expect("recall");
        let sk = keys(&sem);
        assert!(
            sk.contains(&"b".to_string()) && !sk.contains(&"a".to_string()),
            "semantic->working filter returned {:?}",
            sk
        );

        // No layer filter -> both.
        let all = tools::handle_recall(&db, json!({"query":"alpha"})).expect("recall");
        assert_eq!(keys(&all).len(), 2, "no filter should return both");

        let _ = fs::remove_file(db_path);
    }
}

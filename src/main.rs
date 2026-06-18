mod connectors;
mod db;
mod embedding;
mod encryption;
mod mcp;
mod models;
mod schema;
mod tools;
mod transport;
mod util;
mod web;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mimir")]
#[command(
    about = "Mimir — persistent memory for AI agents — MCP JSON-RPC stdio server",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// SQLite database path (used when no subcommand given)
    #[arg(long, default_value_t = default_db_path())]
    db: String,

    /// Path to AES-256-GCM encryption key file (base64-encoded, 32 bytes)
    #[arg(long)]
    encryption_key: Option<String>,

    /// Start the web dashboard HTTP server alongside the MCP stdio server
    #[arg(long)]
    web: bool,

    /// Web dashboard port (default: 8767)
    #[arg(long, default_value_t = 8767)]
    port: u16,

    /// Web dashboard bind address (default: 127.0.0.1 — use 0.0.0.0 to expose)
    #[arg(long, default_value_t = String::from("127.0.0.1"))]
    web_bind: String,

    /// Ollama API endpoint for the mimir_ask RAG tool
    #[arg(long)]
    llm_endpoint: Option<String>,

    /// API key for LLM endpoint (Bearer token — required for OpenAI, OpenRouter, etc.)
    #[arg(long)]
    llm_api_key: Option<String>,

    /// Separate embedding endpoint (OpenAI /v1/embeddings, Ollama /api/embed, etc.)
    /// If not set, defaults to Ollama /api/embed derived from llm_endpoint.
    #[arg(long)]
    embedding_endpoint: Option<String>,

    /// Path to ONNX embedding model (enables local embeddings, no Ollama required)
    #[arg(long)]
    embedding_model: Option<String>,

    /// Ollama model name (default: llama3)
    #[arg(long, default_value_t = String::from("llama3"))]
    llm_model: String,

    /// Path to connectors.yaml config file for external connectors
    #[arg(long)]
    connectors_config: Option<String>,

    /// Deprecated compatibility flag; MCP stdio mode is always enabled
    #[arg(long = "mcp", default_value_t = false, hide = true)]
    _mcp: bool,

    /// MCP transport mode: stdio (default), sse, or http
    #[arg(long, default_value_t = String::from("stdio"))]
    transport: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP JSON-RPC stdio server
    Serve {
        /// SQLite database path
        #[arg(long, default_value_t = default_db_path())]
        db: String,

        /// Path to AES-256-GCM encryption key file (base64-encoded, 32 bytes)
        #[arg(long)]
        encryption_key: Option<String>,

        /// Start the web dashboard HTTP server alongside the MCP stdio server
        #[arg(long)]
        web: bool,

        /// Web dashboard port (default: 8767)
        #[arg(long, default_value_t = 8767)]
        port: u16,

        /// Web dashboard bind address (default: 127.0.0.1 — use 0.0.0.0 to expose)
        #[arg(long, default_value_t = String::from("127.0.0.1"))]
        web_bind: String,

        /// Ollama API endpoint for the mimir_ask RAG tool
        #[arg(long)]
        llm_endpoint: Option<String>,

        /// API key for LLM endpoint (Bearer token — required for OpenAI, OpenRouter, etc.)
        #[arg(long)]
        llm_api_key: Option<String>,

        /// Separate embedding endpoint (OpenAI /v1/embeddings, Ollama /api/embed, etc.)
        /// If not set, defaults to Ollama /api/embed derived from llm_endpoint.
        #[arg(long)]
        embedding_endpoint: Option<String>,

        /// Path to ONNX embedding model (enables local embeddings, no Ollama required)
        #[arg(long)]
        embedding_model: Option<String>,

        /// Ollama model name (default: llama3)
        #[arg(long, default_value_t = String::from("llama3"))]
        llm_model: String,

        /// Path to connectors.yaml config file for external connectors
        #[arg(long)]
        connectors_config: Option<String>,

        /// Deprecated compatibility flag; MCP stdio mode is always enabled
        #[arg(long = "mcp", default_value_t = false, hide = true)]
        _mcp: bool,

        /// MCP transport mode: stdio (default), sse, or http
        #[arg(long, default_value_t = String::from("stdio"))]
        transport: String,
    },

    /// Migrate a v0.1.x Mimir database to v0.2.0 schema
    Migrate {
        /// Path to the source v0.1.x database
        #[arg(long)]
        from: String,

        /// Path to the target v0.2.0 database (creates if needed)
        #[arg(long)]
        to: String,
    },

    /// Generate a new AES-256-GCM encryption key and write it to a file
    Keygen {
        /// Path to write the key file (default: ~/.mimir/secret.key)
        #[arg(long, default_value_t = default_key_file())]
        key_file: String,
    },
}

fn default_db_path() -> String {
    std::env::var("MIMIR_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| {
                eprintln!("mimir: could not determine home directory. Set MIMIR_DB_PATH or HOME/USERPROFILE.");
                std::process::exit(1);
            });
        let dir = format!("{}/.mimir/data", home);
        let _ = std::fs::create_dir_all(&dir);
        format!("{}/mimir.db", dir)
    })
}

fn default_key_file() -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/root".to_string());
    format!("{}/.mimir/secret.key", home)
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Keygen { key_file }) => {
            let expanded = if key_file.starts_with("~/") {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| "/root".to_string());
                key_file.replacen("~", &home, 1)
            } else {
                key_file.clone()
            };

            // Create parent directory if needed
            if let Some(parent) = std::path::Path::new(&expanded).parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("mimir: failed to create directory {}: {}", parent.display(), e);
                    std::process::exit(1);
                }
            }

            let key = crate::encryption::EncryptionManager::generate_key();
            match std::fs::write(&expanded, &key) {
                Ok(_) => {
                    // Set restrictive permissions (owner read-only)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(&expanded, std::fs::Permissions::from_mode(0o600));
                    }
                    println!("Key written to {}", expanded);
                    println!("Use --encryption-key {} to enable encryption", expanded);
                }
                Err(e) => {
                    eprintln!("mimir: failed to write key file {}: {}", expanded, e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Migrate { from, to }) => {
            let target_db = match db::Database::open(&to) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("mimir: failed to open target database at {}: {}", to, e);
                    std::process::exit(1);
                }
            };

            match target_db.migrate_from_v0_1(&from) {
                Ok(report) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_else(|_| {
                            "Migration complete (report serialization failed)".to_string()
                        })
                    );
                }
                Err(e) => {
                    eprintln!("mimir: migration failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Serve { ref db, ref encryption_key, ref web, ref port, ref web_bind, ref llm_endpoint, ref llm_api_key, ref embedding_endpoint, ref llm_model, ref embedding_model: _, ref connectors_config, ref transport, .. }) => {
            let db_path = db.clone();
            let mut database = match db::Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("mimir: failed to open database at {}: {}", db_path, e);
                    std::process::exit(1);
                }
            };
            if let Some(ref key_file) = encryption_key {
                if let Err(e) = database.set_encryption(key_file) {
                    eprintln!("mimir: encryption setup failed: {}", e);
                    std::process::exit(1);
                }
                eprintln!("mimir: encryption enabled (key: {})", key_file);
            }

            // Configure LLM for mimir_ask if endpoint is provided
            if let Some(ref endpoint) = llm_endpoint {
                database.set_llm(true, endpoint, llm_model, llm_api_key.as_deref(), embedding_endpoint.as_deref());
                eprintln!("mimir: LLM enabled (endpoint: {}, model: {})", endpoint, llm_model);
            }

            // Load connectors from YAML config if provided
            if let Some(ref config_path) = connectors_config {
                match load_connectors(config_path) {
                    Ok(connectors) => {
                        let count = connectors.len();
                        database.set_connectors(connectors);
                        eprintln!("mimir: loaded {} connector(s) from {}", count, config_path);
                    }
                    Err(e) => {
                        eprintln!("mimir: fatal — failed to load connectors: {}", e);
                        std::process::exit(1);
                    }
                }
            }

            // Start web dashboard in background if requested
            if *web {
                let web_port = *port;
                let web_bind_addr = web_bind.clone();
                let web_key = encryption_key.clone();
                let mut web_db = match db::Database::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        eprintln!("mimir: failed to open web database: {}", e);
                        std::process::exit(1);
                    }
                };
                // Propagate encryption key to web dashboard DB
                if let Some(ref key_file) = web_key {
                    if let Err(e) = web_db.set_encryption(key_file) {
                        eprintln!("mimir: web dashboard encryption setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
                let web_db = std::sync::Arc::new(std::sync::Mutex::new(web_db));
                let router = crate::web::build_router(web_db);
                let addr = format!("{}:{}", web_bind_addr, web_port);
                eprintln!("mimir: web dashboard starting on http://{}", addr);

                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(rt) => rt,
                        Err(e) => {
                            eprintln!("mimir: web dashboard runtime error: {}", e);
                            return;
                        }
                    };
                    rt.block_on(async {
                        let listener = match tokio::net::TcpListener::bind(&addr).await {
                            Ok(l) => l,
                            Err(e) => {
                                eprintln!("mimir: web dashboard bind error: {}", e);
                                return;
                            }
                        };
                        if let Err(e) = axum::serve(listener, router).await {
                            eprintln!("mimir: web dashboard error: {}", e);
                        }
                    });
                });
            }

            // Determine transport mode
            let tmode = match transport.as_str() {
                "sse" => Some(crate::transport::TransportMode::Sse),
                "http" => Some(crate::transport::TransportMode::Http),
                _ => None,
            };

            if let Some(mode) = tmode {
                let transport_db = std::sync::Arc::new(std::sync::Mutex::new(database));
                crate::transport::init_transport_state(transport_db);
                let transport_router = crate::transport::build_transport_router(mode);
                let transport_addr = format!("{}:{}", web_bind, *port);
                let mode_label = match mode {
                    transport::TransportMode::Sse => "sse",
                    transport::TransportMode::Http => "http",
                };
                eprintln!("mimir: MCP over {} transport on http://{}", mode_label, transport_addr);
                eprintln!("mimir: POST http://{}/message", transport_addr);
                if mode == transport::TransportMode::Sse {
                    eprintln!("mimir: GET  http://{}/sse", transport_addr);
                }
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let listener = tokio::net::TcpListener::bind(&transport_addr).await.unwrap();
                    axum::serve(listener, transport_router).await.unwrap();
                });
            } else {
                mcp::run_server(database);
            }
        }
        None => {
            let db_path = cli.db.clone();
            let mut database = match db::Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("mimir: failed to open database at {}: {}", db_path, e);
                    std::process::exit(1);
                }
            };
            if let Some(ref key_file) = cli.encryption_key {
                if let Err(e) = database.set_encryption(key_file) {
                    eprintln!("mimir: encryption setup failed: {}", e);
                    std::process::exit(1);
                }
                eprintln!("mimir: encryption enabled (key: {})", key_file);
            }

            if let Some(ref endpoint) = cli.llm_endpoint {
                database.set_llm(true, endpoint, &cli.llm_model, cli.llm_api_key.as_deref(), cli.embedding_endpoint.as_deref());
                eprintln!("mimir: LLM enabled (endpoint: {}, model: {})", endpoint, cli.llm_model);
            }

            if let Some(ref config_path) = cli.connectors_config {
                match load_connectors(config_path) {
                    Ok(connectors) => {
                        let count = connectors.len();
                        database.set_connectors(connectors);
                        eprintln!("mimir: loaded {} connector(s) from {}", count, config_path);
                    }
                    Err(e) => {
                        eprintln!("mimir: fatal — failed to load connectors: {}", e);
                        std::process::exit(1);
                    }
                }
            }

            if cli.web {
                let web_port = cli.port;
                let web_bind_addr = cli.web_bind.clone();
                let web_key = cli.encryption_key.clone();
                let mut web_db = match db::Database::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        eprintln!("mimir: failed to open web database: {}", e);
                        std::process::exit(1);
                    }
                };
                if let Some(ref key_file) = web_key {
                    if let Err(e) = web_db.set_encryption(key_file) {
                        eprintln!("mimir: web dashboard encryption setup failed: {}", e);
                        std::process::exit(1);
                    }
                }
                let web_db = std::sync::Arc::new(std::sync::Mutex::new(web_db));
                let router = crate::web::build_router(web_db);
                let addr = format!("{}:{}", web_bind_addr, web_port);
                eprintln!("mimir: web dashboard starting on http://{}", addr);

                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Runtime::new() {
                        Ok(rt) => rt,
                        Err(e) => {
                            eprintln!("mimir: web dashboard runtime error: {}", e);
                            return;
                        }
                    };
                    rt.block_on(async {
                        let listener = match tokio::net::TcpListener::bind(&addr).await {
                            Ok(l) => l,
                            Err(e) => {
                                eprintln!("mimir: web dashboard bind error: {}", e);
                                return;
                            }
                        };
                        if let Err(e) = axum::serve(listener, router).await {
                            eprintln!("mimir: web dashboard error: {}", e);
                        }
                    });
                });
            }

            // Determine transport mode
            let transport_mode = match cli.transport.as_str() {
                "sse" => Some(transport::TransportMode::Sse),
                "http" => Some(transport::TransportMode::Http),
                _ => None,
            };

            if let Some(mode) = transport_mode {
                let transport_db = std::sync::Arc::new(std::sync::Mutex::new(database));
                crate::transport::init_transport_state(transport_db);
                let transport_router = crate::transport::build_transport_router(mode);
                let transport_addr = format!("{}:{}", cli.web_bind, cli.port);
                let mode_label = match mode {
                    transport::TransportMode::Sse => "sse",
                    transport::TransportMode::Http => "http",
                };
                eprintln!("mimir: MCP over {} transport on http://{}", mode_label, transport_addr);
                eprintln!("mimir: POST http://{}/message", transport_addr);
                if mode == transport::TransportMode::Sse {
                    eprintln!("mimir: GET  http://{}/sse", transport_addr);
                }
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let listener = tokio::net::TcpListener::bind(&transport_addr).await.unwrap();
                    axum::serve(listener, transport_router).await.unwrap();
                });
            } else {
                mcp::run_server(database);
            }
        }
    }
}

fn load_connectors(path: &str) -> Result<Vec<Box<dyn crate::connectors::Connector>>, String> {
    let expanded = if path.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/root".to_string());
        path.replacen("~", &home, 1)
    } else {
        path.to_string()
    };
    let contents = std::fs::read_to_string(&expanded)
        .map_err(|e| format!("Cannot read connectors config {}: {}", expanded, e))?;
    let config: serde_yaml::Value = serde_yaml::from_str(&contents)
        .map_err(|e| format!("Invalid YAML in {}: {}", expanded, e))?;

    let mut connectors: Vec<Box<dyn crate::connectors::Connector>> = Vec::new();

    // Load GitHub connector if configured
    if let Some(github) = config.get("connectors").and_then(|c| c.get("github")) {
        let enabled = github.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        if enabled {
            let token = github.get("token").and_then(|v| v.as_str()).unwrap_or("");
            let repos: Vec<String> = github
                .get("repos")
                .and_then(|v| v.as_sequence())
                .map(|s| s.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let days_past = github.get("days_past").and_then(|v| v.as_u64()).unwrap_or(90) as u32;
            let max_items = github.get("max_items_per_repo").and_then(|v| v.as_u64()).unwrap_or(500) as usize;

            let gcfg = crate::connectors::github::GitHubConnectorConfig {
                enabled: true,
                token: token.to_string(),
                repos,
                days_past,
                max_items_per_repo: max_items,
            };
            connectors.push(Box::new(crate::connectors::github::GitHubConnector::new(gcfg)));
        }
    }

    // Load file watcher connector if configured
    if let Some(fw) = config.get("connectors").and_then(|c| c.get("file_watcher")) {
        let enabled = fw.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        if enabled {
            let paths: Vec<String> = fw
                .get("paths")
                .and_then(|v| v.as_sequence())
                .map(|s| s.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let extensions: Vec<String> = fw
                .get("extensions")
                .and_then(|v| v.as_sequence())
                .map(|s| s.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_else(|| vec![".md".to_string(), ".txt".to_string()]);
            let debounce_ms = fw.get("debounce_ms").and_then(|v| v.as_u64()).unwrap_or(1500);

            let fcfg = crate::connectors::file_watcher::FileWatcherConfig {
                enabled: true,
                paths,
                extensions,
                debounce_ms,
            };
            connectors.push(Box::new(crate::connectors::file_watcher::FileWatcher::new(fcfg)));
        }
    }

    Ok(connectors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_direct_server_with_db() {
        let cli = Cli::parse_from(["mimir", "--db", "/tmp/mimir-direct.db"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.db, "/tmp/mimir-direct.db");
    }

    #[test]
    fn parses_migrate_subcommand() {
        let cli = Cli::parse_from([
            "mimir",
            "migrate",
            "--from",
            "/tmp/old.db",
            "--to",
            "/tmp/new.db",
        ]);
        match cli.command {
            Some(Commands::Migrate { from, to }) => {
                assert_eq!(from, "/tmp/old.db");
                assert_eq!(to, "/tmp/new.db");
            }
            _ => panic!("expected migrate subcommand"),
        }
    }
}

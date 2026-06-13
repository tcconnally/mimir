mod db;
mod encryption;
mod mcp;
mod models;
mod schema;
mod tools;
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

    /// Deprecated compatibility flag; MCP stdio mode is always enabled
    #[arg(long = "mcp", default_value_t = false, hide = true)]
    _mcp: bool,
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

        /// Deprecated compatibility flag; MCP stdio mode is always enabled
        #[arg(long = "mcp", default_value_t = false, hide = true)]
        _mcp: bool,
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
        Some(Commands::Serve { ref db, ref encryption_key, ref web, ref port, .. }) => {
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

            // Start web dashboard in background if requested
            if *web {
                let web_port = *port;
                let web_db = match db::Database::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        eprintln!("mimir: failed to open web database: {}", e);
                        std::process::exit(1);
                    }
                };
                let web_db = std::sync::Arc::new(std::sync::Mutex::new(web_db));
                let router = crate::web::build_router(web_db);
                let addr = format!("0.0.0.0:{}", web_port);
                eprintln!("mimir: web dashboard starting on http://{}", addr);

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
                        axum::serve(listener, router).await.unwrap();
                    });
                });
            }

            mcp::run_server(database);
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

            if cli.web {
                let web_port = cli.port;
                let web_db = match db::Database::open(&db_path) {
                    Ok(db) => db,
                    Err(e) => {
                        eprintln!("mimir: failed to open web database: {}", e);
                        std::process::exit(1);
                    }
                };
                let web_db = std::sync::Arc::new(std::sync::Mutex::new(web_db));
                let router = crate::web::build_router(web_db);
                let addr = format!("0.0.0.0:{}", web_port);
                eprintln!("mimir: web dashboard starting on http://{}", addr);

                std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().unwrap();
                    rt.block_on(async {
                        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
                        axum::serve(listener, router).await.unwrap();
                    });
                });
            }

            mcp::run_server(database);
        }
    }
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

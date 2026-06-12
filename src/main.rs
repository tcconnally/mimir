mod db;
mod mcp;
mod models;
mod schema;
mod tools;

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
}

fn default_db_path() -> String {
    std::env::var("MIMIR_DB_PATH").unwrap_or_else(|_| {
        // M-4: use platform-appropriate home directory.
        // On Windows, HOME is typically unset; fall back to USERPROFILE.
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

fn main() {
    let cli = Cli::parse();

    match cli.command {
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
        Some(Commands::Serve { ref db, .. }) => {
            let db_path = db.clone();
            let database = match db::Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("mimir: failed to open database at {}: {}", db_path, e);
                    std::process::exit(1);
                }
            };
            mcp::run_server(database);
        }
        None => {
            let db_path = cli.db.clone();
            let database = match db::Database::open(&db_path) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("mimir: failed to open database at {}: {}", db_path, e);
                    std::process::exit(1);
                }
            };
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

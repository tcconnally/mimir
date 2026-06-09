mod db;
mod mcp;
mod tools;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mneme")]
#[command(
    about = "Persistent memory engine for AI agents — MCP JSON-RPC stdio server",
    version = "0.1.0"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// SQLite database path (used when no subcommand given)
    #[arg(long, default_value_t = default_db_path())]
    db: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP JSON-RPC stdio server
    Serve {
        /// SQLite database path
        #[arg(long, default_value_t = default_db_path())]
        db: String,

        /// MCP mode (for compatibility — always on)
        #[arg(long, default_value_t = false)]
        mcp: bool,
    },
}

fn default_db_path() -> String {
    std::env::var("MNEME_DB_PATH").unwrap_or_else(|_| "mneme.db".to_string())
}

fn main() {
    let cli = Cli::parse();

    // Determine db path based on subcommand or top-level flag
    let db_path = match &cli.command {
        Some(Commands::Serve { db, .. }) => db.clone(),
        None => cli.db.clone(),
    };

    let database = match db::Database::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("mneme: failed to open database at {}: {}", db_path, e);
            std::process::exit(1);
        }
    };

    mcp::run_server(database);
}

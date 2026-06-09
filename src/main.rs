mod db;
mod mcp;
mod tools;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mneme")]
#[command(
    about = "Persistent memory engine for AI agents — MCP JSON-RPC stdio server",
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_direct_server_with_db() {
        let cli = Cli::parse_from(["mneme", "--db", "/tmp/mneme-direct.db"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.db, "/tmp/mneme-direct.db");
    }

    #[test]
    fn parses_direct_server_with_deprecated_mcp_flag() {
        let cli = Cli::parse_from(["mneme", "--db", "/tmp/mneme-direct.db", "--mcp"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.db, "/tmp/mneme-direct.db");
    }

    #[test]
    fn parses_serve_subcommand_with_deprecated_mcp_flag() {
        let cli = Cli::parse_from(["mneme", "serve", "--db", "/tmp/mneme-serve.db", "--mcp"]);
        match cli.command {
            Some(Commands::Serve { db, .. }) => assert_eq!(db, "/tmp/mneme-serve.db"),
            None => panic!("expected serve subcommand"),
        }
    }
}

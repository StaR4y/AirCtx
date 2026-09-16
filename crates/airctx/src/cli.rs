use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "airctx", version, about = "Share agent context across devices")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Open the interactive UI (default)
    Ui,

    /// Start the server
    Serve {
        /// Listen address, e.g. 0.0.0.0:8787 (optional on first run)
        #[arg(long)]
        addr: Option<String>,
        /// Security path (the secret URL segment). Random if omitted on first run.
        #[arg(long)]
        gate: Option<String>,
        /// Data directory (holds airctx.db and server.toml)
        #[arg(long, default_value = "./data")]
        data_dir: PathBuf,
    },

    /// Ping a server health endpoint (no token required)
    Ping {
        /// Server base URL including the security path
        url: Option<String>,
        /// Load gate from this data dir (Docker HEALTHCHECK)
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },

    /// Save server address and token
    Login { url: String, token: String },

    /// Write a context entry ('-' or omitted reads stdin)
    Set {
        key: String,
        /// Value; '-' reads from stdin
        value: Option<String>,
        /// TTL, e.g. 90s / 10m / 2h / 3600
        #[arg(short, long)]
        ttl: Option<String>,
        #[arg(long = "content-type", default_value = "text/plain")]
        content_type: String,
    },

    /// Read a context entry
    Get {
        key: String,
        /// Print version/content-type to stderr
        #[arg(short, long)]
        meta: bool,
    },

    /// List keys
    Ls {
        prefix: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(short, long, default_value_t = 1000)]
        limit: u32,
    },

    /// Delete a context entry
    Rm { key: String },

    /// Watch changes in real time (Ctrl-C to exit)
    Watch {
        prefix: Option<String>,
        /// Replay changes since the given version
        #[arg(long)]
        since_version: Option<i64>,
    },

    /// Control the background sync daemon
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Subcommand)]
pub enum DaemonCmd {
    /// Run daemon in foreground (debug / autostart entry)
    Run,
    /// Start daemon in background
    Start,
    /// Stop daemon
    Stop,
    /// Show status (JSON)
    Status,
    /// Register autostart and start now
    Install,
    /// Remove autostart and stop
    Uninstall,
    /// Trigger an immediate sync
    Sync,
}
